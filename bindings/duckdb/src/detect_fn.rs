//! detect_table(query, domain_code := NULL, level := NULL, max_rows := 100000):
//! which columns of a query are spatial keys (spec 2.0 section 14), with the engine's
//! detector, as geosquare-v2 `detect_table`. One row per column (x_idx/y_idx as one pair).

use crate::sql::{self, lit, Params, Plan, E};
use geosquare_core::detect::{self, Column, DType, Kind, Status, Value};

fn kind_name(k: Kind) -> &'static str {
    match k {
        Kind::Uri => "uri",
        Kind::Gid => "gid",
        Kind::PackedId => "packed_id",
        Kind::XyIndices => "xy_indices",
        Kind::NotSpatial => "none",
    }
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Certain => "certain",
        Status::NeedsDomain => "needs_domain",
        Status::NeedsLevel => "needs_level",
        Status::NeedsConfirmation => "needs_confirmation",
        Status::Rejected => "rejected",
    }
}

/// DuckDB column type -> detector dtype: text -> string, integers -> int64, else other.
fn dtype(duck: &str) -> DType {
    let t = duck.to_ascii_uppercase();
    match t.as_str() {
        "VARCHAR" => DType::Str,
        "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT" | "USMALLINT"
        | "UINTEGER" | "UBIGINT" | "UHUGEINT" => DType::Int64,
        _ => DType::Other,
    }
}

fn list<T: ToString>(items: &[T], quote: bool, ty: &str) -> String {
    let v: Vec<String> = items
        .iter()
        .map(|x| if quote { lit(&x.to_string()) } else { x.to_string() })
        .collect();
    format!("[{}]::{ty}[]", v.join(", "))
}

pub fn plan(p: &Params) -> Result<Plan, E> {
    let query = p.pos_text(0, "query")?;
    let domain = p.text("domain_code");
    let level = match p.int("level") {
        Some(l) if (0..=255).contains(&l) => Some(l as u8),
        Some(l) => return Err(format!("level {l} is out of range").into()),
        None => None,
    };
    let max_rows = p.int("max_rows").unwrap_or(100_000);
    let lease = sql::lease()?;
    let src = format!("(SELECT * FROM ({query}) AS gs_q{})", if max_rows > 0 { format!(" LIMIT {max_rows}") } else { String::new() });
    let described = sql::query_text_on(&lease, &format!("SELECT column_name, column_type FROM (DESCRIBE SELECT * FROM {src})"))?;
    let cols: Vec<(String, DType)> = described
        .into_iter()
        .map(|r| (r[0].clone().unwrap_or_default(), dtype(r[1].as_deref().unwrap_or(""))))
        .collect();
    let select: Vec<String> = cols
        .iter()
        .map(|(n, _)| format!("CAST({} AS VARCHAR)", sql::ident(n)))
        .collect();
    let rows = if select.is_empty() {
        Vec::new()
    } else {
        sql::query_text_on(&lease, &format!("SELECT {} FROM {src}", select.join(", ")))?
    };
    let mut table = Vec::with_capacity(cols.len());
    for (k, (name, dt)) in cols.iter().enumerate() {
        let mut values = Vec::with_capacity(rows.len());
        for r in &rows {
            values.push(match (&r[k], dt) {
                (None, _) => Value::Null,
                (Some(_), DType::Other) => Value::Str(String::new()), // only null-ness is read
                (Some(s), DType::Str) => Value::Str(s.clone()),
                (Some(s), DType::Int64) => Value::Int(s.parse::<i64>().map_err(|_| {
                    format!("column {name:?}: value {s} does not fit in int64")
                })?),
            });
        }
        table.push(Column { name: name.clone(), dtype: *dt, values });
    }
    let results = detect::detect_table(&table, domain.as_deref(), level);
    if results.is_empty() {
        return Ok(Plan::Query(format!(
            "SELECT * FROM (SELECT NULL::VARCHAR AS \"column\", NULL::VARCHAR AS kind, NULL::VARCHAR AS status, \
             NULL::VARCHAR AS reason, NULL::DOUBLE AS valid_ratio, NULL::VARCHAR[] AS domain_codes, \
             NULL::USMALLINT[] AS domain_ids, NULL::UTINYINT[] AS levels, NULL::VARCHAR[] AS invalid_examples) LIMIT 0"
        )));
    }
    let values: Vec<String> = results
        .iter()
        .map(|r| {
            format!(
                "({}, {}, {}, {}, {}::DOUBLE, {}, {}, {}, {})",
                lit(&r.column),
                lit(kind_name(r.kind)),
                lit(status_name(r.status)),
                lit(&r.reason),
                r.valid_ratio,
                list(&r.domain_codes, true, "VARCHAR"),
                list(&r.domain_ids, false, "USMALLINT"),
                list(&r.levels, false, "UTINYINT"),
                list(&r.invalid_examples, true, "VARCHAR"),
            )
        })
        .collect();
    Ok(Plan::Query(format!(
        "SELECT * FROM (VALUES {}) AS t(\"column\", kind, status, reason, valid_ratio, domain_codes, domain_ids, levels, invalid_examples)",
        values.join(", ")
    )))
}
