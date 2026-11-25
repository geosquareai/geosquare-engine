//! Looking at cell datasets without writing them.
//!
//! * `list_cell_datasets(prefix)`: the datasets in the folders right below `prefix`
//! * `validate_cell_dataset(path)`: identity, uniqueness, NULLs, row count and files checks
//! * `cell_dataset_stats(path, exact := false)`: min / max / NULL count per value column, from
//!   the Iceberg manifests (no data read) or, with `exact` or for folder layouts, a scan

use crate::{
    avro::{self, D},
    dataset::{self, join, Layout, Source},
    sql::{self, ident, lit, Lease, Params, Plan, E},
    storage::Storage,
    writer,
};
use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};

fn sv(s: &str) -> String {
    lit(s)
}

fn opt_lit(v: Option<&str>) -> String {
    v.map_or("NULL::VARCHAR".into(), lit)
}

fn layout_name(src: &Source) -> &'static str {
    match src.layout {
        Layout::Flat => "flat",
        Layout::Partitioned(_) => "partitioned",
        Layout::Iceberg { .. } => "iceberg",
    }
}

// ---------------------------------------------------------------------------
// list_cell_datasets
// ---------------------------------------------------------------------------

pub fn plan_list(p: &Params) -> Result<Plan, E> {
    let prefix = p.pos_text(0, "prefix")?;
    let lease = sql::lease()?;
    let store = Storage::for_path(&lease, &prefix, &writer::s3_params_pub(p))?;
    let dirs = store.list_dirs(&prefix)?;
    let mut rows = Vec::new();
    for dir in dirs {
        let dir = dir.trim_end_matches('/').to_string();
        let Ok(src) = dataset::open(&lease, &dir) else {
            continue; // not a dataset
        };
        let m = src.manifest.clone().unwrap_or(Json::Null);
        let info = dataset::dataset_info(&lease, &src).unwrap_or(Json::Null);
        let name = dir.rsplit('/').next().unwrap_or(&dir);
        let name = name.strip_prefix("data_name=").unwrap_or(name);
        let int = |v: Option<&Json>| {
            v.and_then(Json::as_i64)
                .map_or("NULL::BIGINT".into(), |n| format!("{n}::BIGINT"))
        };
        let txt = |v: Option<&Json>| opt_lit(v.and_then(Json::as_str));
        rows.push(format!(
            "({}, {}, {}, {}, {}, {}, {}, {}, {}, {}, {})",
            sv(name),
            sv(&dir),
            sv(layout_name(&src)),
            txt(m.get("domain_code")),
            int(m.get("level")),
            int(m.pointer("/storage/row_count")),
            m.get("value_fields")
                .and_then(Json::as_array)
                .map_or("NULL::BIGINT".into(), |a| format!("{}::BIGINT", a.len())),
            txt(info.get("title")),
            txt(info.get("description")),
            txt(info.get("data_period")),
            txt(info.get("data_version")),
        ));
    }
    let cols = "name, path, layout, domain_code, level, row_count, value_columns, title, description, data_period, data_version";
    Ok(Plan::Query(if rows.is_empty() {
        "SELECT NULL::VARCHAR AS name, NULL::VARCHAR AS path, NULL::VARCHAR AS layout, NULL::VARCHAR AS domain_code, \
             NULL::BIGINT AS level, NULL::BIGINT AS row_count, NULL::BIGINT AS value_columns, NULL::VARCHAR AS title, \
             NULL::VARCHAR AS description, NULL::VARCHAR AS data_period, NULL::VARCHAR AS data_version LIMIT 0"
            .to_string()
    } else {
        format!(
            "SELECT * FROM (VALUES {}) AS t({cols}) ORDER BY name",
            rows.join(", ")
        )
    }))
}

// ---------------------------------------------------------------------------
// validate_cell_dataset
// ---------------------------------------------------------------------------

/// Live data files of the current Iceberg snapshot (path, record count, the entry record).
fn iceberg_files(lease: &Lease, store: &Storage, src: &Source) -> Result<Vec<(String, D)>, E> {
    let Layout::Iceberg { metadata } = &src.layout else {
        return Ok(Vec::new());
    };
    let meta = dataset::metadata_json(lease, metadata)?;
    let loc = meta
        .get("location")
        .and_then(Json::as_str)
        .unwrap_or(&src.root)
        .to_string();
    let reloc = |p: &str| match p.strip_prefix(loc.trim_end_matches('/')) {
        Some(rest) => format!("{}{rest}", src.root.trim_end_matches('/')),
        None => p.to_string(),
    };
    let cur = meta.get("current-snapshot-id").and_then(Json::as_i64);
    let Some(list) = meta
        .get("snapshots")
        .and_then(Json::as_array)
        .and_then(|a| {
            a.iter()
                .find(|s| s.get("snapshot-id").and_then(Json::as_i64) == cur)
        })
        .and_then(|s| s.get("manifest-list"))
        .and_then(Json::as_str)
    else {
        return Ok(Vec::new());
    };
    let manifests: Vec<String> = crate::iceberg::read_manifest_list(&store.get(&reloc(list))?)?
        .into_iter()
        .map(|m| reloc(&m.path))
        .collect();
    let mut out = Vec::new();
    for bytes in store.get_many(&manifests, 16)? {
        let (_, records) = avro::read_container(&bytes)?;
        for e in records {
            if e.get("status").and_then(D::long) == Some(2) {
                continue;
            }
            if let Some(df) = e.get("data_file") {
                if let Some(D::Str(f)) = df.get("file_path") {
                    out.push((reloc(f), df.clone()));
                }
            }
        }
    }
    Ok(out)
}

pub fn plan_validate(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?;
    let lease = sql::lease()?;
    let root = path.trim_end_matches('/').to_string();
    let src = dataset::open(&lease, &root)?;
    let mut checks: Vec<(String, bool, String)> = Vec::new();
    let Some(m) = src.manifest.clone() else {
        return Err(format!("{root} has no cell manifest; nothing to validate against").into());
    };
    let code = m
        .get("domain_code")
        .and_then(Json::as_str)
        .unwrap_or("")
        .to_string();
    let level = m.get("level").and_then(Json::as_i64).unwrap_or(-1);
    let domain = crate::grid::domain_by_code(&code);
    checks.push((
        "manifest".into(),
        domain.is_ok() && (0..=15).contains(&level),
        format!(
            "domain {code}, level {level}, profile {}",
            m.get("profile_version")
                .and_then(Json::as_str)
                .unwrap_or("?")
        ),
    ));
    let fields: Vec<String> = m
        .get("value_fields")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| f.get("name").and_then(Json::as_str).map(str::to_string))
        .collect();
    let from = format!("read_cell_dataset({})", lit(&root));
    let cols: Vec<String> = sql::query_text_on(
        &lease,
        &format!("SELECT column_name FROM (DESCRIBE SELECT * FROM {from})"),
    )?
    .into_iter()
    .filter_map(|r| r.into_iter().next().flatten())
    .collect();
    // A time cube: one row per (cell, period), periods of one type inside the declared range.
    let grain = crate::timequery::grain_of(Some(&m))?;
    let identity: &[&str] = if grain.is_some() {
        &["gid", "packed_id", "x_idx", "y_idx", "period"]
    } else {
        &["gid", "packed_id", "x_idx", "y_idx"]
    };
    let missing: Vec<&String> = fields.iter().filter(|f| !cols.contains(f)).collect();
    let extra: Vec<&String> = cols
        .iter()
        .filter(|c| !fields.contains(c) && !identity.contains(&c.as_str()))
        .collect();
    checks.push((
        "columns".into(),
        missing.is_empty() && extra.is_empty(),
        if missing.is_empty() && extra.is_empty() {
            format!("{} value columns match the manifest", fields.len())
        } else {
            format!("missing in data: {missing:?}; not in manifest: {extra:?}")
        },
    ));
    let mut manifest_records: Option<i64> = None;
    if let Layout::Iceberg { .. } = &src.layout {
        let store = Storage::for_path(&lease, &root, &writer::s3_params_pub(p))?;
        let files = iceberg_files(&lease, &store, &src)?;
        let listed: HashSet<String> = store
            .list(&join(&root, "data"))?
            .into_iter()
            .map(|(f, _, _)| f)
            .collect();
        let gone: Vec<&String> = files
            .iter()
            .map(|(f, _)| f)
            .filter(|f| !listed.contains(*f))
            .collect();
        let records: i64 = files
            .iter()
            .filter_map(|(_, d)| d.get("record_count").and_then(D::long))
            .sum();
        manifest_records = Some(records);
        let gone_n = gone.len();
        checks.push((
            "data_files_exist".into(),
            gone.is_empty(),
            format!(
                "{} files referenced, {} missing{}",
                files.len(),
                gone.len(),
                gone.first()
                    .map_or(String::new(), |g| format!(" (e.g. {g})"))
            ),
        ));
        if gone_n > 0 {
            for c in [
                "row_count",
                "unique_cells",
                "gid_matches_packed_id",
                "domain_and_level",
                "no_null_values",
            ] {
                checks.push((c.into(), false, "skipped: data files are missing".into()));
            }
            return Ok(checks_plan(&checks));
        }
    }
    // One scan for every count.
    let has = |c: &str| cols.iter().any(|x| x == c);
    let nulls: Vec<String> = fields
        .iter()
        .filter(|f| cols.contains(f))
        .map(|f| format!("count(*) FILTER (WHERE {} IS NULL)", ident(f)))
        .collect();
    let null_sum = if nulls.is_empty() {
        "0".into()
    } else {
        nulls.join(" + ")
    };
    let pack_check = if has("packed_id") && has("gid") {
        format!(
            "count(*) FILTER (WHERE pack({}, gid) IS DISTINCT FROM packed_id)",
            lit(&code)
        )
    } else {
        "0".into()
    };
    let key = if has("packed_id") { "packed_id" } else { "gid" };
    let level_check = if has("packed_id") {
        format!(
            "count(*) FILTER (WHERE (packed_id >> 50) & 15 <> {level} OR (packed_id >> 54) <> {})",
            domain.as_ref().map_or(-1, |d| d.id as i64)
        )
    } else {
        format!("count(*) FILTER (WHERE length(gid) <> {level})")
    };
    let (distinct_key, period_check) = match &grain {
        Some(g) if has("period") => {
            let range = match (
                m.get("period_start").and_then(Json::as_str),
                m.get("period_end").and_then(Json::as_str),
            ) {
                (Some(a), Some(b)) => format!(" OR period < {} OR period >= {}", lit(a), lit(b)),
                _ => String::new(),
            };
            (
                format!("({key}, period)"),
                format!(
                    "count(*) FILTER (WHERE period IS NULL OR NOT period_valid(period, {}){range})",
                    lit(&g.spec())
                ),
            )
        }
        _ => (key.to_string(), "0".to_string()),
    };
    let r = sql::query_text_on(
        &lease,
        &format!(
            "SELECT count(*), count(DISTINCT {distinct_key}), {pack_check}, {level_check}, {null_sum}, \
             count(*) FILTER (WHERE {key} IS NULL), {period_check} FROM {from}"
        ),
    )?;
    let v: Vec<i64> = r
        .into_iter()
        .next()
        .unwrap_or_default()
        .into_iter()
        .map(|x| x.and_then(|s| s.parse().ok()).unwrap_or(0))
        .collect();
    let (rows, distinct, bad_pack, bad_level, null_values, null_ids, bad_periods) =
        (v[0], v[1], v[2], v[3], v[4], v[5], v[6]);
    let declared = m.pointer("/storage/row_count").and_then(Json::as_i64);
    checks.push((
        "row_count".into(),
        declared.is_none_or(|d| d == rows),
        format!(
            "{rows} rows; manifest says {}",
            declared.map_or("nothing".into(), |d| d.to_string())
        ),
    ));
    if let Some(g) = &grain {
        checks.push((
            "unique_cell_periods".into(),
            distinct == rows && null_ids == 0,
            format!("{distinct} distinct (cell, period) of {rows}; {null_ids} without id"),
        ));
        checks.push((
            "periods".into(),
            has("period") && bad_periods == 0,
            if has("period") {
                format!(
                    "{bad_periods} rows with no {} label inside [{}, {})",
                    g.spec(),
                    m.get("period_start").and_then(Json::as_str).unwrap_or("?"),
                    m.get("period_end").and_then(Json::as_str).unwrap_or("?")
                )
            } else {
                "the data has no period column".into()
            },
        ));
    } else {
        checks.push((
            "unique_cells".into(),
            distinct == rows && null_ids == 0,
            format!("{distinct} distinct of {rows}; {null_ids} without id"),
        ));
    }
    checks.push((
        "gid_matches_packed_id".into(),
        bad_pack == 0,
        format!("{bad_pack} rows differ"),
    ));
    checks.push((
        "domain_and_level".into(),
        bad_level == 0,
        format!("{bad_level} rows outside domain {code} level {level}"),
    ));
    checks.push((
        "no_null_values".into(),
        null_values == 0,
        format!("{null_values} NULL values in value columns"),
    ));
    if let Some(records) = manifest_records {
        checks.push((
            "manifest_record_count".into(),
            records == rows,
            format!("manifests count {records} rows, data has {rows}"),
        ));
    }
    Ok(checks_plan(&checks))
}

fn checks_plan(checks: &[(String, bool, String)]) -> Plan {
    let rows: Vec<String> = checks
        .iter()
        .map(|(c, ok, d)| format!("({}, {ok}, {})", lit(c), lit(d)))
        .collect();
    Plan::Query(format!(
        "SELECT * FROM (VALUES {}) AS t(\"check\", ok, detail)",
        rows.join(", ")
    ))
}

// ---------------------------------------------------------------------------
// cell_dataset_stats
// ---------------------------------------------------------------------------

/// Iceberg single-value bound (spec appendix D) as f64, for numeric types.
fn decode_bound(ty: &str, b: &[u8]) -> Option<f64> {
    match ty {
        "double" => Some(f64::from_le_bytes(b.get(..8)?.try_into().ok()?)),
        "float" => Some(f32::from_le_bytes(b.get(..4)?.try_into().ok()?) as f64),
        "long" => Some(i64::from_le_bytes(b.get(..8)?.try_into().ok()?) as f64),
        "int" => Some(i32::from_le_bytes(b.get(..4)?.try_into().ok()?) as f64),
        _ => None,
    }
}

/// (min, max, NULL count, rows) of a column from a scan.
type ScanStats = (Option<f64>, Option<f64>, i64, i64);
/// (min, max, NULL count, rows, source) of a column.
type ColumnStats<'a> = (Option<f64>, Option<f64>, Option<i64>, i64, &'a str);
/// Lower bounds, upper bounds and NULL counts of one data file, by field id.
type FileMetrics = (HashMap<i64, D>, HashMap<i64, D>, HashMap<i64, D>);

fn kv_map(d: Option<&D>) -> HashMap<i64, D> {
    let mut out = HashMap::new();
    if let Some(D::Arr(items)) = d {
        for it in items {
            if let (Some(k), Some(v)) = (it.get("key").and_then(D::long), it.get("value")) {
                out.insert(k, v.clone());
            }
        }
    }
    out
}

pub fn plan_stats(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?;
    let exact = p.get("exact").and_then(sql::Param::int).unwrap_or(0) != 0;
    let lease = sql::lease()?;
    let root = path.trim_end_matches('/').to_string();
    let src = dataset::open(&lease, &root)?;
    let m = src.manifest.clone().unwrap_or(Json::Null);
    let fields: Vec<(String, String)> = m
        .get("value_fields")
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| {
            Some((
                f.get("name")?.as_str()?.to_string(),
                f.get("unit")
                    .and_then(Json::as_str)
                    .unwrap_or("")
                    .to_string(),
            ))
        })
        .collect();
    if fields.is_empty() {
        return Err("cell_dataset_stats needs the value columns of the cell manifest".into());
    }
    let from = format!("read_cell_dataset({})", lit(&root));
    let scan = |names: &[&(String, String)]| -> Result<HashMap<String, ScanStats>, E> {
        if names.is_empty() {
            return Ok(HashMap::new());
        }
        let parts: Vec<String> = names
            .iter()
            .map(|(n, _)| {
                let c = ident(n);
                format!("min({c})::DOUBLE, max({c})::DOUBLE, count(*) FILTER (WHERE {c} IS NULL)")
            })
            .collect();
        let r = sql::query_text_on(
            &lease,
            &format!("SELECT count(*), {} FROM {from}", parts.join(", ")),
        )?;
        let v = r.into_iter().next().unwrap_or_default();
        let f = |i: usize| v.get(i).cloned().flatten();
        let rows = f(0).and_then(|s| s.parse().ok()).unwrap_or(0);
        Ok(names
            .iter()
            .enumerate()
            .map(|(i, (n, _))| {
                (
                    n.clone(),
                    (
                        f(1 + 3 * i).and_then(|s| s.parse().ok()),
                        f(2 + 3 * i).and_then(|s| s.parse().ok()),
                        f(3 + 3 * i).and_then(|s| s.parse().ok()).unwrap_or(0),
                        rows,
                    ),
                )
            })
            .collect())
    };
    let mut out: HashMap<String, ColumnStats> = HashMap::new();
    let mut need_scan: Vec<&(String, String)> = Vec::new();
    if let (Layout::Iceberg { metadata }, false) = (&src.layout, exact) {
        let meta = dataset::metadata_json(&lease, metadata)?;
        let sid = meta
            .get("current-schema-id")
            .and_then(Json::as_i64)
            .unwrap_or(0);
        let schema: HashMap<String, (i64, String)> = meta
            .get("schemas")
            .and_then(Json::as_array)
            .and_then(|a| {
                a.iter()
                    .find(|s| s.get("schema-id").and_then(Json::as_i64) == Some(sid))
            })
            .and_then(|s| s.get("fields"))
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|f| {
                Some((
                    f.get("name")?.as_str()?.to_string(),
                    (f.get("id")?.as_i64()?, f.get("type")?.as_str()?.to_string()),
                ))
            })
            .collect();
        let store = Storage::for_path(&lease, &root, &writer::s3_params_pub(p))?;
        let files = iceberg_files(&lease, &store, &src)?;
        let rows: i64 = files
            .iter()
            .filter_map(|(_, d)| d.get("record_count").and_then(D::long))
            .sum();
        let per_file: Vec<FileMetrics> = files
            .iter()
            .map(|(_, d)| {
                (
                    kv_map(d.get("lower_bounds")),
                    kv_map(d.get("upper_bounds")),
                    kv_map(d.get("null_value_counts")),
                )
            })
            .collect();
        for f in &fields {
            let Some((id, ty)) = schema.get(&f.0) else {
                need_scan.push(f);
                continue;
            };
            let (mut lo, mut hi, mut nulls, mut complete) =
                (f64::INFINITY, f64::NEG_INFINITY, 0i64, true);
            for (l, u, n) in &per_file {
                match (l.get(id), u.get(id)) {
                    (Some(D::Bytes(a)), Some(D::Bytes(b))) => {
                        match (decode_bound(ty, a), decode_bound(ty, b)) {
                            (Some(a), Some(b)) => {
                                lo = lo.min(a);
                                hi = hi.max(b);
                            }
                            _ => complete = false,
                        }
                    }
                    _ => {
                        // a file with only NULLs has no bounds; otherwise metrics are missing
                        let all_null = n.get(id).and_then(D::long).is_some();
                        if !all_null {
                            complete = false;
                        }
                    }
                }
                match n.get(id).and_then(D::long) {
                    Some(k) => nulls += k,
                    None => complete = false,
                }
            }
            if complete && !files.is_empty() {
                out.insert(
                    f.0.clone(),
                    (
                        lo.is_finite().then_some(lo),
                        hi.is_finite().then_some(hi),
                        Some(nulls),
                        rows,
                        "manifest",
                    ),
                );
            } else {
                out.insert(f.0.clone(), (None, None, None, rows, "unavailable"));
            }
        }
    } else {
        need_scan.extend(fields.iter());
    }
    for (n, (lo, hi, nulls, rows)) in scan(&need_scan)? {
        out.insert(n, (lo, hi, Some(nulls), rows, "scan"));
    }
    let f = |v: Option<f64>| v.map_or("NULL::DOUBLE".into(), |x| format!("{x:?}::DOUBLE"));
    let rows: Vec<String> = fields
        .iter()
        .map(|(n, unit)| {
            let (lo, hi, nulls, rows, source) =
                out.get(n)
                    .cloned()
                    .unwrap_or((None, None, None, 0, "unavailable"));
            format!(
                "({}, {}, {}, {}, {}, {rows}::BIGINT, {})",
                lit(n),
                if unit.is_empty() {
                    "NULL::VARCHAR".into()
                } else {
                    lit(unit)
                },
                f(lo),
                f(hi),
                nulls.map_or("NULL::BIGINT".into(), |k| format!("{k}::BIGINT")),
                lit(source)
            )
        })
        .collect();
    Ok(Plan::Query(format!(
        "SELECT * FROM (VALUES {}) AS t(\"column\", unit, min, max, null_count, row_count, source)",
        rows.join(", ")
    )))
}
