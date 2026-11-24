//! `write_cell_dataset(query, path, layout := 'flat' | 'partitioned' | 'iceberg', ...)`.
//!
//! The query result is staged once in a temporary table sorted by `packed_id`, checked
//! (one domain, one level, unique cells, gid/packed_id/x_idx/y_idx agree), and written:
//!
//! * flat        — `<path>/data/part-00000.parquet` + `<path>/manifest.json`
//! * partitioned — one sorted file per `gid_prefix=<L chars>` (spec 2.0 section 15)
//! * iceberg     — one sorted file per `truncate(W, packed_id)` partition, then a new
//!   snapshot (manifest, manifest list, metadata JSON, version hint)
//!
//! Every Parquet file carries the cell manifest under the key `geosquare.manifest`, and
//! files are zstd-compressed with small row groups so packed_id ranges prune well.

use crate::{
    dataset::{self, join, MANIFEST_FILE, MANIFEST_KEY, PREFIX_INDEX_FILE},
    grid::{domain_by_code, level_u8},
    iceberg::{self, DataFile, Field, ManifestFile},
    manifest11::{self, FieldIn, FileSpec},
    metadata::PerColumn,
    sql::{self, ident, lit, Lease, Params, Plan, E},
    storage::{is_local, local, S3Params, Storage},
};
use geosquare_core::{codec, domain::Domain, packing, topology};
use serde_json::{json, Value as Json};
use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// In-memory database for staged rows, attached once per database so every internal
/// connection (the parallel partition writers) can read the staged table.
const STAGE_DB: &str = "gs_stage_mem";

thread_local! {
    static STAGE_NAME: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// The staged table of the write running on this thread.
fn stage_table() -> String {
    STAGE_NAME.with(|s| s.borrow().clone())
}

/// Partitions written at the same time (`GEOSQUARE_V2_WRITE_THREADS`, default 8).
fn write_threads() -> usize {
    std::env::var("GEOSQUARE_V2_WRITE_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8)
}
const IDENTITY: [&str; 4] = ["gid", "packed_id", "x_idx", "y_idx"];
const RESERVED: [&str; 10] = [
    "domain",
    "level",
    "source_id",
    "source_row",
    "coverage_ratio",
    "length_ratio",
    "assignment_method",
    "boundary_policy",
    "geometry_wkb",
    "gid_prefix",
];

#[derive(Clone)]
pub struct Options {
    layout: String,
    mode: String,
    domain_code: Option<String>,
    level: Option<i64>,
    partition_level: Option<i64>,
    row_group_size: i64,
    value_semantics: String,
    aggregation_rule: Option<String>,
    value_fields: Option<String>,
    notes: Option<String>,
    /// 'query' (gid, packed_id, x_idx, y_idx) or 'compact' (gid only), as in the contract.
    identity: String,
    /// Iceberg column metrics: 'auto' (Iceberg default: first 100 columns), 'key' (cell id
    /// columns only; for wide tables), 'all'.
    column_metrics: String,
    /// Units, descriptions, title, period/version, source and coverage fields.
    extra: crate::metadata::Extra,
    /// Write 0 instead of NULL in numeric value columns (default true).
    fill_nulls: bool,
    /// Aggregate the input cells to this ancestor level before writing (see rollup_query).
    rollup_level: Option<i64>,
    /// Parent rule per column (`space_aggregation`), used by rollup_level.
    space_aggregation: Option<String>,
    /// Weight per column (`space_weight`): a COUNT or TOTAL column, or `area`.
    space_weight: Option<String>,
    /// value_semantics was passed (not the default), so it is not inherited.
    value_semantics_given: bool,
    /// (cell manifest, dataset_info) of the dataset being appended to or overwritten: its
    /// metadata is kept unless this write passes other values.
    prev: Option<(Json, Json)>,
    s3: S3Params,
    /// `manifest_version` as given (`1.0` or `1.1`). None keeps the version of the dataset
    /// being appended to or overwritten, else 1.0.
    manifest_version: Option<String>,
    /// The manifest version this write produces, set by `write` from the parameter, the
    /// dataset being replaced, and the default.
    version: String,
    /// Governance and lineage parameters (manifest 1.1).
    gov: manifest11::Params,
}

/// Data files per Iceberg manifest (see write_iceberg).
const MANIFEST_FILES: usize = 128;

/// Cell id columns, which always get full Iceberg metrics.
const KEY_COLUMNS: [&str; 4] = ["packed_id", "gid", "x_idx", "y_idx"];

pub(crate) fn s3_params_pub(p: &Params) -> S3Params {
    s3_params(p)
}

fn s3_params(p: &Params) -> S3Params {
    S3Params {
        region: p.text("s3_region"),
        endpoint: p.text("s3_endpoint"),
        url_style: p.text("s3_url_style"),
        use_ssl: p
            .text("s3_use_ssl")
            .map(|v| !matches!(v.to_ascii_lowercase().as_str(), "false" | "0" | "no")),
        access_key_id: p.text("s3_access_key_id"),
        secret_access_key: p.text("s3_secret_access_key"),
        session_token: p.text("s3_session_token"),
    }
}

pub fn plan_write(p: &Params) -> Result<Plan, E> {
    let query = p.pos_text(0, "query")?;
    let path = p.pos_text(1, "path")?;
    if let Some(v) = p.text("manifest_version") {
        if v != "1.0" && v != "1.1" {
            return Err(format!("manifest_version must be '1.0' or '1.1'; received {v:?}").into());
        }
    }
    let gov = manifest11::Params {
        owner: p.text("owner"),
        source_name: p.text("source_name"),
        license: p.text("license"),
        share_policy: p.text("share_policy"),
        refresh: p.text("refresh"),
        derived_from: p.text("derived_from"),
    };
    manifest11::check_params(&gov)?;
    let o = Options {
        layout: p
            .text("layout")
            .unwrap_or_else(|| "flat".into())
            .to_ascii_lowercase(),
        mode: p
            .text("mode")
            .unwrap_or_else(|| "create".into())
            .to_ascii_lowercase(),
        domain_code: p.text("domain_code"),
        level: p.int("level"),
        partition_level: p.int("partition_level"),
        row_group_size: p.int("row_group_size").unwrap_or(8192),
        value_semantics: p
            .text("value_semantics")
            .unwrap_or_else(|| "MEASUREMENT".into()),
        aggregation_rule: p.text("aggregation_rule"),
        value_fields: p.text("value_fields"),
        notes: p.text("notes"),
        identity: p
            .text("identity")
            .unwrap_or_else(|| "query".into())
            .to_ascii_lowercase(),
        fill_nulls: p.int("fill_nulls").is_none_or(|v| v != 0),
        rollup_level: p.int("rollup_level"),
        space_aggregation: p.text("space_aggregation"),
        space_weight: p.text("space_weight"),
        value_semantics_given: p.text("value_semantics").is_some(),
        prev: None,
        column_metrics: p
            .text("column_metrics")
            .unwrap_or_else(|| "auto".into())
            .to_ascii_lowercase(),
        extra: crate::metadata::Extra::from_params(p)?,
        manifest_version: p.text("manifest_version"),
        version: "1.0".into(),
        gov,
        s3: s3_params(p),
    };
    if !matches!(o.identity.as_str(), "query" | "compact") {
        return Err(
            "identity must be 'query' (gid, packed_id, x_idx, y_idx) or 'compact' (gid only)"
                .into(),
        );
    }
    if o.identity == "compact" && o.layout == "iceberg" {
        return Err(
            "identity 'compact' is for flat/partitioned folders; Iceberg tables keep packed_id"
                .into(),
        );
    }
    if !matches!(o.layout.as_str(), "flat" | "partitioned" | "iceberg") {
        return Err("layout must be 'flat', 'partitioned' or 'iceberg'".into());
    }
    if !matches!(
        o.mode.as_str(),
        "create" | "overwrite" | "append" | "upsert"
    ) {
        return Err("mode must be 'create', 'overwrite', 'append' or 'upsert'".into());
    }
    if o.mode == "upsert" && o.layout != "iceberg" {
        return Err("mode 'upsert' is only for layout 'iceberg' (folders are rewritten whole: use 'overwrite')".into());
    }
    if o.mode == "append" && o.layout != "iceberg" {
        return Err("mode 'append' is only for layout 'iceberg' (a contract folder embeds row_count in every file)".into());
    }
    if o.row_group_size < 1 {
        return Err("row_group_size must be positive".into());
    }
    let columns = ["layout", "location", "files", "rows"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    Ok(Plan::Action(
        Box::new(move || write(&query, &path, &o)),
        columns,
    ))
}

// ---------------------------------------------------------------------------

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
    h.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    h.finish()
}

pub(crate) fn uuid4() -> String {
    let (a, b) = (random_u64(), random_u64());
    let a = (a & 0xffff_ffff_ffff_0fff) | 0x0000_0000_0000_4000;
    let b = (b & 0x3fff_ffff_ffff_ffff) | 0x8000_0000_0000_0000;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xffff,
        b >> 48,
        b & 0xffff_ffff_ffff
    )
}

fn sync_marker() -> [u8; 16] {
    let mut s = [0u8; 16];
    s[..8].copy_from_slice(&random_u64().to_le_bytes());
    s[8..].copy_from_slice(&random_u64().to_le_bytes());
    s
}

fn iso_now() -> String {
    let secs = now_ms() / 1000;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Data files for an S3 target: DuckDB writes them to a local folder and the native client
/// uploads them in parallel (a remote COPY per partition is slow when there are thousands).
/// Folder: $GEOSQUARE_V2_STAGING_DIR or the system temp dir; requests at a time:
/// $GEOSQUARE_V2_UPLOAD_CONCURRENCY (default 32). The folder is removed afterwards.
struct Staging {
    dir: Option<std::path::PathBuf>,
    files: Vec<(String, std::path::PathBuf)>,
}

impl Staging {
    fn new(root: &str) -> Result<Self, E> {
        if !crate::storage::is_s3(root) || cfg!(not(feature = "native-s3")) {
            return Ok(Staging {
                dir: None,
                files: Vec::new(),
            });
        }
        #[cfg(target_family = "wasm")]
        return Err("writing to S3 is not available in DuckDB-Wasm".into());
        #[cfg(not(target_family = "wasm"))]
        {
            let base = std::env::var_os("GEOSQUARE_V2_STAGING_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or_else(std::env::temp_dir);
            let dir = base.join(format!("geosquare_v2_upload_{}", uuid4()));
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create staging folder {}: {e}", dir.display()))?;
            Ok(Staging {
                dir: Some(dir),
                files: Vec::new(),
            })
        }
    }

    /// Where DuckDB writes the file that ends up at `remote` (`rel` below the table root).
    fn path(&mut self, remote: &str, rel: &str) -> Result<String, E> {
        let Some(dir) = &self.dir else {
            return Ok(remote.to_string());
        };
        let p = dir.join(rel);
        #[cfg(not(target_family = "wasm"))]
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        self.files.push((remote.to_string(), p.clone()));
        Ok(p.to_string_lossy().into_owned())
    }

    fn upload(&mut self, store: &Storage) -> Result<(), E> {
        if self.files.is_empty() {
            return Ok(());
        }
        let n = std::env::var("GEOSQUARE_V2_UPLOAD_CONCURRENCY")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(32);
        store.upload_files(&self.files, n)?;
        self.files.clear();
        Ok(())
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        #[cfg(not(target_family = "wasm"))]
        if let Some(d) = &self.dir {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}

fn ensure_dir(path: &str) -> Result<(), E> {
    #[cfg(not(target_family = "wasm"))]
    if is_local(path) {
        std::fs::create_dir_all(local(path)).map_err(|e| format!("cannot create {path}: {e}"))?;
    }
    #[cfg(target_family = "wasm")]
    let _ = path;
    Ok(())
}

fn rows(lease: &Lease, sql: &str) -> Result<Vec<Vec<Option<String>>>, E> {
    sql::query_text_on(lease, sql)
}

fn one_row(lease: &Lease, sql: &str) -> Result<Vec<Option<String>>, E> {
    rows(lease, sql)?
        .into_iter()
        .next()
        .ok_or_else(|| "query returned no row".into())
}

fn num(v: &Option<String>) -> i64 {
    v.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// (name, DuckDB type) of a query.
fn describe(lease: &Lease, sql: &str) -> Result<Vec<(String, String)>, E> {
    Ok(rows(
        lease,
        &format!("SELECT column_name, column_type FROM (DESCRIBE {sql})"),
    )?
    .into_iter()
    .map(|r| {
        (
            r[0].clone().unwrap_or_default(),
            r[1].clone().unwrap_or_default(),
        )
    })
    .collect())
}

/// Arrow type name for the manifest `parquet_type` (as geosquare-v2 compares it).
fn parquet_type(duck: &str) -> String {
    let t = duck.to_ascii_uppercase();
    match t.as_str() {
        "TINYINT" => "int8".into(),
        "SMALLINT" => "int16".into(),
        "INTEGER" => "int32".into(),
        "BIGINT" => "int64".into(),
        "UTINYINT" => "uint8".into(),
        "USMALLINT" => "uint16".into(),
        "UINTEGER" => "uint32".into(),
        "UBIGINT" => "uint64".into(),
        "FLOAT" => "float".into(),
        "DOUBLE" => "double".into(),
        "VARCHAR" => "string".into(),
        "BOOLEAN" => "bool".into(),
        "BLOB" => "binary".into(),
        "DATE" => "date32[day]".into(),
        "TIMESTAMP" => "timestamp[us]".into(),
        _ if t.starts_with("DECIMAL") => t
            .replace("DECIMAL", "decimal128")
            .replace(',', ", ")
            .to_lowercase(),
        _ => t.to_lowercase(),
    }
}

fn is_numeric(duck: &str) -> bool {
    let t = duck.to_ascii_uppercase();
    matches!(
        t.as_str(),
        "TINYINT"
            | "SMALLINT"
            | "INTEGER"
            | "BIGINT"
            | "UTINYINT"
            | "USMALLINT"
            | "UINTEGER"
            | "UBIGINT"
            | "FLOAT"
            | "DOUBLE"
            | "HUGEINT"
    ) || t.starts_with("DECIMAL")
}

/// Iceberg type and the SQL cast that produces it.
fn iceberg_type(duck: &str) -> Result<(String, Option<&'static str>), E> {
    let t = duck.to_ascii_uppercase();
    Ok(match t.as_str() {
        "BOOLEAN" => ("boolean".into(), None),
        "TINYINT" | "SMALLINT" | "UTINYINT" | "USMALLINT" => ("int".into(), Some("INTEGER")),
        "INTEGER" => ("int".into(), None),
        "UINTEGER" => ("long".into(), Some("BIGINT")),
        "BIGINT" => ("long".into(), None),
        "FLOAT" => ("float".into(), None),
        "DOUBLE" => ("double".into(), None),
        "VARCHAR" => ("string".into(), None),
        "BLOB" => ("binary".into(), None),
        "DATE" => ("date".into(), None),
        "TIMESTAMP" => ("timestamp".into(), None),
        "TIMESTAMP WITH TIME ZONE" => ("timestamptz".into(), None),
        _ if t.starts_with("DECIMAL") => (
            t.replace("DECIMAL", "decimal")
                .replace(',', ", ")
                .to_lowercase(),
            None,
        ),
        _ => {
            return Err(format!(
                "column type {duck} has no Iceberg equivalent; cast it in the query"
            )
            .into())
        }
    })
}

fn path_bits(level: u8) -> u32 {
    packing::path_bits(level)
}

/// Iceberg truncate width for partitioning level-`level` packed ids at `partition_level`.
pub fn truncate_width(level: u8, partition_level: u8) -> i64 {
    1i64 << (path_bits(level) - path_bits(partition_level) + 1)
}

struct Staged {
    domain: &'static Domain,
    level: u8,
    rows: i64,
    columns: Vec<(String, String)>,
    /// Identity columns that were not in the query and were derived.
    derived: Vec<&'static str>,
}

impl Staged {
    fn has(&self, n: &str) -> bool {
        self.columns.iter().any(|(c, _)| c == n)
    }
}

/// `rollup_level := R`: the input cells aggregated to their level-R ancestors, one row per
/// ancestor. Each numeric column is aggregated by its `aggregation_rule` (one word or
/// `{column: rule}`: sum, mean, min, max, median); without a rule COUNT columns (`value_semantics`)
/// are summed and the others averaged. An input `coverage_ratio` / `length_ratio` column
/// (e.g. from query_cell_dataset) weights the cells and is not written.
fn rollup_query(lease: &Lease, query: &str, r: i64, o: &Options) -> Result<String, E> {
    if !(0..=codec::MAX_LEVEL as i64).contains(&r) {
        return Err(format!(
            "rollup_level must be in [0, {}]; received {r}",
            codec::MAX_LEVEL
        )
        .into());
    }
    let src = format!("SELECT * FROM ({query}) AS gs_src");
    let cols = describe(lease, &src)?;
    let has = |n: &str| cols.iter().any(|(c, _)| c == n);
    let gid = if has("gid") {
        "CAST(r.gid AS VARCHAR)"
    } else if has("packed_id") {
        "packed_to_gid(CAST(r.packed_id AS BIGINT))"
    } else {
        return Err("the query needs a gid or a packed_id column".into());
    };
    let weight = ["coverage_ratio", "length_ratio"]
        .into_iter()
        .find(|w| has(w));
    let w = weight.map_or("1.0".to_string(), |w| format!("r.{w}"));
    type Rules = (Option<String>, serde_json::Map<String, Json>);
    let word_or_map = |text: &Option<String>| -> Result<Rules, E> {
        match text.as_deref().map(str::trim) {
            Some(t) if t.starts_with('{') => match serde_json::from_str::<Json>(t) {
                Ok(Json::Object(m)) => Ok((None, m)),
                _ => Err("aggregation_rule / value_semantics must be a word or a JSON object {column: value}".into()),
            },
            Some(t) => Ok((Some(t.to_string()), Default::default())),
            None => Ok((None, Default::default())),
        }
    };
    let space_agg =
        crate::metadata::PerColumn::parse(o.space_aggregation.clone(), "space_aggregation")?;
    let space_weight = crate::metadata::PerColumn::parse(o.space_weight.clone(), "space_weight")?;
    let (sem_all, sem_map) = word_or_map(&Some(o.value_semantics.clone()))?;
    let skip = [
        "gid",
        "packed_id",
        "x_idx",
        "y_idx",
        "uri",
        "domain_code",
        "level",
        "cell_count",
    ];
    let mut aggs = Vec::new();
    for (c, ty) in &cols {
        if skip.contains(&c.as_str()) || Some(c.as_str()) == weight {
            continue;
        }
        if !is_numeric_type(ty) {
            return Err(format!(
                "rollup_level aggregates numeric columns; {c} is {ty}: aggregate or drop it in the query"
            )
            .into());
        }
        let sem = sem_map
            .get(c)
            .and_then(Json::as_str)
            .map(str::to_string)
            .or_else(|| sem_all.clone())
            .unwrap_or_default();
        // The parent rule and weight come from space_aggregation / space_weight (spec 2.0,
        // dataset metadata section 4), not from aggregation_rule, which is the source-to-cell meaning.
        let rule = space_agg.get(c).map(str::to_string);
        let space_w = space_weight.get(c).map(str::to_string);
        let Some((rule, space_w)) = crate::dataset::space_rule(c, &sem, rule, space_w)? else {
            continue; // `none`: the column is not rolled up
        };
        aggs.push(format!(
            "{} AS {}",
            crate::dataset::space_expr(c, &rule, space_w.as_deref(), &w)?,
            ident(c)
        ));
    }
    if aggs.is_empty() {
        return Err("rollup_level: the query has no value columns".into());
    }
    // Keep the domain with the cells: the parent's packed_id from the input's domain bits.
    let packed = if has("packed_id") {
        format!(
            ", pack(CAST(any_value(r.packed_id >> 54) AS INTEGER), left({gid}, {r})) AS packed_id"
        )
    } else {
        String::new()
    };
    Ok(format!(
        "SELECT left({gid}, {r}) AS gid{packed}, {} FROM ({src}) AS r GROUP BY 1",
        aggs.join(", ")
    ))
}

/// compact_cell_dataset(path, partition_level := <current>): rewrite a dataset in place with
/// one sorted file per partition (merging the files of many appends), optionally at another
/// partition level, keeping its metadata (value fields with units and rules, source,
/// coverage, notes, title, description, period, version, column descriptions). Iceberg: one
/// new snapshot; run expire_snapshots afterwards to delete the old files.
pub fn plan_compact(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?;
    let partition_level = p.int("partition_level");
    let s3 = s3_params(p);
    Ok(Plan::Action(
        Box::new(move || compact(&path, partition_level, &s3)),
        ["layout", "location", "files", "rows"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    ))
}

/// The partition level a dataset is written with (from the manifest's storage.partitioning).
fn current_partition_level(manifest: &Json) -> Option<i64> {
    let level = manifest.get("level").and_then(Json::as_i64)? as u8;
    for p in manifest
        .pointer("/storage/partitioning")
        .and_then(Json::as_array)?
        .iter()
        .filter_map(Json::as_str)
    {
        if let Some(l) = p.strip_prefix("gid_prefix:") {
            return l.parse().ok();
        }
        if let Some(w) = p
            .strip_prefix("iceberg:truncate(")
            .and_then(|r| r.split(',').next())
            .and_then(|w| w.parse::<i64>().ok())
        {
            return (1..=level)
                .find(|pl| truncate_width(level, *pl) == w)
                .map(i64::from);
        }
    }
    None
}

fn compact(
    path: &str,
    partition_level: Option<i64>,
    s3: &S3Params,
) -> Result<Vec<Vec<Option<String>>>, E> {
    let lease = sql::lease()?;
    let root = path.trim_end_matches('/').to_string();
    let src = dataset::open(&lease, &root)?;
    let manifest = src
        .manifest
        .clone()
        .ok_or("compact_cell_dataset needs a dataset with a cell manifest")?;
    let info = dataset::dataset_info(&lease, &src)?;
    let layout = match src.layout {
        dataset::Layout::Flat => "flat",
        dataset::Layout::Partitioned(_) => "partitioned",
        dataset::Layout::Iceberg { .. } => "iceberg",
    };
    let text = |v: Option<&Json>| v.and_then(Json::as_str).map(str::to_string);
    let descriptions: serde_json::Map<String, Json> = info
        .get("columns")
        .and_then(Json::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(k, v)| {
            v.get("description")
                .filter(|d| d.is_string())
                .map(|d| (k.clone(), d.clone()))
        })
        .collect();
    let object = |v: Option<&Json>| v.and_then(Json::as_object).cloned();
    let fields = manifest
        .get("value_fields")
        .cloned()
        .unwrap_or_else(|| json!([]));
    let o = Options {
        layout: layout.into(),
        mode: "overwrite".into(),
        domain_code: text(manifest.get("domain_code")),
        level: None,
        partition_level: partition_level.or_else(|| current_partition_level(&manifest)),
        row_group_size: 8192,
        value_semantics: "MEASUREMENT".into(),
        aggregation_rule: None,
        value_fields: Some(fields.to_string()),
        notes: text(manifest.pointer("/provenance/notes")),
        identity: if manifest
            .pointer("/identity/packed_id_column")
            .is_some_and(Json::is_null)
        {
            "compact".into()
        } else {
            "query".into()
        },
        column_metrics: if fields.as_array().map_or(0, Vec::len) > 96 {
            "key".into()
        } else {
            "auto".into()
        },
        extra: crate::metadata::Extra {
            descriptions: crate::metadata::PerColumn::parse(
                (!descriptions.is_empty()).then(|| Json::Object(descriptions).to_string()),
                "column_descriptions",
            )?,
            title: text(info.get("title")),
            description: text(info.get("description")),
            data_period: text(info.get("data_period")),
            data_version: text(info.get("data_version")),
            source: object(manifest.get("source")),
            coverage: object(manifest.get("coverage")),
            ..Default::default()
        },
        fill_nulls: true,
        rollup_level: None,
        space_aggregation: None,
        space_weight: None,
        value_semantics_given: false,
        prev: None,
        manifest_version: None,
        version: "1.0".into(),
        gov: Default::default(),
        s3: s3.clone(),
    };
    let query = format!("SELECT * FROM read_cell_dataset({})", lit(&root));
    write(&query, &root, &o)
}

/// Stage the query sorted by packed_id and run the identity checks.
fn stage(lease: &Lease, query: &str, o: &Options) -> Result<Staged, E> {
    let stage = stage_table();
    sql::exec_on(
        lease,
        &format!("ATTACH IF NOT EXISTS ':memory:' AS {STAGE_DB}"),
    )?;
    let src = format!("SELECT * FROM ({query}) AS gs_src");
    let cols = describe(lease, &src)?;
    let has = |n: &str| cols.iter().any(|(c, _)| c == n);
    if !has("gid") && !has("packed_id") {
        return Err("the query needs a gid or a packed_id column".into());
    }
    if let Some((c, _)) = cols.iter().find(|(c, _)| RESERVED.contains(&c.as_str())) {
        return Err(format!("column {c:?} is a reserved cell-dataset name; rename it").into());
    }
    let domain = match &o.domain_code {
        Some(c) => domain_by_code(c)?,
        None if has("packed_id") => {
            let r = one_row(
                lease,
                &format!("SELECT min(packed_id >> 54), max(packed_id >> 54) FROM ({src})"),
            )?;
            if r[0] != r[1] {
                return Err(
                    "packed_id values come from more than one domain; one dataset holds one domain"
                        .into(),
                );
            }
            crate::grid::domain_by_id(num(&r[0]) as i32)?
        }
        None => {
            return Err("pass domain_code := '<code>' (the query has gid but no packed_id)".into())
        }
    };
    let code = lit(domain.code);
    let packed = if has("packed_id") {
        "CAST(packed_id AS BIGINT)".to_string()
    } else {
        format!("pack({code}, gid)")
    };
    let gid = if has("gid") {
        "CAST(gid AS VARCHAR)".to_string()
    } else {
        "canonical_to_gid(CAST((packed_id >> 50) & 15 AS INTEGER), unpack_int64(packed_id).x_idx, unpack_int64(packed_id).y_idx)".to_string()
    };
    let mut select = vec![format!("{gid} AS gid"), format!("{packed} AS packed_id")];
    let mut derived = Vec::new();
    if !has("gid") {
        derived.push("gid");
    }
    if !has("packed_id") {
        derived.push("packed_id");
    }
    for axis in ["x_idx", "y_idx"] {
        if !has(axis) {
            select.push(format!("unpack_int64({packed}).{axis} AS {axis}"));
            derived.push(axis);
        }
    }
    for (c, ty) in &cols {
        if c == "gid" || c == "packed_id" {
            continue;
        }
        // A rollup value stays NULL where no child has data (median: fewer than 3 children).
        // Filling it with 0 would make that gap look like a measured zero.
        if o.fill_nulls
            && o.rollup_level.is_none()
            && !IDENTITY.contains(&c.as_str())
            && is_numeric_type(ty)
        {
            select.push(format!("coalesce({0}, CAST(0 AS {ty})) AS {0}", ident(c)));
        } else {
            select.push(ident(c));
        }
    }
    sql::exec_on(lease, &format!("DROP TABLE IF EXISTS {stage}"))?;
    sql::exec_on(
        lease,
        &format!(
            "CREATE TABLE {stage} AS SELECT {} FROM ({src}) ORDER BY packed_id",
            select.join(", ")
        ),
    )?;

    let check_pack = if has("gid") && has("packed_id") {
        format!("count(*) FILTER (WHERE pack({code}, gid) <> packed_id)")
    } else {
        "0".into()
    };
    let check_xy = if has("x_idx") || has("y_idx") {
        let mut parts = Vec::new();
        if has("x_idx") {
            parts.push("x_idx <> unpack_int64(packed_id).x_idx");
        }
        if has("y_idx") {
            parts.push("y_idx <> unpack_int64(packed_id).y_idx");
        }
        format!("count(*) FILTER (WHERE {})", parts.join(" OR "))
    } else {
        "0".into()
    };
    let r = one_row(
        lease,
        &format!(
            "SELECT count(*), count(DISTINCT packed_id), min((packed_id >> 50) & 15), max((packed_id >> 50) & 15), \
             min(packed_id >> 54), max(packed_id >> 54), {check_pack}, {check_xy}, count(*) FILTER (WHERE packed_id IS NULL OR gid IS NULL) FROM {stage}"
        ),
    )?;
    let (n, distinct) = (num(&r[0]), num(&r[1]));
    if n == 0 {
        return Err("the query returned no rows".into());
    }
    if num(&r[8]) > 0 {
        return Err("gid/packed_id must not be NULL".into());
    }
    if num(&r[2]) != num(&r[3]) {
        return Err("cells come from more than one level; one dataset holds one level".into());
    }
    if num(&r[4]) != num(&r[5]) || num(&r[4]) != domain.id as i64 {
        return Err(format!("packed_id domain bits do not match domain {}", domain.code).into());
    }
    if num(&r[6]) > 0 {
        return Err(format!(
            "{} rows have a gid that does not match packed_id",
            num(&r[6])
        )
        .into());
    }
    if num(&r[7]) > 0 {
        return Err(format!(
            "{} rows have x_idx/y_idx that do not match the cell",
            num(&r[7])
        )
        .into());
    }
    if distinct != n {
        return Err(format!(
            "{} duplicate cells: a cell_values dataset needs one row per cell (aggregate first)",
            n - distinct
        )
        .into());
    }
    let level = level_u8(num(&r[2]) as i32)?;
    if let Some(l) = o.level {
        if l != level as i64 {
            return Err(format!("level := {l} but the cells are level {level}").into());
        }
    }
    Ok(Staged {
        domain,
        level,
        rows: n,
        columns: describe(lease, &format!("SELECT * FROM {stage}"))?,
        derived,
    })
}

/// The value fields of the manifest: the `value_fields` parameter, else one per column.
fn value_fields(st: &Staged, o: &Options) -> Result<Json, E> {
    // value_semantics / aggregation_rule: one value for every numeric column, or a JSON
    // object per column, e.g. {"population": "COUNT", "score": "MEASUREMENT"}.
    let per_column =
        |text: &Option<String>, what: &str| -> Result<Option<serde_json::Map<String, Json>>, E> {
            match text.as_deref().map(str::trim) {
                Some(t) if t.starts_with('{') => match serde_json::from_str::<Json>(t) {
                    Ok(Json::Object(m)) => Ok(Some(m)),
                    _ => Err(
                        format!("{what} must be a word or a JSON object {{column: value}}").into(),
                    ),
                },
                _ => Ok(None),
            }
        };
    let sem_map = per_column(&Some(o.value_semantics.clone()), "value_semantics")?;
    let agg_map = per_column(&o.aggregation_rule, "aggregation_rule")?;
    let sem_for = |c: &str| -> Json {
        match &sem_map {
            Some(m) => m.get(c).cloned().unwrap_or(json!("MEASUREMENT")),
            None => json!(o.value_semantics),
        }
    };
    let agg_for = |c: &str| -> Json {
        match &agg_map {
            Some(m) => m.get(c).cloned().unwrap_or(Json::Null),
            None => json!(o.aggregation_rule),
        }
    };
    let value_fields: Json = match &o.value_fields {
        Some(text) => serde_json::from_str(text).map_err(|e| format!("value_fields is not valid JSON: {e}"))?,
        None => Json::Array(
            st.columns
                .iter()
                .filter(|(c, _)| !IDENTITY.contains(&c.as_str()) && c != "uri")
                .map(|(c, t)| {
                    let numeric = is_numeric(t);
                    json!({
                        "name": c,
                        "parquet_type": parquet_type(t),
                        "logical_type": if numeric { "numeric" } else { "categorical" },
                        "semantics": if numeric { sem_for(c) } else { json!("CATEGORICAL") },
                        "unit": null,
                        "nullable": true,
                        "aggregation_rule": if numeric { agg_for(c) } else { agg_map.as_ref().and_then(|m| m.get(c.as_str()).cloned()).unwrap_or(Json::Null) },
                    })
                })
                .collect(),
        ),
    };
    if value_fields.as_array().is_none_or(|a| a.is_empty()) {
        return Err(
            "the dataset needs at least one value column besides gid/packed_id/x_idx/y_idx".into(),
        );
    }
    Ok(value_fields)
}

/// The cell-dataset manifest: manifest 1.0, or 1.1 when `facts` is given.
fn manifest(
    st: &Staged,
    o: &Options,
    value_fields: Json,
    partitioning: Vec<String>,
    row_count: i64,
    facts: Option<&manifest11::Facts>,
) -> Result<Json, E> {
    let query_profile =
        o.identity == "query" && st.has("packed_id") && st.has("x_idx") && st.has("y_idx");
    let col = |n: &str| if query_profile { json!(n) } else { Json::Null };
    let per_column_aggregation = o
        .aggregation_rule
        .as_deref()
        .is_some_and(|t| t.trim_start().starts_with('{'));
    let parse = |name: &str, text: &Option<String>, default: Json| -> Result<Json, E> {
        match text {
            Some(t) => {
                serde_json::from_str(t).map_err(|e| format!("{name} is not valid JSON: {e}").into())
            }
            None => Ok(default),
        }
    };
    let semantics = if !o.value_semantics.trim_start().starts_with('{')
        && is_numeric_semantics(&o.value_semantics)
    {
        json!(o.value_semantics)
    } else {
        Json::Null
    };
    let mut m = json!({
        "manifest_version": "1.0",
        "dataset_type": "cell_values",
        "grid_system": "geosquare",
        "grid_version": "v2",
        "domain_code": st.domain.code,
        "domain_id": st.domain.id,
        "level": st.level,
        "profile_version": geosquare_core::domain::PROFILE_VERSION,
        "registry_version": geosquare_core::domain::REGISTRY_VERSION,
        "value_fields": value_fields,
        "identity": {
            "gid_column": "gid",
            "uri_prefix": format!("geosquare:v2:{}:", st.domain.code),
            "packed_id_column": col("packed_id"),
            "x_idx_column": col("x_idx"),
            "y_idx_column": col("y_idx"),
            "unique_gid": true
        },
        "coverage": parse("coverage", &None, json!({
            "assignment_method": "unspecified", "boundary_policy": null, "boundary_sha256": null,
            "coverage_mode": null, "min_coverage": null, "length_mode": null}))?,
        "source": parse("source", &None, json!({
            "format": "duckdb_query", "geometry_type": null, "source_crs": null,
            "source_id_column": null, "source_dataset": null, "source_row_count": null}))?,
        "storage": {
            "parquet_compression": "zstd",
            "geometry_column": null, "geometry_format": null, "geometry_crs": null,
            "partitioning": partitioning,
            "sorted_by": ["packed_id"],
            "row_count": row_count,
            "unique_cell_count": row_count
        },
        "provenance": {
            "created_at": iso_now(),
            "created_by": format!("geosquare_v2 duckdb extension {}", env!("CARGO_PKG_VERSION")),
            "generator": "write_cell_dataset",
            "aggregation_rule": if per_column_aggregation { Json::Null } else { json!(o.aggregation_rule) },
            "value_semantics": semantics,
            "notes": o.notes
        }
    });
    if let Some((prev, _)) = &o.prev {
        inherit(&mut m, prev, o);
    }
    crate::metadata::apply(&mut m, &o.extra)?;
    if let Some(f) = facts {
        let prev = o.prev.as_ref().map(|(p, _)| p);
        let governance = manifest11::governance(&o.gov, o.extra.title.as_deref(), prev);
        let derived = manifest11::derived_from(&o.gov, prev)?;
        manifest11::apply(&mut m, f, governance, derived);
    }
    Ok(m)
}

/// Keep the metadata of the dataset being appended to or overwritten: per column (same name)
/// unit, semantics and aggregation rule; source, coverage, notes and the provenance rules.
/// What this write passes explicitly wins (units/source/coverage are applied afterwards).
fn inherit(m: &mut Json, prev: &Json, o: &Options) {
    if o.value_fields.is_none() {
        let old: std::collections::HashMap<&str, &Json> = prev
            .get("value_fields")
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|f| Some((f.get("name")?.as_str()?, f)))
            .collect();
        if let Some(fields) = m.get_mut("value_fields").and_then(Json::as_array_mut) {
            for f in fields {
                let name = f
                    .get("name")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string();
                let Some(p) = old.get(name.as_str()) else {
                    continue;
                };
                let mut keys = vec!["unit"];
                if !o.value_semantics_given {
                    keys.push("semantics");
                }
                if o.aggregation_rule.is_none() {
                    keys.push("aggregation_rule");
                }
                for k in keys {
                    if let Some(v) = p.get(k) {
                        f[k] = v.clone();
                    }
                }
            }
        }
    }
    for section in ["source", "coverage"] {
        if let Some(v) = prev.get(section).filter(|v| v.is_object()) {
            m[section] = v.clone();
        }
    }
    let pv = |k: &str| prev.pointer(&format!("/provenance/{k}")).cloned();
    if o.notes.is_none() {
        if let Some(v) = pv("notes") {
            m["provenance"]["notes"] = v;
        }
    }
    if o.aggregation_rule.is_none() {
        if let Some(v) = pv("aggregation_rule") {
            m["provenance"]["aggregation_rule"] = v;
        }
    }
    if !o.value_semantics_given {
        if let Some(v) = pv("value_semantics") {
            m["provenance"]["value_semantics"] = v;
        }
    }
}

/// DuckDB numeric column types (as DESCRIBE prints them).
fn is_numeric_type(ty: &str) -> bool {
    let t = ty.to_ascii_uppercase();
    matches!(
        t.as_str(),
        "TINYINT"
            | "SMALLINT"
            | "INTEGER"
            | "BIGINT"
            | "HUGEINT"
            | "UTINYINT"
            | "USMALLINT"
            | "UINTEGER"
            | "UBIGINT"
            | "UHUGEINT"
            | "FLOAT"
            | "DOUBLE"
    ) || t.starts_with("DECIMAL")
}

fn is_numeric_semantics(s: &str) -> bool {
    matches!(
        s,
        "COUNT"
            | "TOTAL"
            | "DENSITY"
            | "MEASUREMENT"
            | "RATE"
            | "ORDINAL"
            | "CATEGORICAL"
            | "RANGE"
    )
}

fn parquet_options(o: &Options, manifest: &Json, extra: &str) -> String {
    format!(
        "FORMAT parquet, COMPRESSION zstd, ROW_GROUP_SIZE {}, KV_METADATA {{{}: {}}}{extra}",
        o.row_group_size,
        lit(MANIFEST_KEY),
        lit(&manifest.to_string())
    )
}

/// Write `manifest` as `<root>/manifest.json` through DuckDB (works for any filesystem).
fn write_manifest(lease: &Lease, root: &str, manifest: &Json) -> Result<(), E> {
    write_json(lease, &join(root, MANIFEST_FILE), manifest)
}

/// Write one JSON object as a file through DuckDB (any filesystem DuckDB can write).
fn write_json(lease: &Lease, path: &str, doc: &Json) -> Result<(), E> {
    let obj = doc.as_object().ok_or("document must be a JSON object")?;
    let cols: Vec<String> = obj
        .iter()
        .map(|(k, v)| format!("{}::JSON AS {}", lit(&v.to_string()), ident(k)))
        .collect();
    sql::exec_on(
        lease,
        &format!(
            "COPY (SELECT {}) TO {} (FORMAT json)",
            cols.join(", "),
            lit(path)
        ),
    )
}

/// Refuse or clear an existing target according to `mode` (local or S3).
fn prepare_target(lease: &Lease, root: &str, o: &Options, store: &Storage) -> Result<(), E> {
    let exists = !sql::query_text_on(
        lease,
        &format!("SELECT file FROM glob({}) LIMIT 1", lit(&join(root, "*"))),
    )?
    .is_empty()
        || !sql::query_text_on(
            lease,
            &format!(
                "SELECT file FROM glob({}) LIMIT 1",
                lit(&join(root, "data/*"))
            ),
        )?
        .is_empty();
    if !exists {
        return Ok(());
    }
    match o.mode.as_str() {
        "create" => {
            Err(format!("{root} already exists; use mode := 'overwrite' or a new path").into())
        }
        "overwrite" => {
            for sub in ["data", "metadata", MANIFEST_FILE, PREFIX_INDEX_FILE] {
                store.delete_prefix(&join(root, sub))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn write(query: &str, path: &str, o: &Options) -> Result<Vec<Vec<Option<String>>>, E> {
    let lease = sql::lease()?;
    let stage_name = format!("{STAGE_DB}.main.\"w_{}\"", uuid4().replace('-', ""));
    STAGE_NAME.with(|s| *s.borrow_mut() = stage_name.clone());
    let root = path.trim_end_matches('/').to_string();
    let result = (|| -> Result<Vec<Vec<Option<String>>>, E> {
        let store = Storage::for_path(&lease, &root, &o.s3)?;
        // Metadata of the dataset being appended to or overwritten, kept by default.
        let mut o = o.clone();
        if o.mode != "create" {
            if let Ok(src) = dataset::open(&lease, &root) {
                if let Some(m) = src.manifest.clone() {
                    let info = dataset::dataset_info(&lease, &src).unwrap_or(Json::Null);
                    o.prev = Some((m, info));
                }
            }
        }
        o.version = resolve_version(&o, &root)?;
        let o = &o;
        // Stage (read the whole input) before clearing the target: the input may read the
        // dataset being overwritten (update / delete by rewriting it).
        let rolled;
        let query = match o.rollup_level {
            Some(r) => {
                rolled = rollup_query(&lease, query, r, o)?;
                rolled.as_str()
            }
            None => query,
        };
        let st = stage(&lease, query, o)?;
        if o.is_v11() {
            // Refuse bad rules, types, domains and values before the target is cleared.
            if !st.domain.crs_released() {
                return Err(format!(
                    "manifest 1.1 needs a released domain: {} is not released for lon/lat",
                    st.domain.code
                )
                .into());
            }
            let vf = value_fields(&st, o)?;
            manifest11::resolve_fields(&field_inputs(&vf, o)?)?;
            check_finite(&lease, &st)?;
        }
        if o.layout != "iceberg" {
            prepare_target(&lease, &root, o, &store)?;
        }
        match o.layout.as_str() {
            "flat" => write_flat(&lease, &root, &st, o),
            "partitioned" => write_partitioned(&lease, &root, &st, o, &store),
            _ => write_iceberg(&lease, &root, &st, o, &store),
        }
    })();
    let _ = sql::exec_on(&lease, &format!("DROP TABLE IF EXISTS {stage_name}"));
    result
}

impl Options {
    /// Whether this write produces manifest 1.1.
    fn is_v11(&self) -> bool {
        self.version == manifest11::VERSION
    }
}

/// Refuse NaN and infinity in float value columns. Manifest 1.1 records their statistics and
/// hashes, which cannot represent them. Runs before the target is cleared.
fn check_finite(lease: &Lease, st: &Staged) -> Result<(), E> {
    let floats: Vec<&str> = st
        .columns
        .iter()
        .filter(|(c, t)| {
            !IDENTITY.contains(&c.as_str())
                && c != "uri"
                && matches!(t.to_ascii_uppercase().as_str(), "FLOAT" | "DOUBLE")
        })
        .map(|(c, _)| c.as_str())
        .collect();
    if floats.is_empty() {
        return Ok(());
    }
    let counts: Vec<String> = floats
        .iter()
        .map(|c| {
            let col = ident(c);
            format!("count(*) FILTER (WHERE isnan({col}) OR isinf({col}))")
        })
        .collect();
    let r = one_row(
        lease,
        &format!("SELECT {} FROM {}", counts.join(", "), stage_table()),
    )?;
    for (name, n) in floats.iter().zip(&r) {
        if num(n) > 0 {
            return Err(format!(
                "column {name}: NaN or infinity cannot be recorded in manifest 1.1; set those values to NULL first"
            )
            .into());
        }
    }
    Ok(())
}

/// The manifest version of a dataset, from its manifest (1.0 when it names none).
fn manifest_version_of(m: &Json) -> String {
    m.get("manifest_version")
        .and_then(Json::as_str)
        .unwrap_or("1.0")
        .to_string()
}

/// The manifest version a write produces: the `manifest_version` parameter, else the version of
/// the dataset being appended to or overwritten, else 1.0. Refuses the combinations manifest 1.1
/// does not support yet: upsert, and appending to or from a table of the other version.
fn resolve_version(o: &Options, root: &str) -> Result<String, E> {
    let prev = o.prev.as_ref().map(|(m, _)| manifest_version_of(m));
    let version = o
        .manifest_version
        .clone()
        .or_else(|| prev.clone())
        .unwrap_or_else(|| "1.0".into());
    if version != "1.1"
        && o.rollup_level.is_none()
        && (o.space_aggregation.is_some() || o.space_weight.is_some())
    {
        return Err(
            "space_aggregation and space_weight need rollup_level (or manifest_version := '1.1')"
                .into(),
        );
    }
    match (version.as_str(), o.mode.as_str(), prev.as_deref()) {
        ("1.1", "upsert", _) => Err(format!(
            "{root}: mode 'upsert' is not available for manifest 1.1 yet; use mode := 'append' or 'overwrite'"
        )
        .into()),
        ("1.1", "append", Some("1.0")) => Err(format!(
            "{root} is manifest 1.0: upgrade it with mode := 'overwrite' and manifest_version := '1.1'"
        )
        .into()),
        ("1.0", "append" | "upsert", Some("1.1")) => Err(format!(
            "{root} is manifest 1.1: appending as 1.0 would drop its hashes and statistics; keep \
             manifest_version 1.1, or overwrite"
        )
        .into()),
        _ => Ok(version),
    }
}

/// The inputs manifest 1.1 needs for each value field: its type and semantics, and the rule and
/// weight. A parameter wins over the value stored in the manifest the field came from.
fn field_inputs<'a>(vf: &'a Json, o: &Options) -> Result<Vec<FieldIn<'a>>, E> {
    let rules = PerColumn::parse(o.space_aggregation.clone(), "space_aggregation")?;
    let weights = PerColumn::parse(o.space_weight.clone(), "space_weight")?;
    // Like the other metadata, the rule and weight of a field being appended to or overwritten
    // are kept unless this write gives them.
    let kept = |name: &str, key: &str| -> Option<String> {
        o.prev
            .as_ref()?
            .0
            .get("value_fields")?
            .as_array()?
            .iter()
            .find(|f| f.get("name").and_then(Json::as_str) == Some(name))?
            .get(key)?
            .as_str()
            .map(str::to_string)
    };
    let fields: &[Json] = vf.as_array().map(Vec::as_slice).unwrap_or(&[]);
    Ok(fields
        .iter()
        .map(|f| {
            let name = f.get("name").and_then(Json::as_str).unwrap_or("");
            let text = |key: &str| f.get(key).and_then(Json::as_str).map(str::to_string);
            FieldIn {
                name,
                parquet_type: f.get("parquet_type").and_then(Json::as_str).unwrap_or(""),
                numeric: f.get("logical_type").and_then(Json::as_str) == Some("numeric"),
                semantics: if o.value_semantics_given {
                    text("semantics").unwrap_or_else(|| "CATEGORICAL".into())
                } else {
                    kept(name, "semantics")
                        .or_else(|| text("semantics"))
                        .unwrap_or_else(|| "CATEGORICAL".into())
                },
                rule: rules
                    .get(name)
                    .map(str::to_string)
                    .or_else(|| text("space_aggregation"))
                    .or_else(|| kept(name, "space_aggregation")),
                weight: weights
                    .get(name)
                    .map(str::to_string)
                    .or_else(|| text("space_weight"))
                    .or_else(|| kept(name, "space_weight")),
            }
        })
        .collect())
}

/// The manifest 1.1 facts of this write, or None for manifest 1.0. `files` are the data files
/// the write creates. When appending, the dataset's manifest is the base the facts extend.
fn facts11(
    lease: &Lease,
    st: &Staged,
    o: &Options,
    vf: &Json,
    files: Vec<FileSpec>,
) -> Result<Option<manifest11::Facts>, E> {
    if !o.is_v11() {
        return Ok(None);
    }
    let fields = field_inputs(vf, o)?;
    let base = o
        .prev
        .as_ref()
        .map(|(m, _)| m)
        .filter(|_| o.mode == "append");
    manifest11::facts(
        lease,
        &stage_table(),
        st.domain,
        &fields,
        files,
        base,
        write_threads(),
    )
    .map(Some)
}

/// Columns written to contract folders: everything, or gid + values for 'compact'.
fn folder_columns(st: &Staged, o: &Options) -> String {
    st.columns
        .iter()
        .filter(|(c, _)| {
            o.identity == "query" || !matches!(c.as_str(), "packed_id" | "x_idx" | "y_idx")
        })
        .map(|(c, _)| ident(c))
        .collect::<Vec<_>>()
        .join(", ")
}

fn write_flat(
    lease: &Lease,
    root: &str,
    st: &Staged,
    o: &Options,
) -> Result<Vec<Vec<Option<String>>>, E> {
    let stage = stage_table();
    let vf = value_fields(st, o)?;
    let file = FileSpec {
        path: "data/part-00000.parquet".into(),
        predicate: "TRUE".into(),
    };
    let facts = facts11(lease, st, o, &vf, vec![file])?;
    let m = manifest(st, o, vf, vec![], st.rows, facts.as_ref())?;
    ensure_dir(&join(root, "data"))?;
    sql::exec_on(
        lease,
        &format!(
            "COPY (SELECT {} FROM {stage} ORDER BY packed_id) TO {} ({})",
            folder_columns(st, o),
            lit(&join(root, "data/part-00000.parquet")),
            parquet_options(o, &m, "")
        ),
    )?;
    write_manifest(lease, root, &m)?;
    write_json(
        lease,
        &join(root, crate::metadata::INFO_FILE),
        &crate::metadata::info(o.prev.as_ref().map(|(_, i)| i), &m, &o.extra),
    )?;
    Ok(vec![vec![
        Some("flat".into()),
        Some(root.to_string()),
        Some("1".into()),
        Some(st.rows.to_string()),
    ]])
}

fn partition_level(st: &Staged, o: &Options) -> Result<u8, E> {
    let l = o.partition_level.unwrap_or(5);
    let l = level_u8(l as i32)?;
    if l == 0 || l > st.level {
        return Err(format!(
            "partition_level must be in [1, {}] (the dataset level)",
            st.level
        )
        .into());
    }
    Ok(l)
}

fn write_partitioned(
    lease: &Lease,
    root: &str,
    st: &Staged,
    o: &Options,
    store: &Storage,
) -> Result<Vec<Vec<Option<String>>>, E> {
    let stage = stage_table();
    let mut staging = Staging::new(root)?;
    let l = partition_level(st, o)?;
    let prefixes: Vec<String> = rows(
        lease,
        &format!("SELECT DISTINCT left(gid, {l}) FROM {stage} ORDER BY 1"),
    )?
    .into_iter()
    .filter_map(|r| r.into_iter().next().flatten())
    .collect();
    let mut jobs = Vec::with_capacity(prefixes.len());
    let mut specs = Vec::with_capacity(prefixes.len());
    for p in &prefixes {
        let (lo, hi) = topology::descendant_range(st.domain.id, codec::decode(p)?, st.level)?;
        let rel = format!("data/gid_prefix={p}/part-00000.parquet");
        ensure_dir(&join(root, &format!("data/gid_prefix={p}")))?;
        specs.push(FileSpec {
            path: rel.clone(),
            predicate: format!("packed_id BETWEEN {lo} AND {hi}"),
        });
        jobs.push((lo, hi, staging.path(&join(root, &rel), &rel)?));
    }
    let vf = value_fields(st, o)?;
    let facts = facts11(lease, st, o, &vf, specs)?;
    let m = manifest(
        st,
        o,
        vf,
        vec![
            "domain_code".into(),
            "level".into(),
            format!("gid_prefix:{l}"),
        ],
        st.rows,
        facts.as_ref(),
    )?;
    let (columns, options) = (folder_columns(st, o), parquet_options(o, &m, ""));
    sql::par_map(jobs, write_threads(), |l, (lo, hi, target)| {
        sql::exec_on(
            l,
            &format!(
                "COPY (SELECT {columns} FROM {stage} WHERE packed_id BETWEEN {lo} AND {hi} ORDER BY packed_id) TO {} ({options})",
                lit(&target)
            ),
        )
    })?;
    staging.upload(store)?;
    write_manifest(lease, root, &m)?;
    write_json(
        lease,
        &join(root, crate::metadata::INFO_FILE),
        &crate::metadata::info(o.prev.as_ref().map(|(_, i)| i), &m, &o.extra),
    )?;
    // Partition index for readers that cannot list folders (plain HTTP).
    let list = serde_json::to_string(&prefixes)?;
    sql::exec_on(
        lease,
        &format!(
            "COPY (SELECT {}::JSON AS gid_prefixes) TO {} (FORMAT json)",
            lit(&list),
            lit(&join(root, PREFIX_INDEX_FILE))
        ),
    )?;
    Ok(vec![vec![
        Some("partitioned".into()),
        Some(root.to_string()),
        Some(prefixes.len().to_string()),
        Some(st.rows.to_string()),
    ]])
}

// ---------------------------------------------------------------------------
// Iceberg
// ---------------------------------------------------------------------------

fn current_snapshot(meta: &Json) -> Option<&Json> {
    let sid = meta.get("current-snapshot-id")?.as_i64()?;
    meta.get("snapshots")?
        .as_array()?
        .iter()
        .find(|s| s.get("snapshot-id").and_then(Json::as_i64) == Some(sid))
}

fn summary_i64(meta: &Json, key: &str) -> i64 {
    current_snapshot(meta)
        .and_then(|s| {
            s.pointer(&format!("/summary/{key}"))?
                .as_str()?
                .parse()
                .ok()
        })
        .unwrap_or(0)
}

fn schema_fields(schema: &Json) -> Vec<Field> {
    schema
        .get("fields")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .map(|f| Field {
                    id: f.get("id").and_then(Json::as_i64).unwrap_or(0),
                    name: f
                        .get("name")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .to_string(),
                    ty: f
                        .get("type")
                        .and_then(Json::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn by_id<'a>(meta: &'a Json, list: &str, key: &str, id: i64) -> Option<&'a Json> {
    meta.get(list)?
        .as_array()?
        .iter()
        .find(|s| s.get(key).and_then(Json::as_i64) == Some(id))
}

/// Strip `file://` so locations written by other tools compare with local roots.
fn plain(p: &str) -> &str {
    p.strip_prefix("file://").unwrap_or(p).trim_end_matches('/')
}

/// Highest metadata version number in `<root>/metadata`.
pub(crate) fn max_metadata_version(lease: &Lease, root: &str) -> Result<Option<u64>, E> {
    Ok(sql::query_text_on(
        lease,
        &format!(
            "SELECT file FROM glob({})",
            lit(&join(root, "metadata/*.metadata.json"))
        ),
    )?
    .into_iter()
    .filter_map(|r| r.into_iter().next().flatten())
    .filter_map(|f| {
        f.rsplit('/')
            .next()?
            .split(['-', '.'])
            .next()?
            .trim_start_matches('v')
            .parse::<u64>()
            .ok()
    })
    .max())
}

/// A DataFile (path, counts, partition, metrics) from a manifest entry's data_file record.
fn data_file_from_entry(df: &crate::avro::D) -> Option<DataFile> {
    use crate::avro::D;
    let path = match df.get("file_path")? {
        D::Str(s) => s.clone(),
        _ => return None,
    };
    let partition = df.get("partition")?.get(iceberg::PARTITION_FIELD)?.long()?;
    let longs = |n: &str| -> Vec<(i64, i64)> {
        match df.get(n) {
            Some(D::Arr(items)) => items
                .iter()
                .filter_map(|kv| Some((kv.get("key")?.long()?, kv.get("value")?.long()?)))
                .collect(),
            _ => Vec::new(),
        }
    };
    let bytes = |n: &str| -> Vec<(i64, Vec<u8>)> {
        match df.get(n) {
            Some(D::Arr(items)) => items
                .iter()
                .filter_map(|kv| match kv.get("value")? {
                    D::Bytes(b) => Some((kv.get("key")?.long()?, b.clone())),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    };
    Some(DataFile {
        path,
        record_count: df.get("record_count")?.long()?,
        size: df.get("file_size_in_bytes")?.long()?,
        partition,
        column_sizes: longs("column_sizes"),
        value_counts: longs("value_counts"),
        null_counts: longs("null_value_counts"),
        lower: bytes("lower_bounds"),
        upper: bytes("upper_bounds"),
    })
}

fn write_iceberg(
    lease: &Lease,
    root: &str,
    st: &Staged,
    o: &Options,
    store: &Storage,
) -> Result<Vec<Vec<Option<String>>>, E> {
    let stage = stage_table();
    // Version of the hint the commit must still find (conditional write at the end).
    let hint_path = join(root, "metadata/version-hint.text");
    let hint_tag = store.version_tag(&hint_path)?;
    let existing = dataset::latest_iceberg_metadata(lease, root).ok();
    if existing.is_some() && o.mode == "create" {
        return Err(format!(
            "{root} already has an Iceberg table; use mode := 'append' or 'overwrite'"
        )
        .into());
    }
    if existing.is_none()
        && !sql::query_text_on(
            lease,
            &format!("SELECT file FROM glob({}) LIMIT 1", lit(&join(root, "*"))),
        )?
        .is_empty()
    {
        return Err(format!("{root} exists but is not an Iceberg table").into());
    }
    let overwrite = o.mode == "overwrite" && existing.is_some();
    let prev = existing.as_ref().map(|(p, m)| (p.clone(), m.clone()));
    // A table copied away from where it was written: paths in its metadata start with the
    // old location. Read them under `root`, and record new files under `root`.
    let old_location = prev
        .as_ref()
        .and_then(|(_, m)| m.get("location")?.as_str())
        .map(|l| plain(l).to_string());
    let relocate = |p: &str| -> String {
        match &old_location {
            Some(old) if plain(p).starts_with(old.as_str()) && old != plain(root) => {
                format!("{}{}", root, &plain(p)[old.len()..])
            }
            _ => p.to_string(),
        }
    };
    let location = match &old_location {
        Some(old) if old == plain(root) => prev
            .as_ref()
            .and_then(|(_, m)| m.get("location")?.as_str())
            .unwrap_or(root)
            .trim_end_matches('/')
            .to_string(),
        _ => root.to_string(),
    };

    // Current schema and partition spec of the table, if any.
    let cur_schema_id = prev
        .as_ref()
        .and_then(|(_, m)| m.get("current-schema-id")?.as_i64())
        .unwrap_or(0);
    let cur_fields = prev
        .as_ref()
        .and_then(|(_, m)| by_id(m, "schemas", "schema-id", cur_schema_id))
        .map(schema_fields)
        .unwrap_or_default();
    let cur_names: Vec<String> = cur_fields.iter().map(|f| f.name.clone()).collect();

    // Columns to write, with Iceberg types. Derived identity columns are left out when
    // appending to a table written without them (for example by another tool).
    let mut wanted: Vec<(String, String, Option<&'static str>)> = Vec::new();
    for (c, t) in &st.columns {
        if prev.is_some()
            && !overwrite
            && st.derived.contains(&c.as_str())
            && !cur_names.contains(c)
        {
            continue;
        }
        let (ty, cast) = iceberg_type(t)?;
        wanted.push((c.clone(), ty, cast));
    }

    // append / upsert: numbers of another numeric type (e.g. a DECIMAL literal) are cast to
    // the table's column type instead of refusing the write.
    if prev.is_some() && !overwrite {
        let numeric =
            |t: &str| matches!(t, "double" | "float" | "long" | "int") || t.starts_with("decimal");
        let duck = |t: &str| -> Option<&'static str> {
            match t {
                "double" => Some("DOUBLE"),
                "float" => Some("FLOAT"),
                "long" => Some("BIGINT"),
                "int" => Some("INTEGER"),
                _ => None,
            }
        };
        for w in wanted.iter_mut() {
            if let Some(cf) = cur_fields.iter().find(|f| f.name == w.0) {
                if cf.ty != w.1 && numeric(&cf.ty) && numeric(&w.1) {
                    if let Some(d) = duck(&cf.ty) {
                        w.1 = cf.ty.clone();
                        w.2 = Some(d);
                    }
                }
            }
        }
    }

    // Field ids: reuse ids of unchanged columns; new or retyped columns get new ids.
    let last_column_id = prev
        .as_ref()
        .and_then(|(_, m)| m.get("last-column-id")?.as_i64())
        .unwrap_or(0);
    let mut next_id = last_column_id;
    let fields: Vec<Field> = match &prev {
        None => wanted
            .iter()
            .enumerate()
            .map(|(i, (n, t, _))| Field {
                id: i as i64 + 1,
                name: n.clone(),
                ty: t.clone(),
            })
            .collect(),
        Some(_) if !overwrite => {
            let mut a: Vec<(&str, &str)> = cur_fields
                .iter()
                .map(|f| (f.name.as_str(), f.ty.as_str()))
                .collect();
            let mut b: Vec<(&str, &str)> = wanted
                .iter()
                .map(|(n, t, _)| (n.as_str(), t.as_str()))
                .collect();
            a.sort();
            b.sort();
            if a != b {
                return Err(format!("columns do not match the table schema: table {a:?}, query {b:?}; use mode := 'overwrite' to change the schema").into());
            }
            cur_fields.clone()
        }
        Some(_) => wanted
            .iter()
            .map(
                |(n, t, _)| match cur_fields.iter().find(|f| &f.name == n && &f.ty == t) {
                    Some(f) => f.clone(),
                    None => {
                        next_id += 1;
                        Field {
                            id: next_id,
                            name: n.clone(),
                            ty: t.clone(),
                        }
                    }
                },
            )
            .collect(),
    };
    let schema_changed = prev.is_some() && {
        let mut a: Vec<(i64, &str)> = cur_fields.iter().map(|f| (f.id, f.name.as_str())).collect();
        let mut b: Vec<(i64, &str)> = fields.iter().map(|f| (f.id, f.name.as_str())).collect();
        a.sort();
        b.sort();
        a != b
    };
    let schema_id = if schema_changed {
        prev.as_ref()
            .and_then(|(_, m)| {
                m.get("schemas")?
                    .as_array()?
                    .iter()
                    .filter_map(|s| s.get("schema-id")?.as_i64())
                    .max()
            })
            .unwrap_or(0)
            + 1
    } else {
        cur_schema_id
    };
    let pk_id = fields
        .iter()
        .find(|f| f.name == "packed_id")
        .ok_or("the table needs a packed_id column")?
        .id;

    // Partition spec: truncate[W](packed_id). Overwrite may change W (new spec id).
    let cur_spec = prev.as_ref().and_then(|(_, m)| {
        let id = m.get("default-spec-id")?.as_i64()?;
        by_id(m, "partition-specs", "spec-id", id).cloned()
    });
    let cur_width: Option<i64> = cur_spec.as_ref().and_then(|s| {
        let pf = s.get("fields")?.as_array()?.first()?;
        let w = pf
            .get("transform")?
            .as_str()?
            .strip_prefix("truncate[")?
            .strip_suffix(']')?
            .parse()
            .ok()?;
        (pf.get("source-id")?.as_i64()? == pk_id).then_some(w)
    });
    if prev.is_some() && cur_width.is_none() && !overwrite {
        return Err("table must be partitioned by truncate[W](packed_id)".into());
    }
    let wanted_width = o
        .partition_level
        .map(|l| level_u8(l as i32).map(|l| truncate_width(st.level, l)))
        .transpose()?;
    let width = match (cur_width, wanted_width) {
        (Some(c), Some(w)) if c != w && !overwrite => {
            return Err(format!("partition_level does not match the table partition truncate[{c}]; use mode := 'overwrite' to change it").into())
        }
        (Some(c), None) => c,
        (_, Some(w)) => w,
        (None, None) => truncate_width(st.level, partition_level(st, o)?),
    };
    let spec_changed = prev.is_some() && cur_width != Some(width);
    let last_partition_id = prev
        .as_ref()
        .and_then(|(_, m)| m.get("last-partition-id")?.as_i64())
        .unwrap_or(iceberg::PARTITION_FIELD_ID - 1);
    let (spec_id, partition_field_id) = if spec_changed {
        let id = prev
            .as_ref()
            .and_then(|(_, m)| {
                m.get("partition-specs")?
                    .as_array()?
                    .iter()
                    .filter_map(|s| s.get("spec-id")?.as_i64())
                    .max()
            })
            .unwrap_or(0)
            + 1;
        (id, last_partition_id + 1)
    } else {
        let fid = cur_spec
            .as_ref()
            .and_then(|s| {
                s.get("fields")?
                    .as_array()?
                    .first()?
                    .get("field-id")?
                    .as_i64()
            })
            .unwrap_or(iceberg::PARTITION_FIELD_ID);
        (
            cur_spec
                .as_ref()
                .and_then(|s| s.get("spec-id")?.as_i64())
                .unwrap_or(0),
            fid,
        )
    };

    // Append: no cell may already be in the table. Upsert: merge the staged rows with the
    // rows of the partitions they touch (staged rows win); only those partitions are written.
    let upsert = o.mode == "upsert";
    if upsert && prev.is_none() {
        return Err(format!("{root}: mode 'upsert' needs an existing Iceberg table").into());
    }
    if upsert && (schema_changed || spec_changed) {
        return Err("mode 'upsert' keeps the table's columns, types and partition level; the input differs: use mode := 'overwrite'".into());
    }
    let mut replaced: std::collections::HashSet<String> = Default::default();
    let mut replaced_rows = 0i64;
    let mut staged_rows = st.rows;
    if let (Some((meta_path, meta)), false) = (&prev, overwrite) {
        let parts_in: Vec<i64> = rows(
            lease,
            &format!("SELECT DISTINCT packed_id - (packed_id % {width}) FROM {stage} ORDER BY 1"),
        )?
        .iter()
        .map(|r| num(&r[0]))
        .collect();
        // files_for takes cell ids: give the first and last id of every partition so its
        // packed_id bounds check covers whole partitions.
        let span: Vec<i64> = parts_in.iter().flat_map(|p| [*p, p + width - 1]).collect();
        let table = crate::icefast::load(lease, root, meta_path, meta)?;
        let planned = table
            .as_ref()
            .map(|t| t.files_for(lease, &span))
            .transpose()?
            .flatten();
        let scan = |files: &[String]| -> String {
            match &table {
                Some(t) => t.scan(files),
                None => format!(
                    "read_parquet([{}], hive_partitioning = false)",
                    files.iter().map(|f| lit(f)).collect::<Vec<_>>().join(", ")
                ),
            }
        };
        match (planned, upsert) {
            (Some(old_files), _) if old_files.is_empty() => {}
            (Some(old_files), false) => {
                let dup = num(&one_row(
                    lease,
                    &format!(
                        "SELECT count(*) FROM {stage} s SEMI JOIN {} t ON s.packed_id = t.packed_id",
                        scan(&old_files)
                    ),
                )?[0]);
                if dup > 0 {
                    return Err(format!("{dup} cells are already in the table; a cell_values table holds one row per cell (use mode := 'upsert' or 'overwrite')").into());
                }
            }
            (Some(old_files), true) => {
                let old = scan(&old_files);
                let cols: Vec<String> = st
                    .columns
                    .iter()
                    .map(|(c, _)| {
                        let c = ident(c);
                        if c == "\"packed_id\"" {
                            "coalesce(n.packed_id, o.packed_id) AS packed_id".to_string()
                        } else {
                            format!("CASE WHEN n.packed_id IS NOT NULL THEN n.{c} ELSE o.{c} END AS {c}")
                        }
                    })
                    .collect();
                replaced_rows = num(&one_row(lease, &format!("SELECT count(*) FROM {old}"))?[0]);
                // stage is `db.main."w_<uuid>"`: the merged table goes next to it.
                let merged = match stage.strip_suffix('"') {
                    Some(q) => format!("{q}_u\""),
                    None => format!("{stage}_u"),
                };
                sql::exec_on(lease, &format!("DROP TABLE IF EXISTS {merged}"))?;
                sql::exec_on(
                    lease,
                    &format!(
                        "CREATE TABLE {merged} AS SELECT {} FROM {stage} n FULL JOIN (SELECT * FROM {old}) o ON n.packed_id = o.packed_id ORDER BY packed_id",
                        cols.join(", ")
                    ),
                )?;
                sql::exec_on(lease, &format!("DROP TABLE {stage}"))?;
                let short = stage.rsplit('.').next().unwrap_or(&stage).to_string();
                sql::exec_on(lease, &format!("ALTER TABLE {merged} RENAME TO {short}"))?;
                staged_rows = num(&one_row(lease, &format!("SELECT count(*) FROM {stage}"))?[0]);
                replaced = old_files.into_iter().collect();
            }
            (None, true) => {
                return Err("this table cannot be planned (delete files or no partition values): use mode := 'overwrite'".into());
            }
            (None, false) => {
                // Fallback for tables icefast cannot plan: iceberg_scan over the staged range.
                dataset::load_iceberg_ext(lease)?;
                let scan = match meta_path.rsplit_once("/metadata/") {
                    Some((dir, file)) => format!(
                        "iceberg_scan({}, version = {}, allow_moved_paths = true)",
                        lit(dir),
                        lit(file.strip_suffix(".metadata.json").unwrap_or(file))
                    ),
                    None => format!("iceberg_scan({})", lit(meta_path)),
                };
                let r = one_row(
                    lease,
                    &format!("SELECT min(packed_id), max(packed_id) FROM {stage}"),
                )?;
                let (lo, hi) = (num(&r[0]), num(&r[1]));
                let dup = num(&one_row(
                    lease,
                    &format!(
                        "SELECT count(*) FROM {stage} s JOIN (SELECT packed_id FROM {scan} \
                         WHERE packed_id BETWEEN {lo} AND {hi}) t ON s.packed_id = t.packed_id"
                    ),
                )?[0]);
                if dup > 0 {
                    return Err(format!("{dup} cells are already in the table; a cell_values table holds one row per cell (use mode := 'overwrite' to replace it)").into());
                }
            }
        }
    }

    let by_name = |n: &str| fields.iter().find(|f| f.name == n);
    let metric_fields: Vec<&Field> = match o.column_metrics.as_str() {
        "all" => fields.iter().collect(),
        "key" => fields
            .iter()
            .filter(|f| KEY_COLUMNS.contains(&f.name.as_str()))
            .collect(),
        "auto" => fields
            .iter()
            .enumerate()
            .filter(|(i, f)| *i < 100 || KEY_COLUMNS.contains(&f.name.as_str()))
            .map(|(_, f)| f)
            .collect(),
        m => {
            return Err(format!("column_metrics must be 'auto', 'key' or 'all', got '{m}'").into())
        }
    };
    let select: Vec<String> = fields
        .iter()
        .map(|f| {
            match wanted
                .iter()
                .find(|(n, _, _)| *n == f.name)
                .and_then(|(_, _, c)| *c)
            {
                Some(c) => format!("CAST({} AS {c}) AS {}", ident(&f.name), ident(&f.name)),
                None => ident(&f.name),
            }
        })
        .collect();
    let field_ids: Vec<String> = fields
        .iter()
        .map(|f| format!("{}: {}", ident(&f.name), f.id))
        .collect();
    let snapshot_id = (random_u64() >> 1) as i64;
    let write_uuid = uuid4();
    let parts: Vec<i64> = rows(
        lease,
        &format!("SELECT DISTINCT packed_id - (packed_id % {width}) FROM {stage} ORDER BY 1"),
    )?
    .iter()
    .map(|r| num(&r[0]))
    .collect();
    let prev_total = if overwrite {
        0
    } else {
        prev.as_ref()
            .map_or(0, |(_, m)| summary_i64(m, "total-records"))
    };
    // Data files: one sorted file per partition, written by DuckDB (staged locally for S3).
    let mut staging = Staging::new(root)?;
    let mut jobs = Vec::with_capacity(parts.len());
    let mut specs = Vec::with_capacity(parts.len());
    for (i, pt) in parts.iter().enumerate() {
        let rel = format!(
            "data/{}={pt}/{write_uuid}-{i:05}.parquet",
            iceberg::PARTITION_FIELD
        );
        specs.push(FileSpec {
            path: rel.clone(),
            predicate: format!("packed_id >= {pt} AND packed_id < {}", pt + width),
        });
        let file = staging.path(&join(root, &rel), &rel)?;
        ensure_dir(&join(
            root,
            &format!("data/{}={pt}", iceberg::PARTITION_FIELD),
        ))?;
        jobs.push((*pt, rel, file));
    }
    let vf = value_fields(st, o)?;
    let facts = facts11(lease, st, o, &vf, specs)?;
    let cell_manifest = manifest(
        st,
        o,
        vf,
        vec![
            "domain_code".into(),
            "level".into(),
            format!("iceberg:truncate({width},packed_id)"),
        ],
        prev_total - replaced_rows + staged_rows,
        facts.as_ref(),
    )?;
    let copy_options = parquet_options(
        o,
        &cell_manifest,
        &format!(", FIELD_IDS {{{}}}", field_ids.join(", ")),
    );
    let mut agg = vec!["count(*)".to_string()];
    for f in &metric_fields {
        let c = ident(&f.name);
        agg.push(format!("count({c})"));
        agg.push(format!("CAST(min({c}) AS VARCHAR)"));
        agg.push(format!("CAST(max({c}) AS VARCHAR)"));
    }
    let agg = agg.join(", ");
    let select = select.join(", ");
    // One sorted file per partition, several partitions at a time on internal connections.
    let files = sql::par_map(jobs, write_threads(), |l, (pt, rel, file)| {
        let filter = format!("packed_id >= {pt} AND packed_id < {}", pt + width);
        sql::exec_on(
            l,
            &format!(
                "COPY (SELECT {select} FROM {stage} WHERE {filter} ORDER BY packed_id) TO {} ({copy_options})",
                lit(&file)
            ),
        )?;
        let r = one_row(l, &format!("SELECT {agg} FROM {stage} WHERE {filter}"))?;
        let n = num(&r[0]);
        let mut df = DataFile {
            path: join(&location, &rel),
            record_count: n,
            size: store.size(&file)?,
            partition: pt,
            ..Default::default()
        };
        for (k, f) in metric_fields.iter().enumerate() {
            df.value_counts.push((f.id, n));
            df.null_counts.push((f.id, n - num(&r[1 + 3 * k])));
            if let (Some(lo), Some(hi)) = (&r[2 + 3 * k], &r[3 + 3 * k]) {
                if let (Some(a), Some(b)) = (
                    iceberg::bound_bytes(&f.ty, lo),
                    iceberg::bound_bytes(&f.ty, hi),
                ) {
                    df.lower.push((f.id, a));
                    df.upper.push((f.id, b));
                }
            }
        }
        for r in rows(l, &format!("SELECT path_in_schema, sum(total_compressed_size) FROM parquet_metadata({}) GROUP BY 1", lit(&file)))? {
            if let Some(f) = r[0].as_deref().and_then(by_name).filter(|f| metric_fields.iter().any(|m| m.id == f.id)) {
                df.column_sizes.push((f.id, num(&r[1])));
            }
        }
        Ok(df)
    })?;

    staging.upload(store)?;

    // Manifest and manifest list.
    let prev_info: Option<Json> = prev
        .as_ref()
        .and_then(|(_, m)| {
            m.pointer(&format!(
                "/properties/{}",
                crate::metadata::INFO_KEY.replace('/', "~1")
            ))
        })
        .and_then(Json::as_str)
        .and_then(|t| serde_json::from_str(t).ok());
    let table_info = crate::metadata::info(prev_info.as_ref(), &cell_manifest, &o.extra);
    let mut schema = iceberg::schema_json(&fields, schema_id);
    crate::metadata::schema_docs(&mut schema, &table_info);
    let mut spec = iceberg::spec_json(pk_id, width);
    spec["spec-id"] = json!(spec_id);
    spec["fields"][0]["field-id"] = json!(partition_field_id);
    // Data files are in packed_id order; one manifest per MANIFEST_FILES of them, each with
    // its partition range in the manifest list, so a reader fetches only the manifests
    // covering its area instead of one manifest listing every file.
    let mut written = Vec::new();
    for (k, chunk) in files.chunks(MANIFEST_FILES).enumerate() {
        let rel = format!("metadata/{write_uuid}-m{k}.avro");
        let bytes = iceberg::manifest_bytes(chunk, snapshot_id, &schema, &spec, sync_marker());
        let len = bytes.len() as i64;
        store.put(&join(root, &rel), bytes)?;
        written.push((rel, len, chunk));
    }
    let seq = prev
        .as_ref()
        .and_then(|(_, m)| m.get("last-sequence-number")?.as_i64())
        .unwrap_or(0)
        + 1;
    let parent = prev
        .as_ref()
        .and_then(|(_, m)| m.get("current-snapshot-id")?.as_i64())
        .filter(|s| *s >= 0);
    let mut entries = Vec::new();
    if let (Some((_, m)), false) = (&prev, overwrite) {
        if let Some(list) = current_snapshot(m).and_then(|s| s.get("manifest-list")?.as_str()) {
            let bytes = store
                .get(&relocate(list))
                .map_err(|e| format!("cannot read manifest list {list}: {e}"))?;
            entries = iceberg::read_manifest_list(&bytes)?;
        }
    }
    let mut carried: Vec<DataFile> = Vec::new();
    let mut replaced_size = 0i64;
    if upsert && !replaced.is_empty() {
        let mut keep = Vec::new();
        for e in entries {
            let range = e
                .partitions
                .as_ref()
                .and_then(|p| p.first())
                .and_then(|(_, lo, hi)| {
                    let f = |b: &Option<Vec<u8>>| {
                        Some(i64::from_le_bytes(b.as_ref()?.get(..8)?.try_into().ok()?))
                    };
                    f(lo).zip(f(hi))
                });
            let touched = match range {
                Some((lo, hi)) => parts.iter().any(|p| *p >= lo && *p <= hi),
                None => true,
            };
            if !touched {
                keep.push(e);
                continue;
            }
            if e.spec_id != spec_id {
                return Err("the table has files of an older partition spec: run compact_cell_dataset first".into());
            }
            let bytes = store.get(&relocate(&e.path))?;
            let (_, recs) = crate::avro::read_container(&bytes)?;
            let mut hit = false;
            let mut rest = Vec::new();
            let mut gone_size = 0i64;
            for r in recs {
                if r.get("status").and_then(crate::avro::D::long) == Some(2) {
                    continue;
                }
                let Some(df) = r.get("data_file") else {
                    continue;
                };
                let Some(f) = data_file_from_entry(df) else {
                    return Err("cannot carry a manifest entry over (unexpected layout): use mode := 'overwrite'".into());
                };
                if replaced.contains(&relocate(&f.path)) {
                    hit = true;
                    gone_size += f.size;
                } else {
                    rest.push(f);
                }
            }
            if hit {
                carried.extend(rest);
                replaced_size += gone_size;
            } else {
                keep.push(e);
            }
        }
        entries = keep;
    }
    // Files kept from manifests an upsert touched: listed again in new manifests.
    let mut written_carried = Vec::new();
    for (k, chunk) in carried.chunks(MANIFEST_FILES).enumerate() {
        let rel = format!("metadata/{write_uuid}-c{k}.avro");
        let bytes = iceberg::manifest_bytes(chunk, snapshot_id, &schema, &spec, sync_marker());
        let len = bytes.len() as i64;
        store.put(&join(root, &rel), bytes)?;
        written_carried.push((rel, len, chunk));
    }
    let le = |v: i64| v.to_le_bytes().to_vec();
    let added_rows: i64 = files.iter().map(|f| f.record_count).sum();
    for (rel, len, chunk) in written.iter().chain(written_carried.iter()) {
        entries.push(ManifestFile {
            path: join(&location, rel),
            length: *len,
            spec_id,
            content: 0,
            sequence_number: seq,
            min_sequence_number: seq,
            added_snapshot_id: snapshot_id,
            added_files: chunk.len() as i64,
            existing_files: 0,
            deleted_files: 0,
            added_rows: chunk.iter().map(|f| f.record_count).sum(),
            existing_rows: 0,
            deleted_rows: 0,
            partitions: Some(vec![(
                false,
                chunk.iter().map(|f| f.partition).min().map(le),
                chunk.iter().map(|f| f.partition).max().map(le),
            )]),
        });
    }
    let list_rel = format!("metadata/snap-{snapshot_id}-1-{write_uuid}.avro");
    store.put(
        &join(root, &list_rel),
        iceberg::manifest_list_bytes(&entries, snapshot_id, parent, seq, sync_marker()),
    )?;

    // Snapshot and table metadata.
    let now = now_ms();
    let added_size: i64 = files.iter().map(|f| f.size).sum();
    let base = |k: &str| {
        if overwrite {
            0
        } else {
            prev.as_ref().map_or(0, |(_, m)| summary_i64(m, k))
        }
    };
    let mut summary = json!({
        "operation": if overwrite || upsert { "overwrite" } else { "append" },
        "added-data-files": files.len().to_string(),
        "added-records": added_rows.to_string(),
        "added-files-size": added_size.to_string(),
        "changed-partition-count": parts.len().to_string(),
        "total-records": (base("total-records") + added_rows - replaced_rows).to_string(),
        "total-data-files": (base("total-data-files") + files.len() as i64 - replaced.len() as i64).to_string(),
        "total-files-size": (base("total-files-size") + added_size - replaced_size).to_string(),
        "total-delete-files": "0",
        "total-position-deletes": "0",
        "total-equality-deletes": "0",
        "geosquare-writer": format!("geosquare_v2 duckdb extension {}", env!("CARGO_PKG_VERSION"))
    });
    if upsert {
        summary["deleted-data-files"] = json!(replaced.len().to_string());
        summary["deleted-records"] = json!(replaced_rows.to_string());
        summary["removed-files-size"] = json!(replaced_size.to_string());
    }
    if overwrite {
        if let Some((_, m)) = &prev {
            summary["deleted-data-files"] = json!(summary_i64(m, "total-data-files").to_string());
            summary["deleted-records"] = json!(summary_i64(m, "total-records").to_string());
            summary["removed-files-size"] = json!(summary_i64(m, "total-files-size").to_string());
        }
    }
    let mut snapshot = json!({
        "snapshot-id": snapshot_id,
        "sequence-number": seq,
        "timestamp-ms": now,
        "manifest-list": join(&location, &list_rel),
        "schema-id": schema_id,
        "summary": summary
    });
    // Iceberg omits parent-snapshot-id for the first snapshot (DuckDB rejects null).
    if let Some(p) = parent {
        snapshot["parent-snapshot-id"] = json!(p);
    }
    let mut meta = match &prev {
        Some((_, m)) => m.clone(),
        None => json!({
            "format-version": 2,
            "table-uuid": uuid4(),
            "location": location,
            "last-sequence-number": 0,
            "last-updated-ms": now,
            "last-column-id": 0,
            "current-schema-id": 0,
            "schemas": [],
            "default-spec-id": 0,
            "partition-specs": [],
            "last-partition-id": iceberg::PARTITION_FIELD_ID,
            "default-sort-order-id": 1,
            "sort-orders": [
                {"order-id": 0, "fields": []},
                {"order-id": 1, "fields": [{"transform": "identity", "source-id": pk_id, "direction": "asc", "null-order": "nulls-first"}]}
            ],
            "properties": {"write.parquet.compression-codec": "zstd", "write.metadata.metrics.column.packed_id": "full"},
            "refs": {},
            "snapshots": [],
            "snapshot-log": [],
            "metadata-log": []
        }),
    };
    let obj = meta.as_object_mut().ok_or("metadata is not an object")?;
    if prev.is_none() || schema_changed {
        obj.entry("schemas")
            .or_insert(json!([]))
            .as_array_mut()
            .ok_or("schemas")?
            .push(schema.clone());
    }
    if prev.is_none() || spec_changed {
        obj.entry("partition-specs")
            .or_insert(json!([]))
            .as_array_mut()
            .ok_or("partition-specs")?
            .push(spec.clone());
        obj.insert(
            "last-partition-id".into(),
            json!(partition_field_id.max(last_partition_id)),
        );
    }
    obj.insert("location".into(), json!(location));
    obj.insert("current-schema-id".into(), json!(schema_id));
    obj.insert("default-spec-id".into(), json!(spec_id));
    obj.insert(
        "last-column-id".into(),
        json!(fields
            .iter()
            .map(|f| f.id)
            .max()
            .unwrap_or(0)
            .max(last_column_id)),
    );
    obj.insert("last-sequence-number".into(), json!(seq));
    obj.insert("last-updated-ms".into(), json!(now));
    obj.insert("current-snapshot-id".into(), json!(snapshot_id));
    obj.entry("snapshots")
        .or_insert(json!([]))
        .as_array_mut()
        .ok_or("snapshots")?
        .push(snapshot);
    obj.entry("snapshot-log")
        .or_insert(json!([]))
        .as_array_mut()
        .ok_or("snapshot-log")?
        .push(json!({"snapshot-id": snapshot_id, "timestamp-ms": now}));
    obj.insert(
        "refs".into(),
        json!({"main": {"snapshot-id": snapshot_id, "type": "branch"}}),
    );
    // The cell manifest describes gid-bearing tables only.
    let props = obj
        .entry("properties")
        .or_insert(json!({}))
        .as_object_mut()
        .ok_or("properties")?;
    if fields.iter().any(|f| f.name == "gid") {
        props.insert(MANIFEST_KEY.into(), json!(cell_manifest.to_string()));
    } else {
        props.remove(MANIFEST_KEY);
    }
    props.insert(
        crate::metadata::INFO_KEY.into(),
        json!(table_info.to_string()),
    );
    if let Some(d) = table_info.get("description").and_then(Json::as_str) {
        props.insert("comment".into(), json!(d));
    }
    if let Some((prev_path, prev_meta)) = &prev {
        let ts = prev_meta
            .get("last-updated-ms")
            .cloned()
            .unwrap_or(json!(now));
        obj.entry("metadata-log")
            .or_insert(json!([]))
            .as_array_mut()
            .ok_or("metadata-log")?
            .push(json!({"metadata-file": prev_path, "timestamp-ms": ts}));
    }

    // Commit: refuse if another writer committed meanwhile (no catalog lock).
    if let Some((prev_path, _)) = &prev {
        let now_head = dataset::latest_iceberg_metadata(lease, root)?.0;
        if &now_head != prev_path {
            return Err(format!(
                "{root} changed during the write (newest metadata is now {now_head}); retry"
            )
            .into());
        }
    }
    let version = max_metadata_version(lease, root)?.map_or(0, |v| v + 1);
    let stem = format!("{version:05}-{}", uuid4());
    let meta_rel = format!("metadata/{stem}.metadata.json");
    store.put(&join(root, &meta_rel), serde_json::to_vec_pretty(&meta)?)?;
    store.put_if(&hint_path, stem.into_bytes(), hint_tag.as_deref())?;
    Ok(vec![vec![
        Some(if overwrite {
            "iceberg (overwrite)".into()
        } else if upsert {
            "iceberg (upsert)".into()
        } else {
            "iceberg".into()
        }),
        Some(join(root, &meta_rel)),
        Some(files.len().to_string()),
        Some(staged_rows.to_string()),
    ]])
}

// ---------------------------------------------------------------------------
// alter_cell_dataset: drop, rename or add columns without rewriting data (Iceberg)
// ---------------------------------------------------------------------------

/// alter_cell_dataset(path, drop_columns := 'a,b' | '["a","b"]', rename_columns :=
/// '{"old": "new"}', add_columns := '{"name": "DOUBLE"}'). Iceberg only: a new schema with the
/// same field ids, so data files stay as they are and are read by field id (renamed columns
/// under their new name, dropped columns left out, added columns 0 until rewritten). The cell
/// manifest and the column descriptions follow.
pub fn plan_alter(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?;
    let drop: Vec<String> = match p.text("drop_columns") {
        None => Vec::new(),
        Some(t) if t.trim_start().starts_with('[') => serde_json::from_str(&t)
            .map_err(|_| "drop_columns must be 'a,b' or a JSON list of names")?,
        Some(t) => t
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
    };
    let rename: Vec<(String, String)> = match p.text("rename_columns") {
        None => Vec::new(),
        Some(t) => match serde_json::from_str::<Json>(&t) {
            Ok(Json::Object(m)) => m
                .into_iter()
                .map(|(k, v)| {
                    Ok((
                        k,
                        v.as_str()
                            .ok_or("rename_columns values must be names")?
                            .to_string(),
                    ))
                })
                .collect::<Result<_, E>>()?,
            _ => return Err("rename_columns must be a JSON object {\"old\": \"new\"}".into()),
        },
    };
    let add: Vec<(String, String)> = match p.text("add_columns") {
        None => Vec::new(),
        Some(t) => match serde_json::from_str::<Json>(&t) {
            Ok(Json::Object(m)) => m
                .into_iter()
                .map(|(k, v)| Ok((k, v.as_str().unwrap_or("DOUBLE").to_ascii_uppercase())))
                .collect::<Result<_, E>>()?,
            _ => return Err("add_columns must be a JSON object {\"name\": \"DOUBLE\"}".into()),
        },
    };
    if drop.is_empty() && rename.is_empty() && add.is_empty() {
        return Err("nothing to alter: pass drop_columns, rename_columns or add_columns".into());
    }
    let s3 = s3_params(p);
    Ok(Plan::Action(
        Box::new(move || alter(&path, &drop, &rename, &add, &s3)),
        vec!["path".into(), "metadata".into(), "columns".into()],
    ))
}

fn alter(
    path: &str,
    drop: &[String],
    rename: &[(String, String)],
    add: &[(String, String)],
    s3: &S3Params,
) -> Result<Vec<Vec<Option<String>>>, E> {
    let lease = sql::lease()?;
    let root = path.trim_end_matches('/').to_string();
    let store = Storage::for_path(&lease, &root, s3)?;
    let src = dataset::open(&lease, &root)?;
    let dataset::Layout::Iceberg { metadata } = &src.layout else {
        return Err("alter_cell_dataset changes Iceberg tables; rewrite a folder dataset with mode := 'overwrite'".into());
    };
    let hint_path = join(&root, "metadata/version-hint.text");
    let hint_tag = store.version_tag(&hint_path)?;
    let mut meta = dataset::metadata_json(&lease, metadata)?;
    let sid = meta
        .get("current-schema-id")
        .and_then(Json::as_i64)
        .unwrap_or(0);
    let schemas = meta
        .get("schemas")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let current = schemas
        .iter()
        .find(|s| s.get("schema-id").and_then(Json::as_i64) == Some(sid))
        .ok_or("current schema not found")?;
    let mut fields: Vec<Json> = current
        .get("fields")
        .and_then(Json::as_array)
        .cloned()
        .unwrap_or_default();
    let name_of = |f: &Json| {
        f.get("name")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_string()
    };
    let protected = ["gid", "packed_id", "x_idx", "y_idx"];
    for d in drop {
        if protected.contains(&d.as_str()) {
            return Err(format!("{d} is a cell id column and cannot be dropped").into());
        }
        let n = fields.len();
        fields.retain(|f| &name_of(f) != d);
        if fields.len() == n {
            return Err(format!("drop_columns: no column {d}").into());
        }
    }
    for (old, new) in rename {
        if protected.contains(&old.as_str()) || protected.contains(&new.as_str()) {
            return Err(format!("cell id columns cannot be renamed ({old} -> {new})").into());
        }
        if fields.iter().any(|f| &name_of(f) == new) {
            return Err(format!("rename_columns: {new} already exists").into());
        }
        let f = fields
            .iter_mut()
            .find(|f| &name_of(f) == old)
            .ok_or_else(|| format!("rename_columns: no column {old}"))?;
        f["name"] = json!(new);
        if let Some(o) = f.as_object_mut() {
            o.remove("doc");
        }
    }
    let mut last_id = meta
        .get("last-column-id")
        .and_then(Json::as_i64)
        .unwrap_or(0);
    let mut added_fields = Vec::new();
    for (name, ty) in add {
        if fields.iter().any(|f| &name_of(f) == name) {
            return Err(format!("add_columns: {name} already exists").into());
        }
        let ice = match ty.as_str() {
            "DOUBLE" => "double",
            "FLOAT" | "REAL" => "float",
            "BIGINT" | "LONG" => "long",
            "INTEGER" | "INT" => "int",
            other => {
                return Err(format!(
                    "add_columns: type {other} is not supported (DOUBLE, FLOAT, BIGINT, INTEGER)"
                )
                .into())
            }
        };
        last_id += 1;
        fields.push(
            json!({"id": last_id, "name": name, "required": false, "type": ice,
                           "initial-default": 0, "write-default": 0}),
        );
        added_fields.push((name.clone(), ice));
    }
    let new_sid = schemas
        .iter()
        .filter_map(|s| s.get("schema-id").and_then(Json::as_i64))
        .max()
        .unwrap_or(0)
        + 1;
    let mut schema = json!({"type": "struct", "schema-id": new_sid, "fields": fields});
    if let Some(ids) = current.get("identifier-field-ids") {
        schema["identifier-field-ids"] = ids.clone();
    }

    // Cell manifest and dataset_info follow the columns.
    let prop = |m: &Json, k: &str| -> Option<Json> {
        m.pointer(&format!("/properties/{}", k.replace('/', "~1")))
            .and_then(Json::as_str)
            .and_then(|t| serde_json::from_str(t).ok())
    };
    let mut manifest = prop(&meta, MANIFEST_KEY).ok_or("the table has no cell manifest")?;
    if manifest_version_of(&manifest) == manifest11::VERSION
        && (!drop.is_empty() || !add.is_empty() || !rename.is_empty())
    {
        return Err(
            "manifest 1.1 tables take no column changes yet: their content hashes are read \
                    by column name, so a renamed or dropped column cannot be checked. Rewrite the \
                    table with mode := 'overwrite' to change its columns"
                .into(),
        );
    }
    let mut info = prop(&meta, crate::metadata::INFO_KEY).unwrap_or_else(|| json!({}));
    if let Some(vf) = manifest
        .get_mut("value_fields")
        .and_then(Json::as_array_mut)
    {
        vf.retain(|f| {
            !drop
                .iter()
                .any(|d| f.get("name").and_then(Json::as_str) == Some(d))
        });
        for f in vf.iter_mut() {
            if let Some((_, new)) = rename
                .iter()
                .find(|(o, _)| f.get("name").and_then(Json::as_str) == Some(o))
            {
                f["name"] = json!(new);
            }
        }
        for (name, ice) in &added_fields {
            vf.push(json!({"name": name, "parquet_type": ice, "logical_type": "numeric", "semantics": "MEASUREMENT",
                           "aggregation_rule": null, "unit": null, "nullable": true}));
        }
    }
    if let Some(cols) = info.get_mut("columns").and_then(Json::as_object_mut) {
        for d in drop {
            cols.remove(d);
        }
        for (o, n) in rename {
            if let Some(v) = cols.remove(o) {
                cols.insert(n.clone(), v);
            }
        }
        for (name, _) in &added_fields {
            cols.insert(name.clone(), json!({"description": null, "unit": null}));
        }
    }
    crate::metadata::schema_docs(&mut schema, &info);

    let now = now_ms();
    let obj = meta.as_object_mut().ok_or("metadata is not an object")?;
    obj.get_mut("schemas")
        .and_then(Json::as_array_mut)
        .ok_or("schemas")?
        .push(schema);
    obj.insert("current-schema-id".into(), json!(new_sid));
    obj.insert("last-column-id".into(), json!(last_id));
    let props = obj
        .entry("properties")
        .or_insert(json!({}))
        .as_object_mut()
        .ok_or("properties")?;
    props.insert(MANIFEST_KEY.into(), json!(manifest.to_string()));
    props.insert(crate::metadata::INFO_KEY.into(), json!(info.to_string()));
    props.insert("geosquare.read-by-field-id".into(), json!("true"));
    let prev_ts = obj.get("last-updated-ms").cloned().unwrap_or(json!(now));
    obj.insert("last-updated-ms".into(), json!(now));
    obj.entry("metadata-log")
        .or_insert(json!([]))
        .as_array_mut()
        .ok_or("metadata-log")?
        .push(json!({"metadata-file": metadata, "timestamp-ms": prev_ts}));
    let version = max_metadata_version(&lease, &root)?.map_or(0, |v| v + 1);
    let stem = format!("{version:05}-{}", uuid4());
    let meta_rel = format!("metadata/{stem}.metadata.json");
    store.put(&join(&root, &meta_rel), serde_json::to_vec_pretty(&meta)?)?;
    store.put_if(&hint_path, stem.into_bytes(), hint_tag.as_deref())?;
    let n = manifest["value_fields"].as_array().map_or(0, Vec::len);
    Ok(vec![vec![
        Some(root.clone()),
        Some(join(&root, &meta_rel)),
        Some(n.to_string()),
    ]])
}

// ---------------------------------------------------------------------------
// update_cell_metadata: change descriptive metadata without rewriting data
// ---------------------------------------------------------------------------

/// update_cell_metadata(path, units := ..., column_descriptions := ..., title := ...,
/// description := ..., data_period := ..., data_version := ..., source := ..., coverage := ...,
/// notes := ...). Iceberg: a new metadata file with the same snapshot; flat/partitioned:
/// manifest.json and dataset_info.json rewritten (Parquet footers keep the old copy).
pub fn plan_update(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?;
    let extra = crate::metadata::Extra::from_params(p)?;
    if extra.is_empty() {
        return Err("nothing to update: pass units, column_descriptions, title, description, data_period, data_version, source, coverage or notes".into());
    }
    let s3 = s3_params(p);
    Ok(Plan::Action(
        Box::new(move || update_metadata(&path, &extra, &s3)),
        vec!["path".into(), "metadata".into(), "value_columns".into()],
    ))
}

fn update_metadata(
    path: &str,
    extra: &crate::metadata::Extra,
    s3: &S3Params,
) -> Result<Vec<Vec<Option<String>>>, E> {
    use crate::metadata::{self as md, INFO_FILE, INFO_KEY};
    let lease = sql::lease()?;
    let root = path.trim_end_matches('/').to_string();
    let store = Storage::for_path(&lease, &root, s3)?;
    let src = dataset::open(&lease, &root)?;
    match &src.layout {
        dataset::Layout::Iceberg { metadata } => {
            let hint_path = join(&root, "metadata/version-hint.text");
            let hint_tag = store.version_tag(&hint_path)?;
            let mut meta = dataset::metadata_json(&lease, metadata)?;
            let prop = |m: &Json, k: &str| -> Option<Json> {
                m.pointer(&format!("/properties/{}", k.replace('/', "~1")))
                    .and_then(Json::as_str)
                    .and_then(|t| serde_json::from_str(t).ok())
            };
            let mut manifest = prop(&meta, MANIFEST_KEY)
                .ok_or("the table has no cell manifest (geosquare.manifest)")?;
            md::apply(&mut manifest, extra)?;
            // A 1.1 manifest keeps the dataset title in governance too.
            if let (Some(title), "1.1") = (&extra.title, manifest_version_of(&manifest).as_str()) {
                manifest["governance"]["title"] = json!(title);
            }
            let info = md::info(prop(&meta, INFO_KEY).as_ref(), &manifest, extra);
            let now = now_ms();
            let obj = meta.as_object_mut().ok_or("metadata is not an object")?;
            // column docs: a new schema version with the same field ids when they change
            let cur = obj
                .get("current-schema-id")
                .and_then(Json::as_i64)
                .unwrap_or(0);
            let schemas = obj
                .get("schemas")
                .and_then(Json::as_array)
                .cloned()
                .unwrap_or_default();
            if let Some(current) = schemas
                .iter()
                .find(|s| s.get("schema-id").and_then(Json::as_i64) == Some(cur))
            {
                let mut next = current.clone();
                md::schema_docs(&mut next, &info);
                if &next != current {
                    let id = schemas
                        .iter()
                        .filter_map(|s| s.get("schema-id").and_then(Json::as_i64))
                        .max()
                        .unwrap_or(0)
                        + 1;
                    next["schema-id"] = json!(id);
                    obj.get_mut("schemas")
                        .and_then(Json::as_array_mut)
                        .ok_or("schemas")?
                        .push(next);
                    obj.insert("current-schema-id".into(), json!(id));
                }
            }
            let props = obj
                .entry("properties")
                .or_insert(json!({}))
                .as_object_mut()
                .ok_or("properties")?;
            props.insert(MANIFEST_KEY.into(), json!(manifest.to_string()));
            props.insert(INFO_KEY.into(), json!(info.to_string()));
            if let Some(d) = info.get("description").and_then(Json::as_str) {
                props.insert("comment".into(), json!(d));
            }
            let prev_ts = obj.get("last-updated-ms").cloned().unwrap_or(json!(now));
            obj.insert("last-updated-ms".into(), json!(now));
            obj.entry("metadata-log")
                .or_insert(json!([]))
                .as_array_mut()
                .ok_or("metadata-log")?
                .push(json!({"metadata-file": metadata, "timestamp-ms": prev_ts}));
            if &dataset::latest_iceberg_metadata(&lease, &root)?.0 != metadata {
                return Err(format!("{root} changed during the update; retry").into());
            }
            let version = max_metadata_version(&lease, &root)?.map_or(0, |v| v + 1);
            let stem = format!("{version:05}-{}", uuid4());
            let meta_rel = format!("metadata/{stem}.metadata.json");
            store.put(&join(&root, &meta_rel), serde_json::to_vec_pretty(&meta)?)?;
            store.put_if(&hint_path, stem.into_bytes(), hint_tag.as_deref())?;
            let n = manifest["value_fields"].as_array().map_or(0, Vec::len);
            Ok(vec![vec![
                Some(root.clone()),
                Some(join(&root, &meta_rel)),
                Some(n.to_string()),
            ]])
        }
        _ => {
            let mut manifest = src
                .manifest
                .clone()
                .ok_or("the dataset has no manifest.json")?;
            if manifest_version_of(&manifest) == manifest11::VERSION {
                // Only manifest.json would change; the Parquet files keep the old copy, and the
                // Python reader requires the two to match.
                return Err(
                    "update_cell_metadata cannot change a manifest 1.1 folder: the Parquet \
                            files would keep the old manifest. Rewrite the folder with mode := \
                            'overwrite' (the metadata is kept), or write a new dataset"
                        .into(),
                );
            }
            md::apply(&mut manifest, extra)?;
            let prev: Option<Json> = sql::query_text_on(
                &lease,
                &format!(
                    "SELECT content FROM read_text({})",
                    lit(&join(&root, INFO_FILE))
                ),
            )
            .ok()
            .and_then(|r| r.into_iter().next())
            .and_then(|r| r.into_iter().next().flatten())
            .and_then(|t| serde_json::from_str(&t).ok());
            let info = md::info(prev.as_ref(), &manifest, extra);
            write_manifest(&lease, &root, &manifest)?;
            write_json(&lease, &join(&root, INFO_FILE), &info)?;
            let n = manifest["value_fields"].as_array().map_or(0, Vec::len);
            Ok(vec![vec![
                Some(root.clone()),
                Some(join(&root, MANIFEST_FILE)),
                Some(n.to_string()),
            ]])
        }
    }
}
