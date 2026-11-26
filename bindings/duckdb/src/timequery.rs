//! Reading time cubes (`cell_time_values`): the period filter, file pruning by period and
//! the roll-up in time.
//!
//! Labels of one period type sort in time order as text, and a coarser label is a prefix of
//! the finer labels inside it, so a period range is a plain text range on the `period`
//! column: `period >= from AND period < to`. Data files carry their period bounds (Iceberg
//! manifest metrics) or their period chunk in the file name (partitioned), and are skipped
//! when they cannot hold a period of the range.

use crate::sql::{self, lit, Plan, Params, E};
use crate::timecube::{next_label, nests, parse_label, rollup_sql, time_rule, Grain};
use serde_json::Value as Json;

/// The period type of a time cube manifest; None for cell_values datasets.
pub fn grain_of(manifest: Option<&Json>) -> Result<Option<Grain>, E> {
    let Some(m) = manifest else { return Ok(None) };
    if m.get("dataset_type").and_then(Json::as_str) != Some("cell_time_values") {
        return Ok(None);
    }
    Grain::new(
        m.get("period_type").and_then(Json::as_str).unwrap_or(""),
        m.get("bucket_hours").and_then(Json::as_i64),
    )
    .map(Some)
}

/// The grain of a label written at any period type, from its length (hours as 1-hour buckets).
fn label_grain(label: &str) -> Result<Grain, E> {
    match label.len() {
        4 => Grain::new("year", None),
        7 => Grain::new("month", None),
        10 => Grain::new("day", None),
        13 => Grain::new("hour", Some(1)),
        _ => Err(format!("{label:?} is not a period label (YYYY, YYYY-MM, YYYY-MM-DD or YYYY-MM-DDTHH)").into()),
    }
}

/// Check a label of any period type (YYYY, YYYY-MM, YYYY-MM-DD or YYYY-MM-DDTHH).
pub fn check_any_label(label: &str) -> Result<(), E> {
    parse_label(label, &label_grain(label)?).map(|_| ())
}

/// A half-open period range `[from, to)`; either end may be open.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PeriodRange {
    pub from: Option<String>,
    pub to: Option<String>,
}

impl PeriodRange {
    /// SQL condition on the `period` column.
    pub fn sql(&self, col: &str) -> String {
        let mut c = Vec::new();
        if let Some(f) = &self.from {
            c.push(format!("{col} >= {}", lit(f)));
        }
        if let Some(t) = &self.to {
            c.push(format!("{col} < {}", lit(t)));
        }
        if c.is_empty() {
            "TRUE".into()
        } else {
            c.join(" AND ")
        }
    }

    /// Whether a file whose periods lie in `[lo, hi]` can hold a period of the range.
    pub fn overlaps(&self, lo: &str, hi: &str) -> bool {
        self.to.as_deref().is_none_or(|t| lo < t) && self.from.as_deref().is_none_or(|f| hi >= f)
    }

    /// Whether the chunk of labels starting with `prefix` can hold a period of the range.
    pub fn overlaps_chunk(&self, prefix: &str) -> bool {
        let from_ok = self.from.as_deref().is_none_or(|f| {
            let n = prefix.len().min(f.len());
            prefix >= &f[..n]
        });
        from_ok && self.to.as_deref().is_none_or(|t| prefix < t)
    }
}

/// What a read asks of a time cube: the period range and the roll-up in time.
#[derive(Debug, Clone)]
pub struct TimeQuery {
    pub grain: Grain,
    pub range: Option<PeriodRange>,
    /// Target grain of `time_rollup`; `Some(None)` is `time_rollup := 'all'`.
    pub rollup: Option<Option<Grain>>,
    pub rule: Option<String>,
}

impl TimeQuery {
    /// Whether the rows the read returns have a `period` column.
    pub fn has_period(&self) -> bool {
        !matches!(self.rollup, Some(None))
    }
}

const PARAMS: [&str; 5] = ["period", "period_from", "period_to", "time_rollup", "time_rule"];

/// The time parameters of a read: `period` (one label, of the dataset's period type or
/// coarser: '2026-02' is every day of February), `period_from` (inclusive) and `period_to`
/// (exclusive), `time_rollup` ('year', 'month', 'day', 'hour:N' or 'all') and `time_rule`
/// (one of sum, mean, min, max, or `{column: rule}`).
///
/// `static_ok`: a dataset without periods ignores them (query_cell_datasets joins it to every
/// period) instead of refusing them.
pub fn parse(p: &Params, manifest: Option<&Json>, static_ok: bool) -> Result<Option<TimeQuery>, E> {
    let Some(grain) = grain_of(manifest)? else {
        if let Some(k) = PARAMS.iter().find(|k| p.get(k).is_some()).filter(|_| !static_ok) {
            return Err(format!("{k} is for time cubes (cell_time_values); this dataset has no periods").into());
        }
        return Ok(None);
    };
    let check = |name: &str, label: &str| -> Result<Grain, E> {
        let g = label_grain(label).map_err(|e| format!("{name}: {e}"))?;
        if g.width() > grain.width() {
            return Err(format!("{name} {label:?} is finer than the dataset's {} periods", grain.spec()).into());
        }
        // A label of the dataset's own type must be one of its periods (hour buckets).
        let g = if g.width() == grain.width() { grain } else { g };
        parse_label(label, &g).map_err(|e| format!("{name}: {e}"))?;
        Ok(g)
    };
    let range = match (p.text("period"), p.text("period_from"), p.text("period_to")) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => {
            return Err("pass period, or period_from / period_to, not both".into())
        }
        (Some(l), None, None) => {
            let g = check("period", &l)?;
            Some(PeriodRange {
                to: next_label(&l, &g).ok(),
                from: Some(l),
            })
        }
        (None, from, to) => {
            if let Some(f) = &from {
                check("period_from", f)?;
            }
            if let Some(t) = &to {
                check("period_to", t)?;
            }
            if let (Some(f), Some(t)) = (&from, &to) {
                if f >= t {
                    return Err(format!("period_from {f} must be before period_to {t} (the range is half-open)").into());
                }
            }
            (from.is_some() || to.is_some()).then_some(PeriodRange { from, to })
        }
    };
    let rollup = match p.text("time_rollup") {
        None => None,
        Some(t) if t.eq_ignore_ascii_case("all") => Some(None),
        Some(t) => {
            let to = Grain::parse(&t, None)?;
            if !nests(&grain, &to) {
                return Err(format!(
                    "time_rollup {} does not hold whole {} periods: pick a coarser period type that the dataset's periods nest in",
                    to.spec(),
                    grain.spec()
                )
                .into());
            }
            Some(Some(to))
        }
    };
    let rule = p.text("time_rule");
    if rule.is_some() && rollup.is_none() {
        return Err("time_rule needs time_rollup".into());
    }
    Ok(Some(TimeQuery { grain, range, rollup, rule }))
}

/// Keep the rows of the period range: `SELECT * FROM (plan) WHERE period ...`.
pub fn filter(plan: Plan, q: &TimeQuery) -> Plan {
    let Some(r) = &q.range else { return plan };
    let cond = r.sql("period");
    let wrap = |inner: &str| format!("SELECT * FROM ({inner}) WHERE {cond}");
    match plan {
        Plan::Query(s) => Plan::Query(wrap(&s)),
        Plan::Typed(s, probe) => Plan::Typed(wrap(&s), probe),
        other => other,
    }
}

/// `time_rollup`: one row per cell and coarser period (or per cell for 'all'). COUNT and
/// TOTAL are summed; MEASUREMENT takes its declared `time_aggregation` (mean, min, max) or
/// `time_rule`; CATEGORICAL cannot be rolled up. Adds `period_count`, the number of dataset
/// periods in each row.
pub fn rollup(plan: Plan, q: &TimeQuery, manifest: Option<&Json>, present: Option<&[String]>) -> Result<Plan, E> {
    let Some(target) = &q.rollup else { return Ok(plan) };
    let over = crate::metadata::PerColumn::parse(q.rule.clone(), "time_rule")?;
    let mut names = Vec::new();
    let mut aggs = Vec::new();
    for f in manifest
        .and_then(|m| m.get("value_fields"))
        .and_then(Json::as_array)
        .into_iter()
        .flatten()
    {
        let name = f.get("name").and_then(Json::as_str).unwrap_or("");
        if name.is_empty() || present.is_some_and(|p| !p.iter().any(|c| c == name)) {
            continue;
        }
        let semantics = f.get("semantics").and_then(Json::as_str).unwrap_or("");
        let rule = match over.get(name) {
            Some(r) => match r {
                "sum" | "mean" | "min" | "max" => r.to_string(),
                other => return Err(format!("time_rule of {name} must be sum, mean, min or max; received {other:?}").into()),
            },
            None => time_rule(semantics, f.get("time_aggregation").and_then(Json::as_str))
                .map_err(|e| format!("column {name}: {e}; pass time_rule := '{{\"{name}\": \"mean\"}}'"))?
                .to_string(),
        };
        let c = format!("r.{}", sql::ident(name));
        let expr = match rule.as_str() {
            "sum" => format!("sum({c})"),
            "mean" => format!("CAST(avg({c}) AS DOUBLE)"),
            "min" => format!("min({c})"),
            _ => format!("max({c})"),
        };
        names.push(sql::ident(name));
        aggs.push(format!("{expr} AS {}", sql::ident(name)));
    }
    let period = match target {
        Some(to) => format!("{} AS period, ", rollup_sql("r.period", &q.grain, to)?),
        None => String::new(),
    };
    let order = if target.is_some() { "gid, period" } else { "gid" };
    let exclude = std::iter::once("period".to_string()).chain(names).collect::<Vec<_>>().join(", ");
    let aggs = if aggs.is_empty() { String::new() } else { format!("{}, ", aggs.join(", ")) };
    let wrap = |inner: &str| {
        format!(
            "SELECT r.* EXCLUDE ({exclude}), {period}{aggs}count(*) AS period_count \
             FROM ({inner}) AS r GROUP BY ALL ORDER BY {order}"
        )
    };
    Ok(match plan {
        Plan::Query(s) => Plan::Query(wrap(&s)),
        Plan::Typed(s, probe) => Plan::Typed(wrap(&s), wrap(&probe)),
        other => other,
    })
}

/// time_values(query, period_type := 'day' | 'hour:N' | ..., value_semantics := ...,
///   time_aggregation := ..., period / period_from / period_to, time_rollup, time_rule)
///
/// The time cube reads of read_cell_dataset on any query with a cell (`gid` or `packed_id`),
/// a `period` column and value columns: CSV, Parquet without a manifest, Delta, a database,
/// read_raster(..., period := ...). Every label must be of the period type.
pub fn plan_time_values(p: &Params) -> Result<Plan, E> {
    let query = p.pos_text(0, "query")?;
    let spec = p.text("period_type").ok_or("time_values needs period_type := 'day' (or year, month, hour:N)")?;
    let grain = Grain::parse(&spec, p.int("bucket_hours"))?;
    let lease = sql::lease()?;
    let cols: Vec<String> = sql::query_text_on(&lease, &format!("SELECT column_name FROM (DESCRIBE {query})"))?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .collect();
    if !cols.iter().any(|c| c == "period") {
        return Err("time_values needs a period column".into());
    }
    let has_gid = cols.iter().any(|c| c == "gid");
    if !has_gid && !cols.iter().any(|c| c == "packed_id") {
        return Err("time_values needs a gid or packed_id column".into());
    }
    let semantics = crate::metadata::PerColumn::parse(p.text("value_semantics"), "value_semantics")?;
    let aggregation = crate::metadata::PerColumn::parse(p.text("time_aggregation"), "time_aggregation")?;
    const KEYS: [&str; 6] = ["gid", "packed_id", "x_idx", "y_idx", "period", "uri"];
    let fields: Vec<Json> = cols
        .iter()
        .filter(|c| !KEYS.contains(&c.as_str()))
        .map(|c| {
            serde_json::json!({
                "name": c,
                "semantics": semantics.get(c).unwrap_or("MEASUREMENT").to_ascii_uppercase(),
                "time_aggregation": aggregation.get(c),
            })
        })
        .collect();
    let manifest = serde_json::json!({
        "dataset_type": "cell_time_values",
        "period_type": grain.name(),
        "bucket_hours": grain.bucket,
        "value_fields": fields,
    });
    let q = parse(p, Some(&manifest), false)?.ok_or("not a time cube")?;
    let check = format!(
        "CASE WHEN period IS NULL OR NOT period_valid(CAST(period AS VARCHAR), {}) \
         THEN error('time_values: ' || coalesce(CAST(period AS VARCHAR), 'NULL') || ' is not a {} label') END",
        lit(&grain.spec()),
        grain.spec()
    );
    let gid = if has_gid { String::new() } else { "packed_to_gid(packed_id) AS gid, ".into() };
    let base = format!(
        "SELECT {gid}* REPLACE (CAST(period AS VARCHAR) AS period) FROM ({query}) WHERE {check} IS NULL"
    );
    rollup(filter(Plan::Query(base), &q), &q, Some(&manifest), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let r = PeriodRange { from: Some("2026-02".into()), to: Some("2026-03".into()) };
        assert!(r.overlaps("2026-01-30", "2026-02-01"));
        assert!(!r.overlaps("2026-03-01", "2026-03-05"));
        assert!(!r.overlaps("2026-01-01", "2026-01-31"));
        assert!(r.overlaps_chunk("2026-02"));
        assert!(!r.overlaps_chunk("2026-03"));
        assert!(!r.overlaps_chunk("2026-01"));
        assert!(r.overlaps_chunk("2026"));
        let d = PeriodRange { from: Some("2026-02-15".into()), to: None };
        assert!(d.overlaps_chunk("2026-02") && !d.overlaps_chunk("2026-01") && d.overlaps_chunk("2027"));
    }
}
