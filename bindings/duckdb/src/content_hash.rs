//! Content hash of a data file (docs/dataset-metadata.md, section 3.1, frozen 2026-11-23).
//!
//! Each row becomes a JSON array: `gid` first, then the value fields in manifest order. Each
//! value is encoded by its declared `parquet_type`, not by how the file stores it: an integer
//! in a `double` field is written `3.0`. The encoded rows are sorted by bytes, and one SHA-256
//! covers them, each row followed by a line feed. The Python library and the vectors in
//! `vectors/spec-2.0/content-hash.json` use the same rules.

use std::error::Error;
use std::ffi::{c_void, CStr, CString};

use duckdb::ffi::*;
use sha2::{Digest, Sha256};

use crate::sql::{self, Lease};

type E = Box<dyn Error>;

/// One value field of the manifest: its name and declared `parquet_type`.
#[derive(Debug, Clone, PartialEq)]
pub struct ValueField {
    pub name: String,
    pub parquet_type: String,
}

/// The family a declared `parquet_type` belongs to.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Declared {
    Float,
    Integer,
    Text,
}

/// The family of a stored column.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Stored {
    Float,
    Integer,
    Text,
}

/// A value read from a data file.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Int(i128),
    Float(f64),
    Text(String),
}

fn declared_family(parquet_type: &str) -> Result<Declared, E> {
    Ok(match parquet_type {
        "double" | "float" => Declared::Float,
        "int8" | "int16" | "int32" | "int64" | "uint8" | "uint16" | "uint32" | "uint64" => {
            Declared::Integer
        }
        "string" => Declared::Text,
        other => {
            return Err(format!("parquet_type {other:?} is not supported by content_sha256").into())
        }
    })
}

fn stored_family(column_type: duckdb_type) -> Result<Stored, E> {
    Ok(match column_type {
        DUCKDB_TYPE_DUCKDB_TYPE_FLOAT | DUCKDB_TYPE_DUCKDB_TYPE_DOUBLE => Stored::Float,
        DUCKDB_TYPE_DUCKDB_TYPE_TINYINT
        | DUCKDB_TYPE_DUCKDB_TYPE_SMALLINT
        | DUCKDB_TYPE_DUCKDB_TYPE_INTEGER
        | DUCKDB_TYPE_DUCKDB_TYPE_BIGINT
        | DUCKDB_TYPE_DUCKDB_TYPE_UTINYINT
        | DUCKDB_TYPE_DUCKDB_TYPE_USMALLINT
        | DUCKDB_TYPE_DUCKDB_TYPE_UINTEGER
        | DUCKDB_TYPE_DUCKDB_TYPE_UBIGINT
        | DUCKDB_TYPE_DUCKDB_TYPE_HUGEINT => Stored::Integer,
        DUCKDB_TYPE_DUCKDB_TYPE_VARCHAR => Stored::Text,
        other => {
            return Err(
                format!("stored column type {other} is not supported by content_sha256").into(),
            )
        }
    })
}

/// Whether a stored value can be written as the declared type. A double field takes integers
/// too, and writes them with a point (float(3) is 3.0).
fn compatible(declared: Declared, stored: Stored) -> bool {
    matches!(
        (declared, stored),
        (Declared::Float, Stored::Float | Stored::Integer)
            | (Declared::Integer, Stored::Integer)
            | (Declared::Text, Stored::Text)
    )
}

/// Python's `float.__repr__` for a finite double: the shortest digits that round-trip, fixed
/// notation for decimal exponents -4 to 15, otherwise `d.ddde+XX` with at least two exponent
/// digits. A negative zero keeps its sign.
pub fn python_float_repr(x: f64) -> Result<String, E> {
    if !x.is_finite() {
        return Err("NaN and infinity cannot be hashed".into());
    }
    // `{:e}` gives the shortest round-trip digits, for example "1.5e-5", "1e16" or "-0e0".
    let scientific = format!("{x:e}");
    let (sign, body) = match scientific.strip_prefix('-') {
        Some(rest) => ("-", rest),
        None => ("", scientific.as_str()),
    };
    let (mantissa, exponent) = body.split_once('e').ok_or("unexpected float format")?;
    let exponent: i32 = exponent.parse()?;
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let count = digits.len() as i32;
    if (-4..16).contains(&exponent) {
        if exponent >= 0 {
            let integer_len = exponent + 1;
            if count <= integer_len {
                let zeros = "0".repeat((integer_len - count) as usize);
                Ok(format!("{sign}{digits}{zeros}.0"))
            } else {
                let (whole, fraction) = digits.split_at(integer_len as usize);
                Ok(format!("{sign}{whole}.{fraction}"))
            }
        } else {
            let zeros = "0".repeat((-exponent - 1) as usize);
            Ok(format!("{sign}0.{zeros}{digits}"))
        }
    } else {
        let mantissa = if count > 1 {
            format!("{}.{}", &digits[..1], &digits[1..])
        } else {
            digits.clone()
        };
        let exponent_sign = if exponent < 0 { '-' } else { '+' };
        Ok(format!(
            "{sign}{mantissa}e{exponent_sign}{:02}",
            exponent.abs()
        ))
    }
}

/// A JSON string as Python's `json.dumps(s, ensure_ascii=False)` writes it.
fn json_string(text: &str, out: &mut String) {
    out.push('"');
    for ch in text.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// One value as its canonical text, for the declared parquet_type.
pub fn encode_value(value: &Value, parquet_type: &str, field: &str) -> Result<String, E> {
    let declared = declared_family(parquet_type)?;
    Ok(match (value, declared) {
        (Value::Null, _) => "null".into(),
        (Value::Float(x), Declared::Float) => python_float_repr(*x)?,
        (Value::Int(i), Declared::Float) => python_float_repr(*i as f64)?,
        (Value::Int(i), Declared::Integer) => i.to_string(),
        (Value::Text(s), Declared::Text) => {
            let mut out = String::new();
            json_string(s, &mut out);
            out
        }
        _ => {
            return Err(
                format!("column {field}: the value cannot be written as {parquet_type}").into(),
            )
        }
    })
}

/// One row as its canonical text: a JSON array with no spaces. `values` are in order, gid first.
pub fn encode_row(values: &[Value], types: &[&str], names: &[&str]) -> Result<String, E> {
    let mut parts = Vec::with_capacity(values.len());
    for ((value, kind), name) in values.iter().zip(types).zip(names) {
        parts.push(encode_value(value, kind, name)?);
    }
    Ok(format!("[{}]", parts.join(",")))
}

/// The digest of encoded rows: sorted by bytes, each followed by a line feed, one SHA-256.
pub fn digest(mut lines: Vec<String>) -> String {
    lines.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    let mut hasher = Sha256::new();
    for line in &lines {
        hasher.update(line.as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Row count and content hash of one data file.
#[derive(Debug, Clone, PartialEq)]
pub struct FileHash {
    pub rows: i64,
    pub sha256: String,
}

/// Hash the rows of a Parquet file. `gid` is the first column; `fields` follow in order.
pub fn file_content_hash(lease: &Lease, path: &str, fields: &[ValueField]) -> Result<FileHash, E> {
    let columns = std::iter::once("gid")
        .chain(fields.iter().map(|f| f.name.as_str()))
        .map(sql::ident)
        .collect::<Vec<_>>()
        .join(", ");
    let query = format!("SELECT {columns} FROM read_parquet({})", sql::lit(path));
    query_content_hash(lease, &query, fields)
}

/// Whether a declared `parquet_type` can be hashed. Other types are refused, not guessed.
pub fn check_parquet_type(parquet_type: &str) -> Result<(), E> {
    declared_family(parquet_type).map(|_| ())
}

/// Hash the rows a query returns, as a data file with these value fields would store them. The
/// query lists `gid` first, then the value fields in the same order.
pub fn query_content_hash(
    lease: &Lease,
    query: &str,
    fields: &[ValueField],
) -> Result<FileHash, E> {
    let names: Vec<&str> = std::iter::once("gid")
        .chain(fields.iter().map(|f| f.name.as_str()))
        .collect();
    let types: Vec<&str> = std::iter::once("string")
        .chain(fields.iter().map(|f| f.parquet_type.as_str()))
        .collect();
    let mut lines = Vec::new();
    let rows = for_each_row(lease, query, &types, &names, |row| {
        lines.push(encode_row(row, &types, &names)?);
        Ok(())
    })?;
    Ok(FileHash {
        rows,
        sha256: digest(lines),
    })
}

/// Run `query` and call `f` on each row as typed values. Checks each stored column against its
/// declared type first. Returns the number of rows.
pub(crate) fn for_each_row(
    lease: &Lease,
    query: &str,
    types: &[&str],
    names: &[&str],
    mut f: impl FnMut(&[Value]) -> Result<(), E>,
) -> Result<i64, E> {
    let csql = CString::new(query)?;
    unsafe {
        let mut res: duckdb_result = std::mem::zeroed();
        if duckdb_query(lease.conn(), csql.as_ptr(), &mut res) != duckdb_state_DuckDBSuccess {
            let message = take_cstr(duckdb_result_error(&mut res));
            duckdb_destroy_result(&mut res);
            return Err(message.into());
        }
        let outcome = read_rows(&mut res, types, names, &mut f);
        duckdb_destroy_result(&mut res);
        outcome
    }
}

/// # Safety
/// `res` must be a successful query result that is destroyed by the caller.
unsafe fn read_rows(
    res: &mut duckdb_result,
    types: &[&str],
    names: &[&str],
    f: &mut impl FnMut(&[Value]) -> Result<(), E>,
) -> Result<i64, E> {
    let columns = duckdb_column_count(res) as usize;
    if columns != names.len() {
        return Err(format!("expected {} columns, found {columns}", names.len()).into());
    }
    let mut stored = Vec::with_capacity(columns);
    for c in 0..columns {
        let family = stored_family(duckdb_column_type(res, c as idx_t))?;
        let declared = declared_family(types[c])?;
        if !compatible(declared, family) {
            return Err(format!(
                "column {}: stored as {family:?} but declared as {}",
                names[c], types[c]
            )
            .into());
        }
        stored.push(family);
    }
    let count = duckdb_row_count(res);
    for r in 0..count {
        let mut row = Vec::with_capacity(columns);
        for (c, kind) in stored.iter().enumerate() {
            if duckdb_value_is_null(res, c as idx_t, r) {
                row.push(Value::Null);
                continue;
            }
            row.push(match *kind {
                Stored::Float => Value::Float(duckdb_value_double(res, c as idx_t, r)),
                Stored::Integer => {
                    let h = duckdb_value_hugeint(res, c as idx_t, r);
                    Value::Int(((h.upper as i128) << 64) | (h.lower as i128))
                }
                Stored::Text => {
                    let p = duckdb_value_varchar(res, c as idx_t, r);
                    let text = CStr::from_ptr(p).to_string_lossy().into_owned();
                    duckdb_free(p as *mut c_void);
                    Value::Text(text)
                }
            });
        }
        f(&row)?;
    }
    Ok(count as i64)
}

fn take_cstr(p: *const std::os::raw::c_char) -> String {
    if p.is_null() {
        return "unknown DuckDB error".into();
    }
    unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value as Json;

    #[test]
    fn python_float_repr_matches_python() {
        let cases: &[(f64, &str)] = &[
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (3.0, "3.0"),
            (-2.25, "-2.25"),
            (0.5, "0.5"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1e15, "1000000000000000.0"),
            (1e16, "1e+16"),
            (1.0 / 3.0, "0.3333333333333333"),
            (0.1 + 0.2, "0.30000000000000004"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
        ];
        for (x, want) in cases {
            assert_eq!(python_float_repr(*x).unwrap(), *want, "{x:?}");
        }
        assert!(python_float_repr(f64::NAN).is_err());
        assert!(python_float_repr(f64::INFINITY).is_err());
    }

    #[test]
    fn strings_escape_as_python_does() {
        let mut out = String::new();
        json_string(
            "a\"b\\c\u{1}\u{1f}\u{8}\u{c}\n\r\t\u{7f}\u{2028}\u{e9}",
            &mut out,
        );
        assert_eq!(
            out,
            "\"a\\\"b\\\\c\\u0001\\u001f\\b\\f\\n\\r\\t\u{7f}\u{2028}\u{e9}\""
        );
    }

    /// The vectors in vectors/spec-2.0/content-hash.json, written by the Python reference.
    #[test]
    fn matches_the_conformance_vectors() {
        let doc: Json =
            serde_json::from_str(include_str!("../../../vectors/spec-2.0/content-hash.json"))
                .unwrap();
        for case in doc["cases"].as_array().unwrap() {
            let fields: Vec<(String, String)> = case["value_fields"]
                .as_array()
                .unwrap()
                .iter()
                .map(|f| {
                    (
                        f["name"].as_str().unwrap().into(),
                        f["parquet_type"].as_str().unwrap().into(),
                    )
                })
                .collect();
            let names: Vec<&str> = std::iter::once("gid")
                .chain(fields.iter().map(|f| f.0.as_str()))
                .collect();
            let types: Vec<&str> = std::iter::once("string")
                .chain(fields.iter().map(|f| f.1.as_str()))
                .collect();
            let mut lines = Vec::new();
            for row in case["rows"].as_array().unwrap() {
                let values: Vec<Value> = row.as_array().unwrap().iter().map(json_value).collect();
                lines.push(encode_row(&values, &types, &names).unwrap());
            }
            let name = case["name"].as_str().unwrap();
            let mut sorted = lines.clone();
            sorted.sort();
            let want: Vec<String> = case["encoded"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().into())
                .collect();
            assert_eq!(sorted, want, "encoded rows of {name}");
            assert_eq!(
                digest(lines),
                case["sha256"].as_str().unwrap(),
                "digest of {name}"
            );
        }
    }

    /// A JSON value of a vector row, as a typed value. Integers in double fields stay integers
    /// here, so the encoder must write them with a point.
    fn json_value(v: &Json) -> Value {
        match v {
            Json::Null => Value::Null,
            Json::String(s) => Value::Text(s.clone()),
            Json::Number(n) if n.is_i64() || n.is_u64() => Value::Int(n.as_i128().unwrap()),
            Json::Number(n) => Value::Float(n.as_f64().unwrap()),
            other => panic!("unexpected vector value {other}"),
        }
    }

    #[test]
    fn declared_family_refuses_unknown_types() {
        assert!(declared_family("bool").is_err());
        assert!(declared_family("date").is_err());
        assert_eq!(declared_family("uint64").unwrap(), Declared::Integer);
    }

    #[test]
    fn integers_in_double_fields_write_a_point() {
        assert_eq!(encode_value(&Value::Int(3), "double", "v").unwrap(), "3.0");
        assert_eq!(encode_value(&Value::Int(3), "int64", "v").unwrap(), "3");
        assert!(encode_value(&Value::Float(3.5), "int64", "v").is_err());
    }

    #[test]
    fn zero_rows_have_the_digest_of_empty_input() {
        assert_eq!(
            digest(Vec::new()),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
