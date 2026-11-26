//! Crosswalk 1.0 (geosquare-v2 docs/crosswalk-format.md, reference `geosquare_v2.crosswalk`):
//! a table (sumber_id, gid, bobot) linking source units (e.g. villages, or V1 GIDs) to V2
//! cells, with a manifest JSON. `bobot` is the share of a source unit in a cell; the shares of
//! one unit sum to 1 within 1e-9.

use crate::sql::{self, ident, lit, Params, Plan, E};
use serde_json::Value as Json;

const WEIGHT_KINDS: [&str; 4] = ["luas", "bangunan", "populasi", "lainnya"];
/// The eleven ASEAN domains of registry 2.0.0.
const DOMAINS: [&str; 11] = ["BN", "ID", "KH", "LA", "MM", "MY", "PH", "SG", "TH", "TL", "VN"];
const REQUIRED: [&str; 8] = [
    "format_version",
    "spec_version",
    "domain_code",
    "level",
    "weight_kind",
    "source_description",
    "created_at",
    "registry_version",
];

/// Check the manifest and return its level.
fn check_manifest(lease: &sql::Lease, m: &Json) -> Result<i64, E> {
    let o = m.as_object().ok_or("manifest must be an object")?;
    let mut missing: Vec<&str> = REQUIRED.iter().copied().filter(|k| !o.contains_key(*k)).collect();
    missing.sort_unstable();
    if !missing.is_empty() {
        return Err(format!("manifest is missing required field(s): {}", missing.join(", ")).into());
    }
    if m["format_version"] != "1.0" {
        return Err("manifest format_version must be '1.0'".into());
    }
    if m["spec_version"] != "2.0" {
        return Err("manifest spec_version must be '2.0'".into());
    }
    if !m["domain_code"].as_str().is_some_and(|d| DOMAINS.contains(&d)) {
        return Err(format!("manifest domain_code must be one of {}", DOMAINS.join(", ")).into());
    }
    let level = match &m["level"] {
        Json::Number(n) if n.is_i64() && (0..=14).contains(&n.as_i64().unwrap()) => n.as_i64().unwrap(),
        _ => return Err("manifest level must be an integer in [0, 14]".into()),
    };
    if !m["weight_kind"].as_str().is_some_and(|w| WEIGHT_KINDS.contains(&w)) {
        let mut k = WEIGHT_KINDS.to_vec();
        k.sort_unstable();
        return Err(format!("manifest weight_kind must be one of {}", k.join(", ")).into());
    }
    for f in ["source_description", "registry_version"] {
        if !m[f].as_str().is_some_and(|s| !s.trim().is_empty()) {
            return Err(format!("manifest {f} must be a non-empty string").into());
        }
    }
    let created = m["created_at"].as_str().filter(|s| !s.trim().is_empty()).ok_or("manifest created_at must be an ISO 8601 string")?;
    let ok = sql::query_text_on(
        lease,
        &format!("SELECT TRY_CAST(replace({}, 'Z', '+00:00') AS TIMESTAMPTZ) IS NOT NULL", lit(created)),
    )?;
    if ok.first().and_then(|r| r.first()).and_then(|v| v.as_deref()) != Some("true") {
        return Err("manifest created_at must be an ISO 8601 string".into());
    }
    Ok(level)
}

/// A regular expression for a valid GID of `level`: each character from the alphabet of its
/// level step (5x5 or 2x2).
fn gid_pattern(level: i64) -> String {
    let mut p = String::from("^");
    for i in 0..level as usize {
        p.push_str(if geosquare_core::codec::SUBDIVISIONS[i] == 5 { "[23456789CEFGHJLMNPQRTVWXY]" } else { "[2345]" });
    }
    p.push('$');
    p
}

fn manifest_json(lease: &sql::Lease, text: &str) -> Result<Json, E> {
    let t = text.trim_start();
    let raw = if t.starts_with('{') { text.to_string() } else { crate::dataset::read_text(lease, text)? };
    serde_json::from_str(&raw).map_err(|e| format!("manifest is not JSON: {e}").into())
}

/// validate_crosswalk(query, manifest := '<JSON or path>') -> (sources, rows, cells)
///
/// The query gives sumber_id, gid and bobot. The first broken rule raises an error naming
/// the row (in query order) as the reference does.
pub fn plan_validate(p: &Params) -> Result<Plan, E> {
    let query = p.pos_text(0, "query")?;
    let lease = sql::lease()?;
    let m = manifest_json(&lease, &p.text("manifest").ok_or("validate_crosswalk needs manifest := '<JSON>' or a manifest file path")?)?;
    let level = check_manifest(&lease, &m)?;
    let cols: Vec<String> = sql::query_text_on(&lease, &format!("SELECT column_name FROM (DESCRIBE {query})"))?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect();
    let mut missing: Vec<&str> = ["bobot", "gid", "sumber_id"].into_iter().filter(|c| !cols.iter().any(|x| x == c)).collect();
    missing.sort_unstable();
    if !missing.is_empty() {
        return Err(format!("rows are missing required field(s): {}", missing.join(", ")).into());
    }
    let pat = lit(&gid_pattern(level));
    // First failing row (1-based, query order) per rule.
    let r = sql::query_text_on(
        &lease,
        &format!(
            "WITH r AS (SELECT row_number() OVER () AS n, sumber_id, gid, bobot FROM ({query})), \
             t AS (SELECT n, CAST(sumber_id AS VARCHAR) AS s, typeof(sumber_id) AS st, CAST(gid AS VARCHAR) AS g, typeof(gid) AS gt, \
                   TRY_CAST(bobot AS DOUBLE) AS w, typeof(bobot) AS wt, \
                   count(*) OVER (PARTITION BY sumber_id, gid ORDER BY n ROWS UNBOUNDED PRECEDING) AS dup FROM r) \
             SELECT \
               min(n) FILTER (WHERE st <> 'VARCHAR' OR s IS NULL OR trim(s) = ''), \
               min(n) FILTER (WHERE gt <> 'VARCHAR' OR g IS NULL OR NOT regexp_full_match(g, {pat})), \
               min(n) FILTER (WHERE wt NOT IN ('DOUBLE','FLOAT','INTEGER','BIGINT','SMALLINT','TINYINT','HUGEINT','UBIGINT','UINTEGER','USMALLINT','UTINYINT') AND wt NOT LIKE 'DECIMAL%'), \
               min(n) FILTER (WHERE w IS NULL OR NOT isfinite(w)), \
               min(n) FILTER (WHERE w <= 0), \
               min(n) FILTER (WHERE dup > 1), \
               count(*) FROM t"
        ),
    )?;
    let v: Vec<Option<i64>> = r[0].iter().map(|x| x.as_deref().and_then(|s| s.parse().ok())).collect();
    // The reference checks row by row: report the earliest failing row, then its rule.
    let row_info = |n: i64| -> Result<(String, String), E> {
        let x = sql::query_text_on(
            &lease,
            &format!("SELECT CAST(sumber_id AS VARCHAR), CAST(gid AS VARCHAR) FROM (SELECT row_number() OVER () AS n, * FROM ({query})) WHERE n = {n}"),
        )?;
        Ok((x[0][0].clone().unwrap_or_default(), x[0][1].clone().unwrap_or_default()))
    };
    let rules = ["sumber_id", "gid", "bobot_type", "bobot_finite", "bobot_positive", "duplicate"];
    if let Some((rule, n)) = rules.iter().zip(&v[..6]).filter_map(|(r, n)| n.map(|n| (*r, n))).min_by_key(|(_, n)| *n) {
        let (s, g) = row_info(n)?;
        return Err(match rule {
            "sumber_id" => format!("row {n} has an empty sumber_id"),
            "gid" => match geosquare_core::codec::decode(&g) {
                Ok(_) if regexp_ok(&g) => format!(
                    "row {n} for sumber_id {} has gid {} at level {}; manifest requires level {level}",
                    py_repr(&s),
                    py_repr(&g),
                    g.len()
                ),
                Ok(_) => format!("row {n} for sumber_id {} has an invalid gid {}", py_repr(&s), py_repr(&g)),
                Err(e) => {
                    // The reference's reason: the first character outside its level's alphabet.
                    let why = g
                        .chars()
                        .enumerate()
                        .find(|(i, c)| *i < 14 && !regexp_ok(&g[..*i + c.len_utf8()].chars().skip(0).collect::<String>()))
                        .map(|(i, c)| format!("character {} is invalid at level {} (base {})", py_repr(&c.to_string()), i + 1, geosquare_core::codec::SUBDIVISIONS[i]))
                        .unwrap_or_else(|| e.to_string());
                    format!("row {n} for sumber_id {} has an invalid gid {}: {why}", py_repr(&s), py_repr(&g))
                }
            },
            "bobot_type" => format!("row {n} for sumber_id {} has bobot that is not a number", py_repr(&s)),
            "bobot_finite" => format!("row {n} for sumber_id {} has bobot that is not finite", py_repr(&s)),
            "bobot_positive" => format!("row {n} for sumber_id {} has bobot <= 0", py_repr(&s)),
            _ => format!("row {n} duplicates (sumber_id, gid) pair ({}, {})", py_repr(&s), py_repr(&g)),
        }
        .into());
    }
    if v[6] == Some(0) {
        return Err("crosswalk has no rows".into());
    }
    // In first-seen order of the source units, as the reference reports them.
    let sums = sql::query_text_on(
        &lease,
        &format!(
            "SELECT CAST(sumber_id AS VARCHAR), CAST(sum(CAST(bobot AS DOUBLE)) AS VARCHAR) \
             FROM (SELECT row_number() OVER () AS n, sumber_id, bobot FROM ({query})) GROUP BY sumber_id \
             HAVING abs(sum(CAST(bobot AS DOUBLE)) - 1) > 1e-9 ORDER BY min(n) LIMIT 1"
        ),
    )?;
    if let Some(r) = sums.first() {
        let total: f64 = r[1].as_deref().and_then(|t| t.parse().ok()).unwrap_or(f64::NAN);
        return Err(format!(
            "weights for sumber_id {} sum to {}, expected 1 within 1e-9",
            py_repr(r[0].as_deref().unwrap_or_default()),
            g17(total)
        )
        .into());
    }
    Ok(Plan::Query(format!(
        "SELECT count(DISTINCT sumber_id) AS sources, count(*) AS \"rows\", count(DISTINCT gid) AS cells FROM ({query})"
    )))
}

/// Python's repr of a str: single quotes unless the text has one and no double quote.
fn py_repr(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let body: String = s
        .chars()
        .flat_map(|c| match c {
            '\\' => vec!['\\', '\\'],
            c if c == q => vec!['\\', c],
            c => vec![c],
        })
        .collect();
    format!("{q}{body}{q}")
}

/// Python's "%.17g".
fn g17(x: f64) -> String {
    if x == 0.0 || !x.is_finite() {
        return format!("{x}");
    }
    let sci = format!("{:.16e}", x);
    let (m, e) = sci.split_once('e').unwrap();
    let e: i32 = e.parse().unwrap();
    let strip = |t: &str| -> String {
        if t.contains('.') { t.trim_end_matches('0').trim_end_matches('.').to_string() } else { t.to_string() }
    };
    if !(-5..17).contains(&e) {
        format!("{}e{}{:02}", strip(m), if e < 0 { '-' } else { '+' }, e.abs())
    } else {
        strip(&format!("{:.*}", (16 - e).max(0) as usize, x))
    }
}

fn regexp_ok(g: &str) -> bool {
    g.chars().enumerate().all(|(i, c)| {
        i < 14
            && if geosquare_core::codec::SUBDIVISIONS[i] == 5 {
                "23456789CEFGHJLMNPQRTVWXY".contains(c)
            } else {
                "2345".contains(c)
            }
    })
}

/// polygon_crosswalk(query, domain_code := ..., level := ..., id_column := 'sumber_id',
///   geometry_column := 'geometry', source_crs := 'EPSG:4326') -> (sumber_id, gid, bobot)
///
/// One row per source polygon and overlapped cell; `bobot` is the overlap area over the
/// area the cells cover (the polygon's area share), so each source sums to 1.
pub fn plan_polygons(p: &Params) -> Result<Plan, E> {
    let query = p.pos_text(0, "query")?;
    let code = p.text("domain_code").ok_or("polygon_crosswalk needs domain_code := 'ID'")?;
    let d = crate::grid::domain_by_code(&code).map_err(|e| e.to_string())?;
    let level = p.int("level").ok_or("polygon_crosswalk needs level := <0..14>")?;
    if !(0..=14).contains(&level) {
        return Err("level must be 0 to 14".into());
    }
    let id = ident(&p.text("id_column").unwrap_or_else(|| "sumber_id".into()));
    let geom = ident(&p.text("geometry_column").unwrap_or_else(|| "geometry".into()));
    let crs = lit(&p.text("source_crs").unwrap_or_else(|| "EPSG:4326".into()));
    let lease = sql::lease()?;
    let bad = sql::query_text_on(
        &lease,
        &format!(
            "SELECT \
               (SELECT any_value(CAST({id} AS VARCHAR)) FROM ({query}) WHERE {id} IS NULL OR trim(CAST({id} AS VARCHAR)) = ''), \
               (SELECT count(*) FROM ({query}) WHERE {id} IS NULL OR trim(CAST({id} AS VARCHAR)) = ''), \
               (SELECT any_value(CAST({id} AS VARCHAR)) FROM (SELECT {id} FROM ({query}) GROUP BY 1 HAVING count(*) > 1)), \
               (SELECT any_value(CAST({id} AS VARCHAR)) FROM ({query}) WHERE NOT regexp_matches(upper(ltrim(CAST({geom} AS VARCHAR))), '^(SRID=[0-9]+;\\s*)?(MULTI)?POLYGON'))"
        ),
    )?;
    if bad[0][1].as_deref() != Some("0") {
        return Err("polygon sumber_id must be a non-empty string".into());
    }
    if let Some(s) = &bad[0][2] {
        return Err(format!("duplicate sumber_id {s:?}").into());
    }
    if let Some(s) = &bad[0][3] {
        return Err(format!("sumber_id {s:?} is not a polygon").into());
    }
    let out = format!(
        "SELECT s AS sumber_id, c.gid AS gid, c.share AS bobot FROM \
         (SELECT CAST({id} AS VARCHAR) AS s, unnest(geometry_to_cells({}, CAST({geom} AS VARCHAR), {level}, {crs})) AS c FROM ({query})) \
         WHERE c.share > 0 ORDER BY sumber_id, gid",
        lit(d.code)
    );
    let empty = sql::query_text_on(
        &lease,
        &format!("SELECT any_value(s) FROM (SELECT CAST({id} AS VARCHAR) AS s, len(geometry_to_cells({}, CAST({geom} AS VARCHAR), {level}, {crs})) AS n FROM ({query})) WHERE n = 0", lit(d.code)),
    )?;
    if let Some(s) = empty.first().and_then(|r| r[0].clone()) {
        return Err(format!("sumber_id {s:?} has no area").into());
    }
    Ok(Plan::Query(out))
}

#[cfg(test)]
mod tests {
    #[test]
    fn python_formats() {
        assert_eq!(super::g17(0.9), "0.90000000000000002");
        assert_eq!(super::g17(1.5), "1.5");
        assert_eq!(super::py_repr("a'b"), "\"a'b\"");
        assert_eq!(super::py_repr("ab"), "'ab'");
    }
}
