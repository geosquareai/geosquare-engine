//! Planned reads of cell-dataset Iceberg tables without `iceberg_scan`.
//!
//! A small-area query needs a handful of data files out of thousands. `iceberg_scan` reads
//! and binds the whole manifest on every query (several times per `query_cell_dataset`), so
//! here the manifests are read once per process, kept in memory (metadata, manifest lists
//! and manifests are immutable files, so a cache keyed by path is always valid), and the
//! data files are chosen from the query's packed ids: the `truncate(W, packed_id)`
//! partition value, else the file's packed_id bounds. The chosen files are then read with
//! `read_parquet`.
//!
//! Only plain tables qualify: no delete files, primitive column types, and a way to prune
//! on packed_id. Anything else returns `None` and the caller uses `iceberg_scan`.

use crate::avro::{self, D};
use crate::sql::{self, lit, Lease, E};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// One live data file of the current snapshot.
#[derive(Debug, Clone)]
pub struct DataFile {
    pub path: String,
    /// truncate(width, packed_id) partition value and its width, when the spec has one.
    pub partition: Option<(i64, i64)>,
    /// packed_id lower/upper bounds from the manifest.
    pub bounds: Option<(i64, i64)>,
}

/// A manifest of the current snapshot and what the manifest list says about it.
#[derive(Debug)]
struct Manifest {
    path: String,
    /// (partition field name, truncate width) of its spec, when it truncates packed_id.
    spec: Option<(String, i64)>,
    /// Lowest and highest partition value of its files (manifest-list summary).
    range: Option<(i64, i64)>,
}

#[derive(Debug)]
pub struct Table {
    /// Current schema as (name, DuckDB type), in order.
    pub columns: Vec<(String, String)>,
    /// (field id, DuckDB type, default literal) per column, in the order of `columns`.
    fields: Vec<(i64, String, String)>,
    /// Read data files by field id (columns renamed, dropped or added after they were
    /// written: alter_cell_dataset).
    by_id: bool,
    pk_id: i64,
    location: String,
    root: String,
    manifests: Vec<Manifest>,
}

/// Partition values of `packed` (sorted) for truncate width `w`, sorted.
fn partition_values(packed: &[i64], w: i64) -> Vec<i64> {
    let mut v: Vec<i64> = packed.iter().map(|p| p - p.rem_euclid(w)).collect();
    v.dedup();
    v
}

impl Table {
    /// Data files that can hold any of `packed` (sorted ascending), or None when a manifest
    /// turns out to need iceberg_scan (delete files, nothing to prune on).
    pub fn files_for(&self, lease: &Lease, packed: &[i64]) -> Result<Option<Vec<String>>, E> {
        if packed.is_empty() {
            return Ok(Some(Vec::new()));
        }
        let (lo, hi) = (packed[0], packed[packed.len() - 1]);
        let mut by_width: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut parts = |w: i64| -> Vec<i64> {
            by_width
                .entry(w)
                .or_insert_with(|| partition_values(packed, w))
                .clone()
        };
        let mut out = Vec::new();
        for m in &self.manifests {
            if let (Some((_, w)), Some((a, b))) = (&m.spec, m.range) {
                let pv = parts(*w);
                let i = pv.partition_point(|v| *v < a);
                if i == pv.len() || pv[i] > b {
                    continue; // no query partition in this manifest's range
                }
            }
            let Some(files) = manifest_files(lease, self, m)? else {
                return Ok(None);
            };
            for f in files.iter() {
                let in_part = match f.partition {
                    Some((pt, w)) => parts(w).binary_search(&pt).is_ok(),
                    None => true,
                };
                let in_bounds = match f.bounds {
                    Some((a, b)) => b >= lo && a <= hi,
                    None => true,
                };
                if in_part && in_bounds {
                    out.push(f.path.clone());
                }
            }
        }
        Ok(Some(out))
    }

    /// Every live data file of the snapshot, or None when a manifest needs iceberg_scan.
    pub fn all_files(&self, lease: &Lease) -> Result<Option<Vec<String>>, E> {
        let mut out = Vec::new();
        for m in &self.manifests {
            let Some(files) = manifest_files(lease, self, m)? else {
                return Ok(None);
            };
            out.extend(files.iter().map(|f| f.path.clone()));
        }
        Ok(Some(out))
    }

    /// `read_parquet(...)` over `files` returning the current schema's columns by name; for
    /// altered tables the files are read by field id (renamed columns, defaults for added
    /// columns, dropped columns left out).
    pub fn scan(&self, files: &[String]) -> String {
        let list: Vec<String> = files.iter().map(|f| lit(f)).collect();
        if !self.by_id {
            return format!(
                "read_parquet([{}], hive_partitioning = false)",
                list.join(", ")
            );
        }
        let map: Vec<String> = self
            .columns
            .iter()
            .zip(&self.fields)
            .map(|((name, _), (id, ty, default))| {
                format!(
                    "{id}: {{name: {}, type: {}, default_value: {default}}}",
                    lit(name),
                    lit(ty)
                )
            })
            .collect();
        format!(
            "read_parquet([{}], hive_partitioning = false, schema = MAP {{{}}})",
            list.join(", "),
            map.join(", ")
        )
    }

    /// `SELECT NULL::type AS col, ...` with the table's columns and no rows.
    pub fn empty_select(&self) -> String {
        self.columns
            .iter()
            .map(|(n, t)| format!("NULL::{t} AS {}", sql_ident(n)))
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn sql_ident(n: &str) -> String {
    format!("\"{}\"", n.replace('"', "\"\""))
}

type TableCache = Mutex<HashMap<String, Option<Arc<Table>>>>;
type ManifestCache = Mutex<HashMap<String, Option<Arc<Vec<DataFile>>>>>;

fn tables() -> &'static TableCache {
    static C: OnceLock<TableCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn manifests() -> &'static ManifestCache {
    static C: OnceLock<ManifestCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Bytes of a file through DuckDB (local, s3://, http(s):// - also in DuckDB-Wasm).
fn fetch(lease: &Lease, path: &str) -> Result<Vec<u8>, E> {
    let rows = sql::query_text_on(
        lease,
        &format!("SELECT hex(content) FROM read_blob({})", lit(path)),
    )?;
    let hex = rows
        .into_iter()
        .next()
        .and_then(|r| r.into_iter().next().flatten())
        .ok_or_else(|| format!("cannot read {path}"))?;
    let b = hex.as_bytes();
    if b.len() % 2 != 0 {
        return Err(format!("{path}: bad hex").into());
    }
    let val = |c: u8| -> Result<u8, E> {
        match c {
            b'0'..=b'9' => Ok(c - b'0'),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'A'..=b'F' => Ok(c - b'A' + 10),
            _ => Err("bad hex digit".into()),
        }
    };
    b.chunks(2)
        .map(|p| Ok(val(p[0])? << 4 | val(p[1])?))
        .collect()
}

/// DuckDB type for an Iceberg primitive type; None for nested types.
fn duck_type(t: &Json) -> Option<String> {
    let s = t.as_str()?;
    Some(match s {
        "boolean" => "BOOLEAN".into(),
        "int" => "INTEGER".into(),
        "long" => "BIGINT".into(),
        "float" => "FLOAT".into(),
        "double" => "DOUBLE".into(),
        "date" => "DATE".into(),
        "time" => "TIME".into(),
        "timestamp" => "TIMESTAMP".into(),
        "timestamptz" => "TIMESTAMPTZ".into(),
        "string" => "VARCHAR".into(),
        "uuid" => "UUID".into(),
        "binary" => "BLOB".into(),
        _ if s.starts_with("decimal(") => format!("DECIMAL{}", &s["decimal".len()..]),
        _ => return None,
    })
}

/// Map a path written under the table's `location` to the folder it is read from.
fn relocate(path: &str, location: &str, root: &str) -> String {
    let loc = location.trim_end_matches('/');
    match path.strip_prefix(loc) {
        Some(rest) if loc != root.trim_end_matches('/') => {
            format!("{}{}", root.trim_end_matches('/'), rest)
        }
        _ => path.to_string(),
    }
}

/// The planned-read view of the table at `metadata_path` (cached), or None when the
/// table needs `iceberg_scan`. Reads the manifest list; manifests are read on demand.
pub fn load(
    lease: &Lease,
    root: &str,
    metadata_path: &str,
    meta: &Json,
) -> Result<Option<Arc<Table>>, E> {
    if let Some(hit) = tables().lock().unwrap().get(metadata_path) {
        return Ok(hit.clone());
    }
    let t = build(lease, root, meta)?.map(Arc::new);
    tables()
        .lock()
        .unwrap()
        .insert(metadata_path.to_string(), t.clone());
    Ok(t)
}

fn build(lease: &Lease, root: &str, meta: &Json) -> Result<Option<Table>, E> {
    let location = meta
        .get("location")
        .and_then(Json::as_str)
        .unwrap_or(root)
        .to_string();
    let sid = meta
        .get("current-schema-id")
        .and_then(Json::as_i64)
        .unwrap_or(0);
    let schema = meta
        .get("schemas")
        .and_then(Json::as_array)
        .and_then(|a| {
            a.iter()
                .find(|s| s.get("schema-id").and_then(Json::as_i64) == Some(sid))
        })
        .or_else(|| meta.get("schema"));
    let Some(fields) = schema
        .and_then(|s| s.get("fields"))
        .and_then(Json::as_array)
    else {
        return Ok(None);
    };
    let mut columns = Vec::new();
    let mut ids = Vec::new();
    let mut pk_id = None;
    for f in fields {
        let name = f
            .get("name")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string();
        let Some(ty) = f.get("type").and_then(duck_type) else {
            return Ok(None);
        };
        if name == "packed_id" {
            pk_id = f.get("id").and_then(Json::as_i64);
        }
        // MAP values need one type: untyped NULL, or a plain number for added numeric columns
        // (read_parquet casts the default to the column type).
        let default = match f.get("initial-default") {
            Some(Json::Number(n)) => format!("{n}"),
            _ => "NULL".to_string(),
        };
        ids.push((
            f.get("id").and_then(Json::as_i64).unwrap_or(-1),
            ty.clone(),
            default,
        ));
        columns.push((name, ty));
    }
    let Some(pk_id) = pk_id else {
        return Ok(None);
    };
    // spec id -> (partition field name, W) for specs that are exactly truncate(W, packed_id).
    let mut specs: HashMap<i64, (String, i64)> = HashMap::new();
    for s in meta
        .get("partition-specs")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        let id = s.get("spec-id").and_then(Json::as_i64).unwrap_or(0);
        let f = s
            .get("fields")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        if let [one] = f.as_slice() {
            let tr = one.get("transform").and_then(Json::as_str).unwrap_or("");
            let w = tr
                .strip_prefix("truncate[")
                .and_then(|r| r.strip_suffix(']'))
                .and_then(|n| n.parse::<i64>().ok());
            if let (Some(w), Some(src)) = (w, one.get("source-id").and_then(Json::as_i64)) {
                if src == pk_id && w > 0 {
                    let name = one
                        .get("name")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .to_string();
                    specs.insert(id, (name, w));
                }
            }
        }
    }
    let by_id = meta
        .pointer("/properties/geosquare.read-by-field-id")
        .and_then(Json::as_str)
        == Some("true");
    let mut table = Table {
        columns,
        fields: ids,
        by_id,
        pk_id,
        location,
        root: root.to_string(),
        manifests: Vec::new(),
    };
    let snap_id = meta
        .get("current-snapshot-id")
        .and_then(Json::as_i64)
        .unwrap_or(-1);
    let snap = meta
        .get("snapshots")
        .and_then(Json::as_array)
        .and_then(|a| {
            a.iter()
                .find(|s| s.get("snapshot-id").and_then(Json::as_i64) == Some(snap_id))
        });
    let Some(snap) = snap else {
        return Ok(Some(table)); // empty table
    };
    let Some(list) = snap.get("manifest-list").and_then(Json::as_str) else {
        return Ok(None);
    };
    let list = crate::iceberg::read_manifest_list(&fetch(lease, &table.relocate(list))?)?;
    if list.iter().any(|m| m.content != 0) {
        return Ok(None); // delete files: leave merge-on-read to iceberg_scan
    }
    for m in list {
        let spec = specs.get(&m.spec_id).cloned();
        let le = |b: &Option<Vec<u8>>| -> Option<i64> {
            let b = b.as_ref()?;
            Some(i64::from_le_bytes(b.get(..8)?.try_into().ok()?))
        };
        let range = match (&spec, m.partitions.as_deref()) {
            (Some(_), Some([(_, lo, hi)])) => le(lo).zip(le(hi)),
            _ => None,
        };
        table.manifests.push(Manifest {
            path: table.relocate(&m.path),
            spec,
            range,
        });
    }
    Ok(Some(table))
}

impl Table {
    /// Map a path written under the table's `location` to the folder it is read from.
    fn relocate(&self, path: &str) -> String {
        relocate(path, &self.location, &self.root)
    }
}

/// Live data files of one manifest (cached by path), or None if it needs iceberg_scan.
fn manifest_files(lease: &Lease, t: &Table, m: &Manifest) -> Result<Option<Arc<Vec<DataFile>>>, E> {
    if let Some(hit) = manifests().lock().unwrap().get(&m.path) {
        return Ok(hit.clone());
    }
    let parsed = parse_manifest(lease, t, m)?.map(Arc::new);
    manifests()
        .lock()
        .unwrap()
        .insert(m.path.clone(), parsed.clone());
    Ok(parsed)
}

fn parse_manifest(lease: &Lease, t: &Table, m: &Manifest) -> Result<Option<Vec<DataFile>>, E> {
    let (_, entries) = avro::read_container(&fetch(lease, &m.path)?)?;
    let mut files = Vec::new();
    for e in entries {
        if e.get("status").and_then(D::long) == Some(2) {
            continue; // deleted entry
        }
        let Some(df) = e.get("data_file") else {
            continue;
        };
        if df.get("content").and_then(D::long).unwrap_or(0) != 0 {
            return Ok(None);
        }
        let path = match df.get("file_path") {
            Some(D::Str(s)) => t.relocate(s),
            _ => return Ok(None),
        };
        let partition = m
            .spec
            .as_ref()
            .and_then(|(name, w)| df.get("partition")?.get(name)?.long().map(|v| (v, *w)));
        let bound = |n: &str| -> Option<i64> {
            let D::Arr(items) = df.get(n)? else {
                return None;
            };
            items.iter().find_map(|kv| {
                if kv.get("key")?.long()? != t.pk_id {
                    return None;
                }
                match kv.get("value")? {
                    D::Bytes(b) if b.len() == 8 => {
                        Some(i64::from_le_bytes(b[..8].try_into().ok()?))
                    }
                    _ => None,
                }
            })
        };
        let bounds = bound("lower_bounds").zip(bound("upper_bounds"));
        if partition.is_none() && bounds.is_none() {
            return Ok(None); // nothing to prune on
        }
        files.push(DataFile {
            path,
            partition,
            bounds,
        });
    }
    Ok(Some(files))
}
