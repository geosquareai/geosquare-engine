//! aggregate_geometries_to_cells: features with a geometry (points, lines, polygons) and
//! values, allocated to cells and aggregated by value semantics, as
//! `geosquare_v2.aggregation.aggregate_geometry_table_to_cells` does.
//!
//! Each feature gives its cells through `geometry_to_cells` with `share`, its share in the cell
//! (polygon area share, line length ratio, points in the cell). Then per cell:
//! COUNT counts the feature records; TOTAL sums value x share; DENSITY, MEASUREMENT and RATE
//! take the share-weighted mean (RATE with `rate_parts` sums numerator and denominator x share
//! and divides); CATEGORICAL picks the class of the largest summed share (`category_rule`
//! `majority_weighted`) or of the most records (`centroid_category`), a tie going to the
//! greatest class as text, and adds `<column>_share`; ORDINAL picks the first class of
//! `ordinal_order` present; RANGE pairs (`range_columns`) keep the lowest lower bound and the
//! highest upper bound. `feature_count` counts the contribution records in the cell (each
//! point is one; `id_column` is only kept out of the value columns).

use crate::metadata::PerColumn;
use crate::sql::{self, ident, lit, Params, Plan, E};
use serde_json::Value as Json;

fn json_map(text: Option<String>, what: &str) -> Result<serde_json::Map<String, Json>, E> {
    match text {
        None => Ok(Default::default()),
        Some(t) => match serde_json::from_str::<Json>(&t) {
            Ok(Json::Object(m)) => Ok(m),
            _ => Err(format!("{what} must be a JSON object").into()),
        },
    }
}

fn pair(v: &Json, what: &str, key: &str) -> Result<(String, String), E> {
    match v.as_array().map(Vec::as_slice) {
        Some([Json::String(a), Json::String(b)]) => Ok((a.clone(), b.clone())),
        _ => Err(format!("{what} {key:?} must be a pair of column names [\"a\", \"b\"]").into()),
    }
}

/// The class picked from a `list(struct_pack(c := ..., w := ...))` column, by `order`.
fn pick(list: &str, order: &str) -> String {
    format!(
        "(SELECT u.c FROM (SELECT unnest({list}, recursive := true)) AS u GROUP BY u.c ORDER BY {order} LIMIT 1)"
    )
}

pub fn plan(p: &Params) -> Result<Plan, E> {
    let query = p.pos_text(0, "query")?;
    let code = p.text("domain_code").ok_or("aggregate_geometries_to_cells needs domain_code := 'ID'")?;
    let d = crate::grid::domain_by_code(&code).map_err(|e| e.to_string())?;
    let level = p.int("level").ok_or("aggregate_geometries_to_cells needs level := <0..14>")?;
    if !(0..=14).contains(&level) {
        return Err("level must be 0 to 14".into());
    }
    let geom_col = p.text("geometry_column").unwrap_or_else(|| "geometry".into());
    let crs = p.text("source_crs").unwrap_or_else(|| "EPSG:4326".into());
    let id_col = p.text("id_column");
    let category_rule = p.text("category_rule").unwrap_or_else(|| "majority_weighted".into());
    if !matches!(category_rule.as_str(), "majority_weighted" | "centroid_category") {
        return Err("category_rule must be 'majority_weighted' or 'centroid_category'".into());
    }
    let semantics = PerColumn::parse(p.text("value_semantics"), "value_semantics")?;
    let boundary = crate::dataset::boundary_args(p, d)?;
    let rate_parts = json_map(p.text("rate_parts"), "rate_parts")?;
    let ranges = json_map(p.text("range_columns"), "range_columns")?;
    let orders = json_map(p.text("ordinal_order"), "ordinal_order")?;

    let lease = sql::lease()?;
    let cols: Vec<String> = sql::query_text_on(&lease, &format!("SELECT column_name FROM (DESCRIBE {query})"))?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect();
    if !cols.contains(&geom_col) {
        return Err(format!("the query has no geometry column {geom_col:?} (geometry_column := ...)").into());
    }
    let mut parts: Vec<&str> = Vec::new();
    for (k, v) in rate_parts.iter().chain(ranges.iter()) {
        let (a, b) = pair(v, "rate_parts / range_columns", k)?;
        for c in [&a, &b] {
            if !cols.contains(c) {
                return Err(format!("column {c:?} of {k:?} is not in the query").into());
            }
        }
        parts.extend(cols.iter().filter(|c| **c == a || **c == b).map(String::as_str));
    }
    let r = |c: &str| format!("x.{}", ident(c));
    let mut aggs: Vec<String> = Vec::new();
    let mut picks: Vec<String> = Vec::new();
    let mut extras: Vec<String> = Vec::new();
    for c in &cols {
        if *c == geom_col || Some(c) == id_col.as_ref() || parts.contains(&c.as_str()) {
            continue;
        }
        let sem = semantics
            .get(c)
            .ok_or_else(|| format!("column {c} has no value_semantics: name it in value_semantics or leave it out of the query"))?
            .to_ascii_uppercase();
        let v = r(c);
        let name = ident(c);
        match sem.as_str() {
            "COUNT" => aggs.push(format!("CAST(sum(x.__c.records) FILTER (WHERE {v} IS NOT NULL) AS BIGINT) AS {name}")),
            "TOTAL" => aggs.push(format!("CAST(sum({v} * x.__c.share) AS DOUBLE) AS {name}")),
            "DENSITY" | "MEASUREMENT" | "RATE" => aggs.push(format!(
                "CAST(sum({v} * x.__c.share) / nullif(sum(CASE WHEN {v} IS NOT NULL THEN x.__c.share END), 0) AS DOUBLE) AS {name}"
            )),
            "CATEGORICAL" => {
                let w = if category_rule == "centroid_category" { "x.__c.records" } else { "x.__c.share" };
                aggs.push(format!(
                    "list(struct_pack(c := {v}, w := CAST({w} AS DOUBLE))) FILTER (WHERE {v} IS NOT NULL) AS {name}"
                ));
                let l = format!("g.{name}");
                picks.push(format!("{} AS {name}", pick(&l, "sum(u.w) DESC, CAST(u.c AS VARCHAR) DESC")));
                extras.push(format!(
                    "(SELECT max(s) / nullif(sum(s), 0) FROM (SELECT sum(u.w) AS s FROM (SELECT unnest({l}, recursive := true)) AS u GROUP BY u.c)) AS {}",
                    ident(&format!("{c}_share"))
                ));
            }
            "ORDINAL" => {
                let order = orders
                    .get(c)
                    .and_then(Json::as_array)
                    .ok_or_else(|| format!("ORDINAL column {c} needs ordinal_order := '{{\"{c}\": [\"first\", \"second\", ...]}}'"))?;
                let list = order.iter().map(|o| lit(&o.as_str().map_or(o.to_string(), str::to_string))).collect::<Vec<_>>().join(", ");
                aggs.push(format!("list(struct_pack(c := {v}, w := CAST(x.__c.share AS DOUBLE))) FILTER (WHERE {v} IS NOT NULL) AS {name}"));
                let l = format!("g.{name}");
                picks.push(format!(
                    "{} AS {name}",
                    pick(&l, &format!("coalesce(list_position([{list}], CAST(u.c AS VARCHAR)), {}) ASC, sum(u.w) DESC, CAST(u.c AS VARCHAR) ASC", order.len() + 1))
                ));
            }
            "RANGE" => return Err(format!("RANGE column {c}: name its bounds in range_columns := '{{\"{c}\": [\"lower_column\", \"upper_column\"]}}'").into()),
            other => return Err(format!("column {c}: unknown value semantics {other:?}").into()),
        }
    }
    for (k, v) in &rate_parts {
        let (a, b) = pair(v, "rate_parts", k)?;
        aggs.push(format!(
            "CAST(sum({} * x.__c.share) / CASE WHEN sum({} * x.__c.share) = 0 THEN error('rate {k}: the allocated denominator is 0') ELSE sum({} * x.__c.share) END AS DOUBLE) AS {}",
            r(&a), r(&b), r(&b), ident(k)
        ));
    }
    for (k, v) in &ranges {
        let (a, b) = pair(v, "range_columns", k)?;
        aggs.push(format!("min({}) AS {}", r(&a), ident(&a)));
        aggs.push(format!("max({}) AS {}", r(&b), ident(&b)));
    }
    // As the reference: contribution records (a point is one record), not distinct features.
    let feature = "CAST(sum(x.__c.records) AS BIGINT)";
    let aggs = if aggs.is_empty() { String::new() } else { format!("{}, ", aggs.join(", ")) };
    let grouped = format!(
        "WITH f AS (SELECT *, row_number() OVER () AS __fid FROM ({query})), \
         x AS (SELECT f.*, unnest(geometry_to_cells({}, CAST(f.{} AS VARCHAR), {level}, {}{boundary})) AS __c FROM f WHERE f.{} IS NOT NULL) \
         SELECT x.__c.gid AS gid, x.__c.packed_id AS packed_id, {aggs}{feature} AS feature_count FROM x GROUP BY 1, 2",
        lit(d.code),
        ident(&geom_col),
        lit(&crs),
        ident(&geom_col)
    );
    Ok(Plan::Query(if picks.is_empty() {
        format!("{grouped} ORDER BY 2")
    } else {
        let extras = extras.iter().map(|e| format!(", {e}")).collect::<String>();
        format!("SELECT * REPLACE ({}){extras} FROM ({grouped}) AS g ORDER BY packed_id", picks.join(", "))
    }))
}
