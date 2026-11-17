//! Column detection for spatial keys (spec 2.0, section 14).
//!
//! The detector classifies values. It never converts, trims, lowercases, or repairs them.
//! A column is accepted when at least 99% of its non-null values pass the check.

use std::collections::BTreeSet;

use crate::codec::{self, side_cell_count, Cell, MAX_LEVEL};
use crate::packing;

/// Shortest GID accepted for a bare GID column.
pub const MIN_GID_LENGTH: usize = 5;
/// Fraction of values that must pass.
pub const MIN_VALID_RATIO: f64 = 0.99;
/// How many failing values are kept as examples.
pub const EXAMPLE_LIMIT: usize = 3;

const URI_PREFIX: &str = "geosquare:v2:";

/// Column type as read by the importer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    Str,
    Int64,
    Other,
}

/// One cell value. `Null` is not counted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Str(String),
    Int(i64),
}

/// The kind of spatial key a column was tested as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Uri,
    Gid,
    PackedId,
    XyIndices,
    NotSpatial,
}

/// Outcome of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Certain,
    NeedsDomain,
    NeedsLevel,
    NeedsConfirmation,
    Rejected,
}

/// Result for one column, or one `x_idx`/`y_idx` pair.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnDetection {
    pub column: String,
    pub kind: Kind,
    pub status: Status,
    pub reason: String,
    pub valid_ratio: f64,
    pub domain_codes: Vec<String>,
    pub domain_ids: Vec<u16>,
    pub levels: Vec<u8>,
    pub invalid_examples: Vec<String>,
}

/// A named column with its type and values, for [`detect_table`].
#[derive(Debug, Clone, PartialEq)]
pub struct Column {
    pub name: String,
    pub dtype: DType,
    pub values: Vec<Value>,
}

fn outcome(
    column: &str,
    kind: Kind,
    status: Status,
    reason: &str,
    valid_ratio: f64,
) -> ColumnDetection {
    ColumnDetection {
        column: column.to_string(),
        kind,
        status,
        reason: reason.to_string(),
        valid_ratio,
        domain_codes: Vec::new(),
        domain_ids: Vec::new(),
        levels: Vec::new(),
        invalid_examples: Vec::new(),
    }
}

fn ratio(valid: usize, total: usize) -> f64 {
    valid as f64 / total as f64
}

fn examples(invalid: &[String]) -> Vec<String> {
    invalid.iter().take(EXAMPLE_LIMIT).cloned().collect()
}

/// Level of a strict V2 GID, or `None`. Length must be 5 to 14.
fn gid_level(value: &str) -> Option<u8> {
    if value.len() < MIN_GID_LENGTH || value.len() > MAX_LEVEL as usize {
        return None;
    }
    codec::decode(value).ok().map(|cell| cell.level)
}

fn valid_domain_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    (1..=16).contains(&bytes.len())
        && bytes[0].is_ascii_uppercase()
        && bytes
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Split `geosquare:v2:<DOMAIN>:<GID>` into `(domain, gid)`.
fn parse_uri(value: &str) -> Option<(&str, &str)> {
    let rest = value.strip_prefix(URI_PREFIX)?;
    let (domain, gid) = rest.split_once(':')?;
    if gid.is_empty() || gid.contains(':') || !valid_domain_code(domain) {
        return None;
    }
    Some((domain, gid))
}

fn detect_uri(column: &str, present: &[&Value], domain: Option<&str>) -> ColumnDetection {
    let mut valid = 0usize;
    let mut domains = BTreeSet::new();
    let mut levels = BTreeSet::new();
    let mut invalid = Vec::new();
    for value in present {
        let text = match value {
            Value::Str(text) => text.as_str(),
            other => {
                invalid.push(format!("{other:?}"));
                continue;
            }
        };
        match parse_uri(text) {
            Some((found, gid))
                if domain.is_none_or(|wanted| wanted == found) && gid_level(gid).is_some() =>
            {
                valid += 1;
                domains.insert(found.to_string());
                levels.insert(gid_level(gid).unwrap_or_default());
            }
            _ => invalid.push(text.to_string()),
        }
    }
    let valid_ratio = ratio(valid, present.len());
    if valid_ratio < MIN_VALID_RATIO {
        let mut out = outcome(
            column,
            Kind::Uri,
            Status::Rejected,
            &format!(
                "{:.2}% of values are valid URIs, below 99%",
                valid_ratio * 100.0
            ),
            valid_ratio,
        );
        out.invalid_examples = examples(&invalid);
        return out;
    }
    let mut out = outcome(
        column,
        Kind::Uri,
        Status::Certain,
        "valid geosquare:v2 URIs",
        valid_ratio,
    );
    out.domain_codes = domains.into_iter().collect();
    out.levels = levels.into_iter().collect();
    out.invalid_examples = examples(&invalid);
    out
}

fn detect_gid(column: &str, present: &[&Value], domain: Option<&str>) -> ColumnDetection {
    let mut levels = BTreeSet::new();
    let mut accepted = Vec::new();
    let mut invalid = Vec::new();
    for value in present {
        let text = match value {
            Value::Str(text) => text.as_str(),
            other => {
                invalid.push(format!("{other:?}"));
                continue;
            }
        };
        match gid_level(text) {
            Some(level) => {
                levels.insert(level);
                accepted.push(text);
            }
            None => invalid.push(text.to_string()),
        }
    }
    let valid_ratio = ratio(accepted.len(), present.len());
    let levels: Vec<u8> = levels.into_iter().collect();
    if valid_ratio < MIN_VALID_RATIO {
        let mut out = outcome(
            column,
            Kind::Gid,
            Status::Rejected,
            &format!(
                "{:.2}% of values are valid GIDs, below 99%",
                valid_ratio * 100.0
            ),
            valid_ratio,
        );
        out.invalid_examples = examples(&invalid);
        return out;
    }
    let (status, reason) = match domain {
        None => (
            Status::NeedsDomain,
            "valid GIDs, but a bare GID needs a domain from the file, the user, or a URI",
        ),
        Some(_) if accepted.iter().all(|value| value.bytes().all(|b| b.is_ascii_digit())) => (
            Status::NeedsConfirmation,
            "all values are digits, so they could also be ordinary codes; confirm with the user",
        ),
        Some(_) => (Status::Certain, "valid bare GIDs in the given domain"),
    };
    let mut out = outcome(column, Kind::Gid, status, reason, valid_ratio);
    if let Some(code) = domain {
        out.domain_codes = vec![code.to_string()];
    }
    out.levels = levels;
    out.invalid_examples = examples(&invalid);
    out
}

fn detect_packed(column: &str, present: &[&Value]) -> ColumnDetection {
    let mut domain_ids = BTreeSet::new();
    let mut levels = BTreeSet::new();
    let mut valid = 0usize;
    let mut invalid = Vec::new();
    for value in present {
        match value {
            Value::Int(packed) => match packing::unpack(*packed) {
                Ok((domain_id, cell)) => {
                    valid += 1;
                    domain_ids.insert(domain_id);
                    levels.insert(cell.level);
                }
                Err(_) => invalid.push(packed.to_string()),
            },
            other => invalid.push(format!("{other:?}")),
        }
    }
    let valid_ratio = ratio(valid, present.len());
    if valid_ratio < MIN_VALID_RATIO {
        let mut out = outcome(
            column,
            Kind::PackedId,
            Status::Rejected,
            &format!(
                "{:.2}% of values are valid packed IDs, below 99%",
                valid_ratio * 100.0
            ),
            valid_ratio,
        );
        out.invalid_examples = examples(&invalid);
        return out;
    }
    let mut out = outcome(
        column,
        Kind::PackedId,
        Status::Certain,
        "exact packed V2 identities; domain is in the value",
        valid_ratio,
    );
    out.domain_ids = domain_ids.into_iter().collect();
    out.levels = levels.into_iter().collect();
    out.invalid_examples = examples(&invalid);
    out
}

/// True when every int value would be a valid GID if it were read as text.
fn digit_gid_in_int_column(present: &[&Value]) -> bool {
    present.iter().all(|value| match value {
        Value::Int(number) => gid_level(&number.to_string()).is_some(),
        _ => false,
    })
}

/// Classify one column. `domain` is the user's or file's domain, if known.
pub fn detect_column(
    column: &str,
    dtype: DType,
    values: &[Value],
    domain: Option<&str>,
) -> ColumnDetection {
    let present: Vec<&Value> = values
        .iter()
        .filter(|value| **value != Value::Null)
        .collect();
    if present.is_empty() {
        return outcome(
            column,
            Kind::NotSpatial,
            Status::Rejected,
            "no non-null values",
            0.0,
        );
    }
    match dtype {
        DType::Int64 => {
            if digit_gid_in_int_column(&present) {
                return outcome(
                    column,
                    Kind::Gid,
                    Status::Rejected,
                    "values look like GIDs but the column is int64; read the column as text first",
                    0.0,
                );
            }
            detect_packed(column, &present)
        }
        DType::Str => {
            let is_uri = present
                .iter()
                .any(|value| matches!(value, Value::Str(text) if text.starts_with(URI_PREFIX)));
            if is_uri {
                detect_uri(column, &present, domain)
            } else {
                detect_gid(column, &present, domain)
            }
        }
        DType::Other => outcome(
            column,
            Kind::NotSpatial,
            Status::Rejected,
            "dtype cannot hold a spatial key",
            0.0,
        ),
    }
}

fn detect_xy(
    label: &str,
    x_values: &[Value],
    y_values: &[Value],
    domain: Option<&str>,
    level: Option<u8>,
) -> ColumnDetection {
    let Some(level) = level else {
        return outcome(
            label,
            Kind::XyIndices,
            Status::NeedsLevel,
            "x/y indices need a level",
            0.0,
        );
    };
    if x_values.len() != y_values.len() {
        return outcome(
            label,
            Kind::XyIndices,
            Status::Rejected,
            "x and y columns differ in length",
            0.0,
        );
    }
    let size = match side_cell_count(level) {
        Ok(size) => size,
        Err(_) => {
            return outcome(
                label,
                Kind::XyIndices,
                Status::Rejected,
                "level is outside 0..=14",
                0.0,
            );
        }
    };
    let mut valid = 0usize;
    let mut invalid = Vec::new();
    for (x_value, y_value) in x_values.iter().zip(y_values.iter()) {
        let pair = match (x_value, y_value) {
            (Value::Int(x), Value::Int(y))
                if *x >= 0 && *y >= 0 && (*x as u64) < size && (*y as u64) < size =>
            {
                Some(())
            }
            _ => None,
        };
        match pair {
            Some(()) => valid += 1,
            None => invalid.push(format!("({x_value:?}, {y_value:?})")),
        }
    }
    let valid_ratio = ratio(valid, x_values.len());
    if valid_ratio < MIN_VALID_RATIO {
        let mut out = outcome(
            label,
            Kind::XyIndices,
            Status::Rejected,
            &format!(
                "{:.2}% of pairs are inside the level {level} grid, below 99%",
                valid_ratio * 100.0
            ),
            valid_ratio,
        );
        out.levels = vec![level];
        out.invalid_examples = examples(&invalid);
        return out;
    }
    let (status, reason) = match domain {
        None => (
            Status::NeedsDomain,
            "indices need a domain from the file or the user",
        ),
        Some(_) => (
            Status::Certain,
            "integer indices inside the grid at the given level",
        ),
    };
    let mut out = outcome(label, Kind::XyIndices, status, reason, valid_ratio);
    if let Some(code) = domain {
        out.domain_codes = vec![code.to_string()];
    }
    out.levels = vec![level];
    out.invalid_examples = examples(&invalid);
    out
}

/// Classify every column in a table, and check `x_idx`/`y_idx` as one pair.
///
/// When both names exist as int64 columns, they become one `x_idx+y_idx` result and are
/// not also tested as packed IDs. `level` is required to judge indices.
pub fn detect_table(
    columns: &[Column],
    domain: Option<&str>,
    level: Option<u8>,
) -> Vec<ColumnDetection> {
    let find = |wanted: &str| {
        columns
            .iter()
            .find(|column| column.name.to_lowercase() == wanted && column.dtype == DType::Int64)
    };
    let pair = match (find("x_idx"), find("y_idx")) {
        (Some(x), Some(y)) => Some((x, y)),
        _ => None,
    };

    let mut results = Vec::new();
    for column in columns {
        if let Some((x, y)) = pair {
            if column.name == x.name || column.name == y.name {
                continue;
            }
        }
        results.push(detect_column(
            &column.name,
            column.dtype,
            &column.values,
            domain,
        ));
    }
    if let Some((x, y)) = pair {
        let label = format!("{}+{}", x.name, y.name);
        results.push(detect_xy(&label, &x.values, &y.values, domain, level));
    }
    results
}

/// The cell a packed value names, when it passes the strict check.
pub fn cell_from_packed(value: i64) -> Option<(u16, Cell)> {
    packing::unpack(value).ok()
}
