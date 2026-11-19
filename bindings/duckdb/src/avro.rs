//! Minimal Avro object container files, enough for Iceberg manifests.
//!
//! Writing uses a schema JSON string verbatim, so Iceberg attributes such as
//! `field-id` and `logicalType: map` survive exactly. Reading is schema-driven and
//! supports the `null` and `deflate` codecs (pyiceberg and Java write deflate).

use serde_json::Value as Json;
use std::error::Error;

type E = Box<dyn Error>;

/// A value to encode, in schema field order.
#[derive(Debug, Clone)]
pub enum V {
    Null,
    Bool(bool),
    Int(i32),
    Long(i64),
    Bytes(Vec<u8>),
    Str(String),
    /// Union branch index and value.
    U(usize, Box<V>),
    Rec(Vec<V>),
    Arr(Vec<V>),
}

fn zigzag(out: &mut Vec<u8>, v: i64) {
    let mut n = ((v << 1) ^ (v >> 63)) as u64;
    loop {
        let b = (n & 0x7f) as u8;
        n >>= 7;
        if n == 0 {
            out.push(b);
            break;
        }
        out.push(b | 0x80);
    }
}

fn encode(out: &mut Vec<u8>, v: &V) {
    match v {
        V::Null => {}
        V::Bool(b) => out.push(*b as u8),
        V::Int(i) => zigzag(out, *i as i64),
        V::Long(i) => zigzag(out, *i),
        V::Bytes(b) => {
            zigzag(out, b.len() as i64);
            out.extend_from_slice(b);
        }
        V::Str(s) => {
            zigzag(out, s.len() as i64);
            out.extend_from_slice(s.as_bytes());
        }
        V::U(i, inner) => {
            zigzag(out, *i as i64);
            encode(out, inner);
        }
        V::Rec(fields) => fields.iter().for_each(|f| encode(out, f)),
        V::Arr(items) => {
            if !items.is_empty() {
                zigzag(out, items.len() as i64);
                items.iter().for_each(|i| encode(out, i));
            }
            zigzag(out, 0);
        }
    }
}

/// Encode a container file with codec `deflate` (raw RFC 1951, as Java and pyiceberg write).
pub fn write_container(
    schema_json: &str,
    meta: &[(&str, String)],
    records: &[V],
    sync: [u8; 16],
) -> Vec<u8> {
    let mut out = b"Obj\x01".to_vec();
    let mut entries: Vec<(String, Vec<u8>)> = vec![
        ("avro.schema".into(), schema_json.as_bytes().to_vec()),
        ("avro.codec".into(), b"deflate".to_vec()),
    ];
    entries.extend(
        meta.iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec())),
    );
    zigzag(&mut out, entries.len() as i64);
    for (k, v) in &entries {
        encode(&mut out, &V::Str(k.clone()));
        encode(&mut out, &V::Bytes(v.clone()));
    }
    zigzag(&mut out, 0);
    out.extend_from_slice(&sync);
    if !records.is_empty() {
        let mut block = Vec::new();
        records.iter().for_each(|r| encode(&mut block, r));
        let block = miniz_oxide::deflate::compress_to_vec(&block, 6);
        zigzag(&mut out, records.len() as i64);
        zigzag(&mut out, block.len() as i64);
        out.extend_from_slice(&block);
        out.extend_from_slice(&sync);
    }
    out
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// A decoded value with field names kept.
#[derive(Debug, Clone)]
#[allow(dead_code)] // every Avro type is decoded; callers read only what they need
pub enum D {
    Null,
    Bool(bool),
    Long(i64),
    Double(f64),
    Bytes(Vec<u8>),
    Str(String),
    Arr(Vec<D>),
    Map(Vec<(String, D)>),
    Rec(Vec<(String, D)>),
}

impl D {
    pub fn get(&self, name: &str) -> Option<&D> {
        match self {
            D::Rec(f) => f.iter().find(|(n, _)| n == name).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn long(&self) -> Option<i64> {
        match self {
            D::Long(v) => Some(*v),
            _ => None,
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, E> {
        let v = *self.b.get(self.i).ok_or("avro: unexpected end of data")?;
        self.i += 1;
        Ok(v)
    }
    fn long(&mut self) -> Result<i64, E> {
        let (mut n, mut shift) = (0u64, 0);
        loop {
            let b = self.byte()?;
            n |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                break;
            }
            shift += 7;
            if shift > 63 {
                return Err("avro: varint too long".into());
            }
        }
        Ok(((n >> 1) as i64) ^ -((n & 1) as i64))
    }
    fn take(&mut self, n: usize) -> Result<&[u8], E> {
        let s = self
            .b
            .get(self.i..self.i + n)
            .ok_or("avro: unexpected end of data")?;
        self.i += n;
        Ok(s)
    }
    fn bytes(&mut self) -> Result<Vec<u8>, E> {
        let n = self.long()? as usize;
        Ok(self.take(n)?.to_vec())
    }
}

fn decode(r: &mut Reader, schema: &Json, named: &mut Vec<(String, Json)>) -> Result<D, E> {
    match schema {
        Json::String(t) => match t.as_str() {
            "null" => Ok(D::Null),
            "boolean" => Ok(D::Bool(r.byte()? != 0)),
            "int" | "long" => Ok(D::Long(r.long()?)),
            "float" => Ok(D::Double(f32::from_le_bytes(r.take(4)?.try_into()?) as f64)),
            "double" => Ok(D::Double(f64::from_le_bytes(r.take(8)?.try_into()?))),
            "bytes" => Ok(D::Bytes(r.bytes()?)),
            "string" => Ok(D::Str(String::from_utf8(r.bytes()?)?)),
            other => {
                let s = named
                    .iter()
                    .find(|(n, _)| n == other || n.ends_with(&format!(".{other}")))
                    .map(|(_, s)| s.clone());
                match s {
                    Some(s) => decode(r, &s, named),
                    None => Err(format!("avro: unknown type {other}").into()),
                }
            }
        },
        Json::Array(branches) => {
            let i = r.long()? as usize;
            decode(r, branches.get(i).ok_or("avro: bad union index")?, named)
        }
        Json::Object(o) => {
            let t = o.get("type").and_then(Json::as_str).unwrap_or("");
            if let Some(n) = o.get("name").and_then(Json::as_str) {
                if matches!(t, "record" | "enum" | "fixed") && !named.iter().any(|(k, _)| k == n) {
                    named.push((n.to_string(), schema.clone()));
                }
            }
            match t {
                "record" => {
                    let mut out = Vec::new();
                    for f in o
                        .get("fields")
                        .and_then(Json::as_array)
                        .ok_or("avro: record without fields")?
                    {
                        let name = f
                            .get("name")
                            .and_then(Json::as_str)
                            .unwrap_or("")
                            .to_string();
                        out.push((
                            name,
                            decode(r, f.get("type").ok_or("avro: field without type")?, named)?,
                        ));
                    }
                    Ok(D::Rec(out))
                }
                "array" => {
                    let items = o.get("items").ok_or("avro: array without items")?.clone();
                    let mut out = Vec::new();
                    loop {
                        let mut n = r.long()?;
                        if n == 0 {
                            break;
                        }
                        if n < 0 {
                            n = -n;
                            r.long()?;
                        }
                        for _ in 0..n {
                            out.push(decode(r, &items, named)?);
                        }
                    }
                    Ok(D::Arr(out))
                }
                "map" => {
                    let values = o.get("values").ok_or("avro: map without values")?.clone();
                    let mut out = Vec::new();
                    loop {
                        let mut n = r.long()?;
                        if n == 0 {
                            break;
                        }
                        if n < 0 {
                            n = -n;
                            r.long()?;
                        }
                        for _ in 0..n {
                            let k = String::from_utf8(r.bytes()?)?;
                            out.push((k, decode(r, &values, named)?));
                        }
                    }
                    Ok(D::Map(out))
                }
                "enum" => Ok(D::Long(r.long()?)),
                "fixed" => {
                    let n = o
                        .get("size")
                        .and_then(Json::as_u64)
                        .ok_or("avro: fixed without size")? as usize;
                    Ok(D::Bytes(r.take(n)?.to_vec()))
                }
                _ => decode(r, &Json::String(t.to_string()), named),
            }
        }
        _ => Err("avro: bad schema".into()),
    }
}

/// Avro file metadata entries.
pub type Meta = Vec<(String, Vec<u8>)>;

/// Decode every record of a container file. Returns (file metadata, records).
pub fn read_container(data: &[u8]) -> Result<(Meta, Vec<D>), E> {
    if data.get(0..4) != Some(b"Obj\x01") {
        return Err("not an Avro container file".into());
    }
    let mut r = Reader { b: data, i: 4 };
    let mut meta = Vec::new();
    loop {
        let mut n = r.long()?;
        if n == 0 {
            break;
        }
        if n < 0 {
            n = -n;
            r.long()?;
        }
        for _ in 0..n {
            let k = String::from_utf8(r.bytes()?)?;
            meta.push((k, r.bytes()?));
        }
    }
    let sync = r.take(16)?.to_vec();
    let get = |k: &str| meta.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let schema: Json = serde_json::from_slice(&get("avro.schema").ok_or("avro: no schema")?)?;
    let codec = get("avro.codec")
        .map(|v| String::from_utf8_lossy(&v).into_owned())
        .unwrap_or_else(|| "null".into());
    let mut records = Vec::new();
    while r.i < data.len() {
        let count = r.long()?;
        let size = r.long()? as usize;
        let raw = r.take(size)?.to_vec();
        if r.take(16)? != sync.as_slice() {
            return Err("avro: sync marker mismatch".into());
        }
        let block = match codec.as_str() {
            "null" => raw,
            "deflate" => miniz_oxide::inflate::decompress_to_vec(&raw)
                .map_err(|e| format!("avro deflate: {e:?}"))?,
            other => return Err(format!("avro codec {other} is not supported").into()),
        };
        let mut br = Reader { b: &block, i: 0 };
        let mut named = Vec::new();
        for _ in 0..count {
            records.push(decode(&mut br, &schema, &mut named)?);
        }
    }
    Ok((meta, records))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let schema = r#"{"type":"record","name":"r","fields":[{"name":"a","type":"long","field-id":1},{"name":"b","type":["null","string"]},{"name":"c","type":{"type":"array","items":"int"}}]}"#;
        let recs = vec![V::Rec(vec![
            V::Long(-5),
            V::U(1, Box::new(V::Str("x".into()))),
            V::Arr(vec![V::Int(1), V::Int(300)]),
        ])];
        let bytes = write_container(schema, &[("k", "v".into())], &recs, [7; 16]);
        let (meta, out) = read_container(&bytes).unwrap();
        assert!(meta.iter().any(|(k, v)| k == "k" && v == b"v"));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].get("a").and_then(D::long), Some(-5));
        match out[0].get("c") {
            Some(D::Arr(v)) => assert_eq!(v.len(), 2),
            _ => panic!(),
        }
    }
}
