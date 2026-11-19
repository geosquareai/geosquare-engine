//! Iceberg format-version 2 metadata for cell tables written by `write_cell_dataset`.
//!
//! The table has no catalog: readers take the newest metadata file (see
//! `dataset::latest_iceberg_metadata`), and `metadata/version-hint.text` points to it so
//! `iceberg_scan('<table dir>')` works too. Data files are written by DuckDB `COPY`;
//! this module writes the manifest, manifest list and metadata JSON.

use crate::avro::{self, D, V};
use serde_json::{json, Value as Json};
use std::error::Error;

type E = Box<dyn Error>;

pub const PARTITION_FIELD: &str = "pk_trunc";
pub const PARTITION_FIELD_ID: i64 = 1000;

/// One table column.
#[derive(Debug, Clone)]
pub struct Field {
    pub id: i64,
    pub name: String,
    pub ty: String,
}

/// One written data file and its column metrics, keyed by field id.
#[derive(Debug, Clone, Default)]
pub struct DataFile {
    pub path: String,
    pub record_count: i64,
    pub size: i64,
    pub partition: i64,
    pub column_sizes: Vec<(i64, i64)>,
    pub value_counts: Vec<(i64, i64)>,
    pub null_counts: Vec<(i64, i64)>,
    pub lower: Vec<(i64, Vec<u8>)>,
    pub upper: Vec<(i64, Vec<u8>)>,
}

fn map_schema(name: &str, key_id: i64, value_id: i64, value_type: &str) -> Json {
    json!({"type": "array", "logicalType": "map", "items": {"type": "record", "name": name, "fields": [
        {"name": "key", "type": "int", "field-id": key_id},
        {"name": "value", "type": value_type, "field-id": value_id}]}})
}

pub fn manifest_entry_schema() -> String {
    let opt = |name: &str, id: i64, t: Json| json!({"name": name, "type": ["null", t], "default": null, "field-id": id});
    json!({"type": "record", "name": "manifest_entry", "fields": [
        {"name": "status", "type": "int", "field-id": 0},
        opt("snapshot_id", 1, json!("long")),
        opt("sequence_number", 3, json!("long")),
        opt("file_sequence_number", 4, json!("long")),
        {"name": "data_file", "field-id": 2, "type": {"type": "record", "name": "r2", "fields": [
            {"name": "content", "type": "int", "field-id": 134},
            {"name": "file_path", "type": "string", "field-id": 100},
            {"name": "file_format", "type": "string", "field-id": 101},
            {"name": "partition", "field-id": 102, "type": {"type": "record", "name": "r102", "fields": [
                {"name": PARTITION_FIELD, "type": ["null", "long"], "default": null, "field-id": PARTITION_FIELD_ID}]}},
            {"name": "record_count", "type": "long", "field-id": 103},
            {"name": "file_size_in_bytes", "type": "long", "field-id": 104},
            opt("column_sizes", 108, map_schema("k117_v118", 117, 118, "long")),
            opt("value_counts", 109, map_schema("k119_v120", 119, 120, "long")),
            opt("null_value_counts", 110, map_schema("k121_v122", 121, 122, "long")),
            opt("lower_bounds", 125, map_schema("k126_v127", 126, 127, "bytes")),
            opt("upper_bounds", 128, map_schema("k129_v130", 129, 130, "bytes")),
            opt("key_metadata", 131, json!("bytes")),
            opt("sort_order_id", 140, json!("int"))
        ]}}
    ]})
    .to_string()
}

pub fn manifest_file_schema() -> String {
    json!({"type": "record", "name": "manifest_file", "fields": [
        {"name": "manifest_path", "type": "string", "field-id": 500},
        {"name": "manifest_length", "type": "long", "field-id": 501},
        {"name": "partition_spec_id", "type": "int", "field-id": 502},
        {"name": "content", "type": "int", "field-id": 517},
        {"name": "sequence_number", "type": "long", "field-id": 515},
        {"name": "min_sequence_number", "type": "long", "field-id": 516},
        {"name": "added_snapshot_id", "type": "long", "field-id": 503},
        {"name": "added_files_count", "type": "int", "field-id": 504},
        {"name": "existing_files_count", "type": "int", "field-id": 505},
        {"name": "deleted_files_count", "type": "int", "field-id": 506},
        {"name": "added_rows_count", "type": "long", "field-id": 512},
        {"name": "existing_rows_count", "type": "long", "field-id": 513},
        {"name": "deleted_rows_count", "type": "long", "field-id": 514},
        {"name": "partitions", "default": null, "field-id": 507, "type": ["null", {"type": "array", "element-id": 508, "items": {
            "type": "record", "name": "r508", "fields": [
                {"name": "contains_null", "type": "boolean", "field-id": 509},
                {"name": "contains_nan", "type": ["null", "boolean"], "default": null, "field-id": 518},
                {"name": "lower_bound", "type": ["null", "bytes"], "default": null, "field-id": 510},
                {"name": "upper_bound", "type": ["null", "bytes"], "default": null, "field-id": 511}]}}]},
        {"name": "key_metadata", "type": ["null", "bytes"], "default": null, "field-id": 519}
    ]})
    .to_string()
}

fn some(v: V) -> V {
    V::U(1, Box::new(v))
}

fn kv_long(pairs: &[(i64, i64)]) -> V {
    some(V::Arr(
        pairs
            .iter()
            .map(|(k, v)| V::Rec(vec![V::Int(*k as i32), V::Long(*v)]))
            .collect(),
    ))
}

fn kv_bytes(pairs: &[(i64, Vec<u8>)]) -> V {
    some(V::Arr(
        pairs
            .iter()
            .map(|(k, v)| V::Rec(vec![V::Int(*k as i32), V::Bytes(v.clone())]))
            .collect(),
    ))
}

pub fn schema_json(fields: &[Field], schema_id: i64) -> Json {
    json!({"type": "struct", "schema-id": schema_id, "fields": fields.iter().map(|f| json!({
        "id": f.id, "name": f.name, "required": false, "type": f.ty})).collect::<Vec<_>>()})
}

pub fn spec_json(source_id: i64, width: i64) -> Json {
    json!({"spec-id": 0, "fields": [{"name": PARTITION_FIELD, "transform": format!("truncate[{width}]"),
        "source-id": source_id, "field-id": PARTITION_FIELD_ID}]})
}

/// Encode the manifest (ADDED entries) for `files`.
pub fn manifest_bytes(
    files: &[DataFile],
    snapshot_id: i64,
    schema: &Json,
    spec: &Json,
    sync: [u8; 16],
) -> Vec<u8> {
    let records: Vec<V> = files
        .iter()
        .map(|f| {
            V::Rec(vec![
                V::Int(1), // ADDED
                some(V::Long(snapshot_id)),
                V::U(0, Box::new(V::Null)), // sequence number inherited from the manifest list
                V::U(0, Box::new(V::Null)),
                V::Rec(vec![
                    V::Int(0),
                    V::Str(f.path.clone()),
                    V::Str("PARQUET".into()),
                    V::Rec(vec![some(V::Long(f.partition))]),
                    V::Long(f.record_count),
                    V::Long(f.size),
                    kv_long(&f.column_sizes),
                    kv_long(&f.value_counts),
                    kv_long(&f.null_counts),
                    kv_bytes(&f.lower),
                    kv_bytes(&f.upper),
                    V::U(0, Box::new(V::Null)),
                    some(V::Int(1)),
                ]),
            ])
        })
        .collect();
    let meta = [
        ("schema", schema.to_string()),
        (
            "schema-id",
            schema
                .get("schema-id")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "0".into()),
        ),
        (
            "partition-spec",
            spec.get("fields").cloned().unwrap_or(json!([])).to_string(),
        ),
        (
            "partition-spec-id",
            spec.get("spec-id")
                .map(|v| v.to_string())
                .unwrap_or_else(|| "0".into()),
        ),
        ("format-version", "2".into()),
        ("content", "data".into()),
    ];
    avro::write_container(&manifest_entry_schema(), &meta, &records, sync)
}

/// `(contains_null, lower_bound, upper_bound)` of one partition field.
pub type PartitionSummary = (bool, Option<Vec<u8>>, Option<Vec<u8>>);

/// A manifest-list entry, either new or carried from the previous snapshot.
pub struct ManifestFile {
    pub path: String,
    pub length: i64,
    pub spec_id: i64,
    pub content: i64,
    pub sequence_number: i64,
    pub min_sequence_number: i64,
    pub added_snapshot_id: i64,
    pub added_files: i64,
    pub existing_files: i64,
    pub deleted_files: i64,
    pub added_rows: i64,
    pub existing_rows: i64,
    pub deleted_rows: i64,
    /// (contains_null, lower, upper) per partition field.
    pub partitions: Option<Vec<PartitionSummary>>,
}

fn opt_bytes(b: &Option<Vec<u8>>) -> V {
    match b {
        Some(v) => some(V::Bytes(v.clone())),
        None => V::U(0, Box::new(V::Null)),
    }
}

pub fn manifest_list_bytes(
    entries: &[ManifestFile],
    snapshot_id: i64,
    parent: Option<i64>,
    seq: i64,
    sync: [u8; 16],
) -> Vec<u8> {
    let records: Vec<V> = entries
        .iter()
        .map(|m| {
            V::Rec(vec![
                V::Str(m.path.clone()),
                V::Long(m.length),
                V::Int(m.spec_id as i32),
                V::Int(m.content as i32),
                V::Long(m.sequence_number),
                V::Long(m.min_sequence_number),
                V::Long(m.added_snapshot_id),
                V::Int(m.added_files as i32),
                V::Int(m.existing_files as i32),
                V::Int(m.deleted_files as i32),
                V::Long(m.added_rows),
                V::Long(m.existing_rows),
                V::Long(m.deleted_rows),
                match &m.partitions {
                    Some(p) => some(V::Arr(
                        p.iter()
                            .map(|(n, lo, hi)| {
                                V::Rec(vec![
                                    V::Bool(*n),
                                    V::U(0, Box::new(V::Null)),
                                    opt_bytes(lo),
                                    opt_bytes(hi),
                                ])
                            })
                            .collect(),
                    )),
                    None => V::U(0, Box::new(V::Null)),
                },
                V::U(0, Box::new(V::Null)),
            ])
        })
        .collect();
    let meta = [
        ("snapshot-id", snapshot_id.to_string()),
        (
            "parent-snapshot-id",
            parent
                .map(|p| p.to_string())
                .unwrap_or_else(|| "null".into()),
        ),
        ("sequence-number", seq.to_string()),
        ("format-version", "2".into()),
    ];
    avro::write_container(&manifest_file_schema(), &meta, &records, sync)
}

/// Read the manifest list of a previous snapshot so its manifests stay in the table.
pub fn read_manifest_list(bytes: &[u8]) -> Result<Vec<ManifestFile>, E> {
    let (_, records) = avro::read_container(bytes)?;
    let long = |r: &D, n: &str| r.get(n).and_then(D::long);
    let mut out = Vec::new();
    for r in records {
        let path = match r.get("manifest_path") {
            Some(D::Str(s)) => s.clone(),
            _ => return Err("manifest list entry without manifest_path".into()),
        };
        let partitions = match r.get("partitions") {
            Some(D::Arr(items)) => Some(
                items
                    .iter()
                    .map(|p| {
                        let b = |n: &str| match p.get(n) {
                            Some(D::Bytes(v)) => Some(v.clone()),
                            _ => None,
                        };
                        (
                            matches!(p.get("contains_null"), Some(D::Bool(true))),
                            b("lower_bound"),
                            b("upper_bound"),
                        )
                    })
                    .collect(),
            ),
            _ => None,
        };
        out.push(ManifestFile {
            path,
            length: long(&r, "manifest_length").unwrap_or(0),
            spec_id: long(&r, "partition_spec_id").unwrap_or(0),
            content: long(&r, "content").unwrap_or(0),
            sequence_number: long(&r, "sequence_number").unwrap_or(0),
            min_sequence_number: long(&r, "min_sequence_number").unwrap_or(0),
            added_snapshot_id: long(&r, "added_snapshot_id").unwrap_or(0),
            added_files: long(&r, "added_files_count")
                .or_else(|| long(&r, "added_data_files_count"))
                .unwrap_or(0),
            existing_files: long(&r, "existing_files_count")
                .or_else(|| long(&r, "existing_data_files_count"))
                .unwrap_or(0),
            deleted_files: long(&r, "deleted_files_count")
                .or_else(|| long(&r, "deleted_data_files_count"))
                .unwrap_or(0),
            added_rows: long(&r, "added_rows_count").unwrap_or(0),
            existing_rows: long(&r, "existing_rows_count").unwrap_or(0),
            deleted_rows: long(&r, "deleted_rows_count").unwrap_or(0),
            partitions,
        });
    }
    Ok(out)
}

/// Single-value serialisation of a bound (Iceberg spec appendix D) from DuckDB's text stats.
pub fn bound_bytes(ty: &str, text: &str) -> Option<Vec<u8>> {
    match ty {
        "int" | "date" => text.parse::<i32>().ok().map(|v| v.to_le_bytes().to_vec()),
        "long" => text.parse::<i64>().ok().map(|v| v.to_le_bytes().to_vec()),
        "float" => text
            .parse::<f32>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| v.to_le_bytes().to_vec()),
        "double" => text
            .parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .map(|v| v.to_le_bytes().to_vec()),
        "boolean" => match text {
            "true" => Some(vec![1]),
            "false" => Some(vec![0]),
            _ => None,
        },
        "string" => Some(text.as_bytes().to_vec()),
        _ => None,
    }
}
