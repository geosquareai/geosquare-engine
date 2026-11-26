//! `cell_compact_set` datasets: `manifest.json` and `data/compact.parquet` with exactly `gid`
//! and `level`, sorted by gid (geosquare-v2 `geosquare_v2.compact_store`). Reads check the
//! manifest, the columns, the row count, the levels and the rules, and refuse a set that
//! breaks one; they never repair it.

use crate::dataset::{self, join, MANIFEST_FILE};
use crate::sql::{self, lit, Lease, Params, Plan, E};
use serde_json::{json, Value as Json};

pub const DATA_FILE: &str = "data/compact.parquet";
pub const DATASET_TYPE: &str = "cell_compact_set";

/// Whether a manifest is a compact set.
pub fn is_compact(m: Option<&Json>) -> bool {
    m.and_then(|m| m.get("dataset_type")).and_then(Json::as_str) == Some(DATASET_TYPE)
}

fn boxed(e: Box<dyn std::error::Error + Send + Sync>) -> E {
    e.to_string().into()
}

/// write_compact_set(query, path, domain_code := ..., canonicalize := false,
///   mode := 'create' | 'overwrite')
///
/// The query gives a `gid` column (else its first column) and may give `level`, which must
/// match each GID. Without `canonicalize` a set that is not canonical is refused.
pub fn plan_write(p: &Params) -> Result<Plan, E> {
    let query = p.pos_text(0, "query")?;
    let path = p.pos_text(1, "path")?.trim_end_matches('/').to_string();
    let code = p
        .text("domain_code")
        .ok_or("write_compact_set needs domain_code := 'ID' (a compact set is in one domain)")?;
    let domain = crate::grid::domain_by_code(&code).map_err(|e| e.to_string())?;
    let canonicalize = p.int("canonicalize").is_some_and(|v| v != 0);
    let mode = p.text("mode").unwrap_or_else(|| "create".into()).to_ascii_lowercase();
    if !matches!(mode.as_str(), "create" | "overwrite") {
        return Err("mode must be 'create' or 'overwrite' (a compact set is rewritten whole)".into());
    }
    let (code, id) = (domain.code.to_string(), domain.id);
    Ok(Plan::Action(
        Box::new(move || write(&query, &path, &code, id as i64, canonicalize, &mode)),
        ["location", "rows", "levels_present"].iter().map(|s| s.to_string()).collect(),
    ))
}

fn write(query: &str, root: &str, code: &str, domain_id: i64, canonicalize: bool, mode: &str) -> Result<sql::TextRows, E> {
    let lease = sql::lease()?;
    if mode == "create" && dataset::read_text(&lease, &join(root, MANIFEST_FILE)).is_ok() {
        return Err(format!("{root} already exists; use mode := 'overwrite'").into());
    }
    let cols: Vec<String> = sql::query_text_on(&lease, &format!("SELECT column_name FROM (DESCRIBE {query})"))?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect();
    let gid_col = if cols.iter().any(|c| c == "gid") { "gid".to_string() } else { cols.first().cloned().ok_or("the query has no columns")? };
    let level_col = cols.iter().any(|c| c == "level").then_some("level");
    let rows = sql::query_text_on(
        &lease,
        &format!(
            "SELECT CAST({} AS VARCHAR), {} FROM ({query})",
            sql::ident(&gid_col),
            level_col.map_or("NULL".into(), |l| format!("CAST({l} AS BIGINT)"))
        ),
    )?;
    let mut items: Vec<(String, Option<i64>)> = Vec::with_capacity(rows.len());
    for r in rows {
        let g = r[0].clone().ok_or("a row has no gid")?;
        items.push((g, r[1].as_deref().and_then(|l| l.parse().ok())));
    }
    let gids: Vec<String> = if canonicalize {
        for (g, l) in &items {
            if let Some(l) = l {
                if *l != g.len() as i64 {
                    return Err(format!("level {l} does not match GID {g:?}").into());
                }
            }
        }
        crate::compact::canonicalize(items.iter().map(|(g, _)| g.as_str())).map_err(boxed)?
    } else {
        if let Some(why) = crate::compact::check(&items).map_err(boxed)? {
            return Err(format!("compact set rejected: {why} (pass canonicalize := true to canonicalize it)").into());
        }
        let mut g: Vec<String> = items.into_iter().map(|(g, _)| g).collect();
        g.sort();
        g
    };
    let mut levels: Vec<usize> = gids.iter().map(String::len).collect();
    levels.sort_unstable();
    levels.dedup();
    let manifest = json!({
        "manifest_version": "1.0",
        "dataset_type": DATASET_TYPE,
        "grid_system": "geosquare",
        "grid_version": "v2",
        "domain_code": code,
        "domain_id": domain_id,
        "levels_present": levels,
        "identity": {"gid_column": "gid", "level_column": "level", "unique_gid": true, "mixed_levels": true},
        "storage": {"parquet_compression": "zstd", "sorted_by": ["gid"], "row_count": gids.len(), "partitioning": []},
        "provenance": {
            "created_at": crate::writer::iso_now(),
            "generator": "geosquare_v2 DuckDB extension",
            "rules": "docs/cell-dataset-contract.md section 2",
        },
    });
    crate::writer::ensure_dir(&join(root, "data"))?;
    let list = gids.iter().map(|g| lit(g)).collect::<Vec<_>>().join(", ");
    let source = if gids.is_empty() {
        "SELECT NULL::VARCHAR AS gid, NULL::TINYINT AS level LIMIT 0".to_string()
    } else {
        format!("SELECT gid, CAST(length(gid) AS TINYINT) AS level FROM (SELECT unnest([{list}]::VARCHAR[]) AS gid) ORDER BY gid")
    };
    sql::exec_on(
        &lease,
        &format!("COPY ({source}) TO {} (FORMAT parquet, COMPRESSION zstd)", lit(&join(root, DATA_FILE))),
    )?;
    crate::writer::write_json(&lease, &join(root, MANIFEST_FILE), &manifest)?;
    Ok(vec![vec![
        Some(root.to_string()),
        Some(gids.len().to_string()),
        Some(levels.iter().map(|l| l.to_string()).collect::<Vec<_>>().join(",")),
    ]])
}

/// The checks a read makes: (check, ok, detail) rows. Used by read_compact_set (which
/// refuses on the first failure) and validate_cell_dataset.
pub fn checks(lease: &Lease, root: &str, m: &Json) -> Result<Vec<(String, bool, String)>, E> {
    let mut out = Vec::new();
    let mixed = m.pointer("/identity/mixed_levels") == Some(&Json::Bool(true));
    out.push(("manifest".into(), mixed, if mixed { "cell_compact_set, mixed_levels true".into() } else { "identity.mixed_levels must be true for cell_compact_set".into() }));
    let file = lit(&join(root, DATA_FILE));
    let cols: Vec<String> = sql::query_text_on(lease, &format!("SELECT column_name FROM (DESCRIBE SELECT * FROM read_parquet({file}))"))?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect();
    let cols_ok = cols == ["gid", "level"];
    out.push(("columns".into(), cols_ok, format!("columns {cols:?}; must be exactly [\"gid\", \"level\"]")));
    if !cols_ok {
        return Ok(out);
    }
    let rows = sql::query_text_on(lease, &format!("SELECT gid, CAST(level AS BIGINT) FROM read_parquet({file})"))?;
    let declared = m.pointer("/storage/row_count").and_then(Json::as_u64);
    out.push((
        "row_count".into(),
        declared == Some(rows.len() as u64),
        format!("{} rows; manifest says {}", rows.len(), declared.map_or("nothing".into(), |d| d.to_string())),
    ));
    let items: Vec<(String, Option<i64>)> = rows
        .iter()
        .map(|r| (r[0].clone().unwrap_or_default(), r[1].as_deref().and_then(|l| l.parse().ok())))
        .collect();
    let bad_level = items.iter().filter(|(g, l)| *l != Some(g.len() as i64)).count();
    let mut levels: Vec<i64> = items.iter().filter_map(|(_, l)| *l).collect();
    levels.sort_unstable();
    levels.dedup();
    let present: Vec<i64> = m.get("levels_present").and_then(Json::as_array).into_iter().flatten().filter_map(Json::as_i64).collect();
    out.push((
        "levels".into(),
        bad_level == 0 && levels == present,
        format!("{bad_level} rows whose level does not match the GID; levels {levels:?}, levels_present {present:?}"),
    ));
    let why = crate::compact::check(&items).map_err(boxed)?;
    out.push(("canonical".into(), why.is_none(), why.unwrap_or_else(|| "canonical: no duplicate, no row inside another, no complete child group".into())));
    Ok(out)
}

/// read_compact_set(path, level := NULL): the rows (gid, level), or with `level` every cell
/// of the set at that level (gid, packed_id). The set is checked first.
pub fn plan_read(p: &Params) -> Result<Plan, E> {
    let path = p.pos_text(0, "path")?.trim_end_matches('/').to_string();
    read_plan(&path, p.int("level"))
}

pub fn read_plan(root: &str, level: Option<i64>) -> Result<Plan, E> {
    let lease = sql::lease()?;
    let m: Json = serde_json::from_str(&dataset::read_text(&lease, &join(root, MANIFEST_FILE))?)?;
    if !is_compact(Some(&m)) {
        return Err(format!("{root} is not a cell_compact_set (dataset_type {:?})", m.get("dataset_type")).into());
    }
    if let Some((c, _, d)) = checks(&lease, root, &m)?.into_iter().find(|(_, ok, _)| !ok) {
        return Err(format!("{root}: stored compact set rejected ({c}): {d}").into());
    }
    let file = lit(&join(root, DATA_FILE));
    let Some(level) = level else {
        return Ok(Plan::Query(format!("SELECT gid, level FROM read_parquet({file}) ORDER BY gid")));
    };
    if !(0..=14).contains(&level) {
        return Err("level must be 0 to 14".into());
    }
    let deeper = sql::query_text_on(&lease, &format!("SELECT max(level) FROM read_parquet({file})"))?;
    if let Some(Some(max)) = deeper.first().and_then(|r| r.first()) {
        if max.parse::<i64>().unwrap_or(0) > level {
            return Err(format!("level {level} is below the deepest row (level {max}); a compact set expands down, it does not roll up").into());
        }
    }
    let code = m.get("domain_code").and_then(Json::as_str).ok_or("the manifest has no domain_code")?;
    Ok(Plan::Query(format!(
        "WITH RECURSIVE e(gid) AS (SELECT gid FROM read_parquet({file}) \
         UNION ALL SELECT unnest(children(gid)) FROM e WHERE length(gid) < {level}) \
         SELECT gid, pack({}, gid) AS packed_id FROM e WHERE length(gid) = {level} ORDER BY packed_id",
        lit(code)
    )))
}
