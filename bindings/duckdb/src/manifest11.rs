//! Manifest 1.1 (docs/dataset-metadata.md, sections 3 to 5), as written by the engine.
//!
//! Opt-in: `write_cell_dataset(..., manifest_version := '1.1')`. The writer computes what 1.1
//! records from the staged rows before it writes any file, because each Parquet file embeds
//! the manifest:
//!
//! * `value_fields[]`: `null_share`, `min_value`, `max_value` over all rows, and the parent rule
//!   and weight (`space_aggregation`, `space_weight`, section 4);
//! * `extent.bbox_wgs84`: the bounding box of the cell outlines in lon/lat;
//! * `storage.data_files[]`: each file's path, content hash (section 3.1) and row count.
//!
//! Governance and lineage come from the write's parameters, else from the dataset being
//! overwritten or appended to, else from the defaults. See docs/manifest-1.1.md.

use crate::content_hash::{self, Value, ValueField};
use crate::dataset;
use crate::grid;
use crate::sql::{self, Lease, E};
use geosquare_core::{domain::Domain, packing};
use serde_json::{json, Value as Json};
use std::collections::BTreeMap;

pub const VERSION: &str = "1.1";
const SPEC_VERSION: &str = "2.0";
const SHARE_POLICIES: [&str; 3] = ["internal", "k_anon_only", "public"];
const REFRESH: [&str; 6] = ["once", "daily", "weekly", "monthly", "yearly", "irregular"];

/// Governance and lineage parameters of a write. Only manifest 1.1 records them.
#[derive(Debug, Clone, Default)]
pub struct Params {
    pub owner: Option<String>,
    pub source_name: Option<String>,
    pub license: Option<String>,
    pub share_policy: Option<String>,
    pub refresh: Option<String>,
    /// A JSON list of strings, or comma-separated text.
    pub derived_from: Option<String>,
}

/// One value field as the writer knows it: its declared type, its semantics, and the rule and
/// weight given for it (a parameter, or the value in the manifest it came from).
pub struct FieldIn<'a> {
    pub name: &'a str,
    pub parquet_type: &'a str,
    pub numeric: bool,
    pub semantics: String,
    pub rule: Option<String>,
    pub weight: Option<String>,
}

/// A data file the write produces: its path below the dataset root, and the rows it holds as a
/// SQL predicate over the staged table.
pub struct FileSpec {
    pub path: String,
    pub predicate: String,
}

/// What manifest 1.1 records about the rows of one write.
#[derive(Debug, Default)]
pub struct Facts {
    /// Per value field name: the keys `space_aggregation`, `space_weight`, `null_share`,
    /// `min_value` and `max_value`, merged into that value field.
    pub fields: BTreeMap<String, Json>,
    /// Bounding box in lon/lat: min lon, min lat, max lon, max lat.
    pub bbox: Option<[f64; 4]>,
    /// `storage.data_files` entries, in the order the files are listed.
    pub data_files: Vec<Json>,
}

/// Check the parameter values the 1.1 schema allows. Runs before the target is cleared.
pub fn check_params(p: &Params) -> Result<(), E> {
    if let Some(s) = &p.share_policy {
        if !SHARE_POLICIES.contains(&s.as_str()) {
            return Err(
                format!("share_policy must be one of {SHARE_POLICIES:?}; received {s:?}").into(),
            );
        }
    }
    if let Some(r) = &p.refresh {
        if !REFRESH.contains(&r.as_str()) {
            return Err(format!("refresh must be one of {REFRESH:?}; received {r:?}").into());
        }
    }
    derived_from(p, None).map(|_| ())
}

/// The parent rule and weight of each value field, checked against spec section 4.
///
/// Without a rule: COUNT and TOTAL sum, DENSITY is a mean weighted by area, and every other
/// semantics is `none` (not rolled up). `none` is the spec's safe default. A weight must be
/// `area` or the name of a COUNT or TOTAL value field.
pub fn resolve_fields(fields: &[FieldIn]) -> Result<Vec<Rule>, E> {
    let semantics: BTreeMap<&str, &str> = fields
        .iter()
        .map(|f| (f.name, f.semantics.as_str()))
        .collect();
    fields
        .iter()
        .map(|f| {
            content_hash::check_parquet_type(f.parquet_type).map_err(|_| {
                format!(
                    "column {}: type {} cannot be a manifest 1.1 value; use an integer, FLOAT, \
                     DOUBLE or VARCHAR column (cast it in the query)",
                    f.name, f.parquet_type
                )
            })?;
            let default = match f.semantics.to_ascii_uppercase().as_str() {
                "COUNT" | "TOTAL" => "sum",
                "DENSITY" => "weighted_mean",
                _ => "none",
            };
            let rule = f.rule.clone().unwrap_or_else(|| default.to_string());
            if rule == "none" && f.weight.is_some() {
                return Err(format!(
                    "column {}: space_weight needs a space_aggregation other than none",
                    f.name
                )
                .into());
            }
            let (aggregation, weight) =
                match dataset::space_rule(f.name, &f.semantics, Some(rule), f.weight.clone())? {
                    None => ("none".to_string(), None),
                    Some((r, w)) => (r, w),
                };
            if let Some(w) = &weight {
                let counted = semantics
                    .get(w.as_str())
                    .is_some_and(|s| matches!(s.to_ascii_uppercase().as_str(), "COUNT" | "TOTAL"));
                if w != "area" && !counted {
                    return Err(format!(
                        "column {}: space_weight {w:?} must name a COUNT or TOTAL value field of \
                         this dataset, or be area",
                        f.name
                    )
                    .into());
                }
            }
            Ok(Rule {
                aggregation,
                weight,
            })
        })
        .collect()
}

/// The resolved parent rule and weight of one value field.
pub struct Rule {
    pub aggregation: String,
    pub weight: Option<String>,
}

/// Row count and statistics of one column over the staged table.
struct Stats {
    nulls: i64,
    min: Option<Json>,
    max: Option<Json>,
}

/// Row count, and per value field the null count and (numeric fields) the min and max.
fn column_stats(lease: &Lease, stage: &str, fields: &[FieldIn]) -> Result<(i64, Vec<Stats>), E> {
    let mut select = vec!["count(*)".to_string()];
    let mut types: Vec<&str> = vec!["int64"];
    let mut names: Vec<String> = vec!["rows".into()];
    for f in fields {
        let c = sql::ident(f.name);
        select.push(format!("count(*) FILTER (WHERE {c} IS NULL)"));
        types.push("int64");
        names.push(format!("{}.nulls", f.name));
        if f.numeric {
            for agg in ["min", "max"] {
                select.push(format!("{agg}({c})"));
                types.push(f.parquet_type);
                names.push(format!("{}.{agg}", f.name));
            }
        }
    }
    let names: Vec<&str> = names.iter().map(String::as_str).collect();
    let query = format!("SELECT {} FROM {stage}", select.join(", "));
    let mut row = Vec::new();
    content_hash::for_each_row(lease, &query, &types, &names, |r| {
        row = r.to_vec();
        Ok(())
    })?;
    let mut values = row.into_iter();
    let rows = match values.next() {
        Some(Value::Int(n)) => i64::try_from(n).map_err(|_| "row count out of range")?,
        other => return Err(format!("no row count: {other:?}").into()),
    };
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        let nulls = match values.next() {
            Some(Value::Int(n)) => i64::try_from(n).map_err(|_| "null count out of range")?,
            other => return Err(format!("column {}: no null count: {other:?}", f.name).into()),
        };
        let (min, max) = if f.numeric {
            (
                json_value(values.next(), f.name)?,
                json_value(values.next(), f.name)?,
            )
        } else {
            (None, None)
        };
        out.push(Stats { nulls, min, max });
    }
    Ok((rows, out))
}

fn json_value(v: Option<Value>, column: &str) -> Result<Option<Json>, E> {
    Ok(match v {
        None | Some(Value::Null) => None,
        Some(Value::Int(i)) => Some(json!(i64::try_from(i)
            .map_err(|_| format!("column {column}: value out of the 64-bit range"))?)),
        Some(Value::Float(x)) if x.is_finite() => Some(json!(x)),
        Some(Value::Float(_)) => {
            return Err(format!("column {column}: NaN and infinity cannot be recorded").into())
        }
        Some(Value::Text(_)) => {
            return Err(format!("column {column}: text has no min or max").into())
        }
    })
}

/// The smaller (or larger) of two JSON numbers, ignoring nulls. Integers compare as integers.
fn pick(a: Option<&Json>, b: Option<&Json>, smaller: bool) -> Option<Json> {
    let a = a.filter(|v| !v.is_null());
    let b = b.filter(|v| !v.is_null());
    match (a, b) {
        (Some(x), Some(y)) => {
            let x_less = match (x.as_i64(), y.as_i64()) {
                (Some(p), Some(q)) => p < q,
                _ => x.as_f64()? < y.as_f64()?,
            };
            let chosen = if x_less == smaller { x } else { y };
            Some(chosen.clone())
        }
        (x, y) => x.or(y).cloned(),
    }
}

/// Statistics of a column over the old rows (from the manifest being appended to) and the new ones.
fn merge_stats(base: &Json, name: &str, base_rows: i64, new: Stats) -> Result<Stats, E> {
    let old = base
        .get("value_fields")
        .and_then(Json::as_array)
        .and_then(|fs| {
            fs.iter()
                .find(|f| f.get("name").and_then(Json::as_str) == Some(name))
        })
        .ok_or_else(|| format!("the table's manifest has no value field {name}"))?;
    let share = old
        .get("null_share")
        .and_then(Json::as_f64)
        .ok_or_else(|| format!("the table's manifest records no null_share for {name}"))?;
    Ok(Stats {
        nulls: (share * base_rows as f64).round() as i64 + new.nulls,
        min: pick(old.get("min_value"), new.min.as_ref(), true),
        max: pick(old.get("max_value"), new.max.as_ref(), false),
    })
}

fn base_rows(base: &Json) -> Result<i64, E> {
    base.pointer("/storage/row_count")
        .and_then(Json::as_i64)
        .ok_or_else(|| "the table's manifest records no row_count".into())
}

fn base_bbox(base: &Json) -> Option<[f64; 4]> {
    let v = base.pointer("/extent/bbox_wgs84")?.as_array()?;
    let n: Vec<f64> = v.iter().map(Json::as_f64).collect::<Option<_>>()?;
    <[f64; 4]>::try_from(n).ok()
}

/// Bounding box of the outlines of the staged cells, in lon/lat (the released grid CRS).
fn cells_bbox(lease: &Lease, stage: &str, domain: &'static Domain) -> Result<Option<[f64; 4]>, E> {
    let mut ids = Vec::new();
    content_hash::for_each_row(
        lease,
        &format!("SELECT DISTINCT packed_id FROM {stage}"),
        &["int64"],
        &["packed_id"],
        |r| {
            if let Some(Value::Int(i)) = r.first() {
                ids.push(*i as i64);
            }
            Ok(())
        },
    )?;
    let mut out: Option<[f64; 4]> = None;
    for id in ids {
        let (_, cell) = packing::unpack(id)?;
        for (lon, lat) in grid::cell_ring(domain, cell, "EPSG:4326", false)? {
            out = Some(match out {
                None => [lon, lat, lon, lat],
                Some([min_lon, min_lat, max_lon, max_lat]) => [
                    min_lon.min(lon),
                    min_lat.min(lat),
                    max_lon.max(lon),
                    max_lat.max(lat),
                ],
            });
        }
    }
    Ok(out)
}

fn union(a: Option<[f64; 4]>, b: Option<[f64; 4]>) -> Option<[f64; 4]> {
    match (a, b) {
        (Some([a0, a1, a2, a3]), Some([b0, b1, b2, b3])) => {
            Some([a0.min(b0), a1.min(b1), a2.max(b2), a3.max(b3)])
        }
        (x, None) | (None, x) => x,
    }
}

/// Everything manifest 1.1 records about the staged rows.
///
/// `base` is the manifest of the dataset being appended to (append only): its statistics,
/// extent and data files are kept and combined with the new rows. `files` are the data files
/// the write creates, each with the predicate that selects its rows from `stage`. Their hashes
/// are computed here, before any file is written.
pub fn facts(
    lease: &Lease,
    stage: &str,
    domain: &'static Domain,
    fields: &[FieldIn],
    files: Vec<FileSpec>,
    base: Option<&Json>,
    threads: usize,
) -> Result<Facts, E> {
    let rules = resolve_fields(fields)?;
    let (rows, stats) = column_stats(lease, stage, fields)?;
    let (old_rows, total_rows) = match base {
        Some(b) => {
            let n = base_rows(b)?;
            (n, n + rows)
        }
        None => (0, rows),
    };
    let mut out = Facts::default();
    for ((f, rule), stat) in fields.iter().zip(&rules).zip(stats) {
        let stat = match base {
            Some(b) => merge_stats(b, f.name, old_rows, stat)?,
            None => stat,
        };
        out.fields.insert(
            f.name.to_string(),
            json!({
                "space_aggregation": rule.aggregation,
                "space_weight": rule.weight,
                "null_share": stat.nulls as f64 / total_rows as f64,
                "min_value": stat.min,
                "max_value": stat.max,
            }),
        );
    }
    out.bbox = union(cells_bbox(lease, stage, domain)?, base.and_then(base_bbox));

    let value_fields: Vec<ValueField> = fields
        .iter()
        .map(|f| ValueField {
            name: f.name.to_string(),
            parquet_type: f.parquet_type.to_string(),
        })
        .collect();
    let columns = std::iter::once("gid".to_string())
        .chain(fields.iter().map(|f| sql::ident(f.name)))
        .collect::<Vec<_>>()
        .join(", ");
    let hashed = sql::par_map(files, threads, |l, spec| {
        let query = format!("SELECT {columns} FROM {stage} WHERE {}", spec.predicate);
        let hash = content_hash::query_content_hash(l, &query, &value_fields)?;
        Ok((spec.path, hash))
    })?;
    let new_rows: i64 = hashed.iter().map(|(_, h)| h.rows).sum();
    if new_rows != rows {
        return Err(format!(
            "the data files hold {new_rows} rows but the staged table has {rows}: a file predicate \
             is wrong"
        )
        .into());
    }
    out.data_files = match base {
        Some(b) => b
            .pointer("/storage/data_files")
            .and_then(Json::as_array)
            .cloned()
            .ok_or("the table's manifest lists no data_files: it is not manifest 1.1")?,
        None => Vec::new(),
    };
    out.data_files.extend(
        hashed.into_iter().map(
            |(path, h)| json!({"path": path, "content_sha256": h.sha256, "row_count": h.rows}),
        ),
    );
    Ok(out)
}

/// Set the manifest 1.1 blocks of a cell manifest that the 1.0 writer built.
pub fn apply(manifest: &mut Json, facts: &Facts, governance: Json, derived: Json) {
    manifest["manifest_version"] = json!(VERSION);
    manifest["extent"] = json!({
        "bbox_wgs84": facts.bbox.map(|b| b.to_vec()),
        "compact_set_path": null,
    });
    manifest["governance"] = governance;
    manifest["storage"]["data_files"] = json!(facts.data_files);
    manifest["provenance"]["spec_version"] = json!(SPEC_VERSION);
    manifest["provenance"]["derived_from"] = derived;
    if let Some(fields) = manifest
        .get_mut("value_fields")
        .and_then(Json::as_array_mut)
    {
        for field in fields {
            let name = field
                .get("name")
                .and_then(Json::as_str)
                .unwrap_or("")
                .to_string();
            if let Some(Json::Object(keys)) = facts.fields.get(&name) {
                for (k, v) in keys {
                    field[k.as_str()] = v.clone();
                }
            }
        }
    }
}

/// The governance block: the parameters, else the dataset being overwritten or appended to,
/// else the defaults (`share_policy` internal, `refresh` once; the rest null).
pub fn governance(p: &Params, title: Option<&str>, prev: Option<&Json>) -> Json {
    let kept = |key: &str| {
        prev.and_then(|m| m.pointer(&format!("/governance/{key}")))
            .filter(|v| !v.is_null())
            .cloned()
    };
    let text = |given: &Option<String>, key: &str| {
        given
            .clone()
            .map(Json::from)
            .or_else(|| kept(key))
            .unwrap_or(Json::Null)
    };
    json!({
        "title": title.map(Json::from).or_else(|| kept("title")).unwrap_or(Json::Null),
        "owner": text(&p.owner, "owner"),
        "source_name": text(&p.source_name, "source_name"),
        "license": text(&p.license, "license"),
        "share_policy": p.share_policy.clone().map(Json::from)
            .or_else(|| kept("share_policy")).unwrap_or(json!("internal")),
        "refresh": p.refresh.clone().map(Json::from)
            .or_else(|| kept("refresh")).unwrap_or(json!("once")),
    })
}

/// `provenance.derived_from`: the parameter, else the dataset being overwritten or appended to.
pub fn derived_from(p: &Params, prev: Option<&Json>) -> Result<Json, E> {
    let Some(text) = &p.derived_from else {
        return Ok(prev
            .and_then(|m| m.pointer("/provenance/derived_from"))
            .cloned()
            .unwrap_or_else(|| json!([])));
    };
    let t = text.trim();
    let items: Vec<String> = if t.starts_with('[') {
        serde_json::from_str(t)
            .map_err(|_| "derived_from must be a JSON list of strings, or comma-separated text")?
    } else {
        t.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect()
    };
    Ok(json!(items))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field<'a>(
        name: &'a str,
        ty: &'a str,
        sem: &'a str,
        rule: Option<&str>,
        weight: Option<&str>,
    ) -> FieldIn<'a> {
        FieldIn {
            name,
            parquet_type: ty,
            numeric: ty != "string",
            semantics: sem.to_string(),
            rule: rule.map(String::from),
            weight: weight.map(String::from),
        }
    }

    #[test]
    fn defaults_follow_semantics() {
        let fields = [
            field("population", "int64", "COUNT", None, None),
            field("density", "double", "DENSITY", None, None),
            field("depth", "double", "MEASUREMENT", None, None),
            field("zone", "string", "CATEGORICAL", None, None),
        ];
        let r = resolve_fields(&fields).unwrap();
        assert_eq!(
            (r[0].aggregation.as_str(), r[0].weight.as_deref()),
            ("sum", None)
        );
        assert_eq!(
            (r[1].aggregation.as_str(), r[1].weight.as_deref()),
            ("weighted_mean", Some("area"))
        );
        assert_eq!(
            (r[2].aggregation.as_str(), r[2].weight.as_deref()),
            ("none", None)
        );
        assert_eq!(
            (r[3].aggregation.as_str(), r[3].weight.as_deref()),
            ("none", None)
        );
    }

    #[test]
    fn weighted_mean_needs_a_counted_weight() {
        let no_weight = [field(
            "price",
            "double",
            "MEASUREMENT",
            Some("weighted_mean"),
            None,
        )];
        assert!(resolve_fields(&no_weight).is_err());
        let not_counted = [
            field(
                "rate",
                "double",
                "RATE",
                Some("weighted_mean"),
                Some("depth"),
            ),
            field("depth", "double", "MEASUREMENT", Some("max"), None),
        ];
        assert!(resolve_fields(&not_counted).is_err());
        let counted = [
            field(
                "rate",
                "double",
                "RATE",
                Some("weighted_mean"),
                Some("population"),
            ),
            field("population", "int64", "COUNT", None, None),
        ];
        let r = resolve_fields(&counted).unwrap();
        assert_eq!(r[0].weight.as_deref(), Some("population"));
    }

    #[test]
    fn refuses_rules_the_table_does_not_allow() {
        assert!(resolve_fields(&[field("n", "int64", "COUNT", Some("median"), None)]).is_err());
        assert!(
            resolve_fields(&[field("n", "int64", "COUNT", Some("sum"), Some("area"))]).is_err()
        );
        assert!(resolve_fields(&[field(
            "z",
            "string",
            "CATEGORICAL",
            Some("majority_weighted"),
            Some("w")
        )])
        .is_err());
        assert!(resolve_fields(&[field("n", "int64", "MEASUREMENT", None, Some("area"))]).is_err());
    }

    #[test]
    fn refuses_types_that_cannot_be_hashed() {
        assert!(resolve_fields(&[field("flag", "bool", "CATEGORICAL", None, None)]).is_err());
        assert!(resolve_fields(&[field("d", "date32[day]", "CATEGORICAL", None, None)]).is_err());
    }

    #[test]
    fn governance_defaults_and_inheritance() {
        let g = governance(&Params::default(), Some("Title"), None);
        assert_eq!(g["share_policy"], "internal");
        assert_eq!(g["refresh"], "once");
        assert!(g["owner"].is_null());
        let prev = json!({"governance": {"owner": "data team", "share_policy": "public", "refresh": "monthly", "title": "Old"}});
        let kept = governance(&Params::default(), None, Some(&prev));
        assert_eq!(kept["owner"], "data team");
        assert_eq!(kept["share_policy"], "public");
        assert_eq!(kept["title"], "Old");
        let given = governance(
            &Params {
                owner: Some("ops".into()),
                ..Default::default()
            },
            None,
            Some(&prev),
        );
        assert_eq!(given["owner"], "ops");
    }

    #[test]
    fn checks_governance_values() {
        assert!(check_params(&Params {
            share_policy: Some("secret".into()),
            ..Default::default()
        })
        .is_err());
        assert!(check_params(&Params {
            refresh: Some("hourly".into()),
            ..Default::default()
        })
        .is_err());
        assert!(check_params(&Params {
            share_policy: Some("k_anon_only".into()),
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn derived_from_accepts_list_or_commas() {
        let p = Params {
            derived_from: Some("a, b,,c".into()),
            ..Default::default()
        };
        assert_eq!(derived_from(&p, None).unwrap(), json!(["a", "b", "c"]));
        let p = Params {
            derived_from: Some(r#"["x"]"#.into()),
            ..Default::default()
        };
        assert_eq!(derived_from(&p, None).unwrap(), json!(["x"]));
    }

    #[test]
    fn merged_minimum_keeps_integers_and_nulls() {
        assert_eq!(pick(Some(&json!(3)), Some(&json!(1)), true), Some(json!(1)));
        assert_eq!(
            pick(Some(&json!(3)), Some(&json!(7)), false),
            Some(json!(7))
        );
        assert_eq!(
            pick(Some(&Json::Null), Some(&json!(2.5)), true),
            Some(json!(2.5))
        );
        assert_eq!(pick(None, None, true), None);
    }
}
