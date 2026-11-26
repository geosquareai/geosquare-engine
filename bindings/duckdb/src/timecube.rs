//! Rules of the time cube (`cell_time_values`, geosquare-v2 docs/time-cube.md, reference
//! `geosquare_v2.timecube`): one row per (cell, period), one period type per dataset, every
//! label a half-open UTC interval.
//!
//! Labels: `YYYY` (year), `YYYY-MM` (month), `YYYY-MM-DD` (day), `YYYY-MM-DDTHH` (hour, the
//! start of a bucket of 1, 2, 3, 4, 6, 8 or 12 hours aligned to 00 UTC). Years 0001-9999, real
//! dates only, ASCII digits, no offsets or short forms. Within one period type the labels
//! have a fixed width, so their text order is their time order.

use std::error::Error;

type E = Box<dyn Error>;

pub const BUCKET_HOURS: [u32; 7] = [1, 2, 3, 4, 6, 8, 12];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Year,
    Month,
    Day,
    Hour,
}

/// A period type with its bucket size (hours, only for `hour`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grain {
    pub kind: Kind,
    pub bucket: Option<u32>,
}

impl Grain {
    pub fn new(period_type: &str, bucket_hours: Option<i64>) -> Result<Grain, E> {
        let kind = match period_type {
            "year" => Kind::Year,
            "month" => Kind::Month,
            "day" => Kind::Day,
            "hour" => Kind::Hour,
            other => {
                return Err(
                    format!("unknown period_type {other:?} (year, month, day or hour)").into(),
                )
            }
        };
        let bucket = match (kind, bucket_hours) {
            (Kind::Hour, Some(b)) if BUCKET_HOURS.contains(&(b as u32)) && b > 0 => Some(b as u32),
            (Kind::Hour, b) => {
                return Err(
                    format!("bucket_hours must be one of {BUCKET_HOURS:?}; received {b:?}").into(),
                )
            }
            (_, Some(_)) => {
                return Err("bucket_hours is only allowed for period_type 'hour'".into())
            }
            (_, None) => None,
        };
        Ok(Grain { kind, bucket })
    }

    /// `period_type` with an optional bucket written in it: `day`, `hour:4`. A bucket given
    /// both ways must agree.
    pub fn parse(spec: &str, bucket_hours: Option<i64>) -> Result<Grain, E> {
        match spec.trim().split_once(':') {
            Some((t, b)) => {
                let b: i64 = b
                    .trim()
                    .parse()
                    .map_err(|_| format!("bad bucket in period type {spec:?} (e.g. 'hour:4')"))?;
                if bucket_hours.is_some_and(|x| x != b) {
                    return Err(format!(
                        "period type {spec:?} and bucket_hours {bucket_hours:?} disagree"
                    )
                    .into());
                }
                Grain::new(t.trim(), Some(b))
            }
            None => Grain::new(spec.trim(), bucket_hours),
        }
    }

    /// `day`, `hour:4`: the form `parse` reads.
    pub fn spec(&self) -> String {
        match self.bucket {
            Some(b) => format!("{}:{b}", self.name()),
            None => self.name().to_string(),
        }
    }

    pub fn name(&self) -> &'static str {
        match self.kind {
            Kind::Year => "year",
            Kind::Month => "month",
            Kind::Day => "day",
            Kind::Hour => "hour",
        }
    }

    /// Characters of a label of this grain.
    pub fn width(&self) -> usize {
        match self.kind {
            Kind::Year => 4,
            Kind::Month => 7,
            Kind::Day => 10,
            Kind::Hour => 13,
        }
    }

    /// Length of a period in seconds when fixed (day, hour); None for months and years.
    fn fixed_seconds(&self) -> Option<i64> {
        match self.kind {
            Kind::Day => Some(86_400),
            Kind::Hour => Some(3_600 * self.bucket.unwrap_or(1) as i64),
            _ => None,
        }
    }
}

/// Days since 1970-01-01 of a civil date (proleptic Gregorian).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Civil date of days since 1970-01-01.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

fn digits(s: &[u8]) -> Option<i64> {
    if s.is_empty() || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(s.iter().fold(0i64, |a, c| a * 10 + (c - b'0') as i64))
}

fn bad(label: &str, grain: &Grain, why: &str) -> E {
    format!("{label:?} is not a valid {} label: {why}", grain.name()).into()
}

/// UTC start of a label, in seconds since 1970-01-01.
pub fn parse_label(label: &str, grain: &Grain) -> Result<i64, E> {
    let b = label.as_bytes();
    if b.len() != grain.width() {
        return Err(bad(label, grain, "wrong format"));
    }
    let year = digits(&b[0..4]).ok_or_else(|| bad(label, grain, "wrong format"))?;
    if year < 1 {
        return Err(bad(label, grain, "year 0000 does not exist"));
    }
    let (mut month, mut day, mut hour) = (1, 1, 0);
    if grain.width() >= 7 {
        if b[4] != b'-' {
            return Err(bad(label, grain, "wrong format"));
        }
        month = digits(&b[5..7]).ok_or_else(|| bad(label, grain, "wrong format"))?;
        if !(1..=12).contains(&month) {
            return Err(bad(label, grain, "month must be 01 to 12"));
        }
    }
    if grain.width() >= 10 {
        if b[7] != b'-' {
            return Err(bad(label, grain, "wrong format"));
        }
        day = digits(&b[8..10]).ok_or_else(|| bad(label, grain, "wrong format"))?;
        if day < 1 || day > days_in_month(year, month) {
            return Err(bad(label, grain, "not a real date"));
        }
    }
    if grain.width() == 13 {
        if b[10] != b'T' {
            return Err(bad(label, grain, "wrong format"));
        }
        hour = digits(&b[11..13]).ok_or_else(|| bad(label, grain, "wrong format"))?;
        if hour > 23 {
            return Err(bad(label, grain, "hour must be 00 to 23"));
        }
        let bucket = grain.bucket.unwrap_or(1) as i64;
        if hour % bucket != 0 {
            return Err(bad(
                label,
                grain,
                &format!("hour {hour:02} does not start a {bucket}-hour bucket"),
            ));
        }
    }
    Ok(days_from_civil(year, month, day) * 86_400 + hour * 3_600)
}

/// Label of the period of `grain` that starts at `seconds` (which must be aligned).
pub fn label_at(seconds: i64, grain: &Grain) -> String {
    let (y, m, d) = civil_from_days(seconds.div_euclid(86_400));
    let h = seconds.rem_euclid(86_400) / 3_600;
    match grain.kind {
        Kind::Year => format!("{y:04}"),
        Kind::Month => format!("{y:04}-{m:02}"),
        Kind::Day => format!("{y:04}-{m:02}-{d:02}"),
        Kind::Hour => format!("{y:04}-{m:02}-{d:02}T{h:02}"),
    }
}

const LAST_INSTANT: i64 = 253_402_300_800; // 10000-01-01T00:00:00Z

/// `[start, end)` of a label in seconds since 1970-01-01 UTC.
pub fn interval(label: &str, grain: &Grain) -> Result<(i64, i64), E> {
    let start = parse_label(label, grain)?;
    let end = match grain.fixed_seconds() {
        Some(s) => start + s,
        None => {
            let (y, m, _) = civil_from_days(start.div_euclid(86_400));
            let (ny, nm) = match grain.kind {
                Kind::Year => (y + 1, 1),
                _ if m == 12 => (y + 1, 1),
                _ => (y, m + 1),
            };
            days_from_civil(ny, nm, 1) * 86_400
        }
    };
    if end >= LAST_INSTANT {
        return Err(format!("{label:?} has no exclusive end inside years 0001 to 9999").into());
    }
    Ok((start, end))
}

/// The label after `label` (its exclusive end as a label of the same grain).
pub fn next_label(label: &str, grain: &Grain) -> Result<String, E> {
    let (_, end) = interval(label, grain)?;
    if end >= LAST_INSTANT {
        return Err(format!("{label:?} is the last period before year 10000").into());
    }
    Ok(label_at(end, grain))
}

/// How `left` relates to `right`: equal, contains, contained, overlap or disjoint.
pub fn relation(left: (&str, &Grain), right: (&str, &Grain)) -> Result<&'static str, E> {
    let (a0, a1) = interval(left.0, left.1)?;
    let (b0, b1) = interval(right.0, right.1)?;
    Ok(if (a0, a1) == (b0, b1) {
        "equal"
    } else if a0 <= b0 && b1 <= a1 {
        "contains"
    } else if b0 <= a0 && a1 <= b1 {
        "contained"
    } else if a0 < b1 && b0 < a1 {
        "overlap"
    } else {
        "disjoint"
    })
}

/// Rank of a grain from fine to coarse (hours by bucket size).
fn rank(g: &Grain) -> u32 {
    match g.kind {
        Kind::Hour => g.bucket.unwrap_or(1),
        Kind::Day => 100,
        Kind::Month => 200,
        Kind::Year => 300,
    }
}

/// Whether every period of `fine` lies inside exactly one period of `coarse`.
pub fn nests(fine: &Grain, coarse: &Grain) -> bool {
    match (fine.kind, coarse.kind) {
        (Kind::Hour, Kind::Hour) => coarse.bucket.unwrap_or(1) % fine.bucket.unwrap_or(1) == 0,
        _ => rank(fine) <= rank(coarse),
    }
}

/// The label of the `to` period that contains `label` (a period of `from`). `to` must be the
/// same grain or a coarser one that nests it (e.g. day -> month, 1-hour -> 4-hour buckets).
pub fn rollup_label(label: &str, from: &Grain, to: &Grain) -> Result<String, E> {
    if !nests(from, to) {
        return Err(format!(
            "{} periods do not nest in {}{} periods",
            from.name(),
            to.name(),
            to.bucket.map_or(String::new(), |b| format!(" ({b}-hour)"))
        )
        .into());
    }
    let start = parse_label(label, from)?;
    Ok(match to.kind {
        Kind::Hour => {
            let step = 3_600 * to.bucket.unwrap_or(1) as i64;
            label_at(start - start.rem_euclid(step), to)
        }
        _ => label[..to.width()].to_string(),
    })
}

/// SQL expression giving the `to` label of a `from` label column `col` (no validation; the
/// labels are already valid). Day/month/year are prefixes; hour buckets floor the hour.
pub fn rollup_sql(col: &str, from: &Grain, to: &Grain) -> Result<String, E> {
    if !nests(from, to) {
        return Err(format!(
            "{} periods do not nest in {} periods",
            from.name(),
            to.name()
        )
        .into());
    }
    Ok(match to.kind {
        Kind::Hour if to.bucket == from.bucket => col.to_string(),
        Kind::Hour => {
            let b = to.bucket.unwrap_or(1);
            format!(
                "(left({col}, 11) || lpad(CAST((CAST(substr({col}, 12, 2) AS INTEGER) // {b}) * {b} AS VARCHAR), 2, '0'))"
            )
        }
        _ if to.kind == from.kind => col.to_string(),
        _ => format!("left({col}, {})", to.width()),
    })
}

/// The rule for rolling a field up in time: sum for COUNT and TOTAL, the declared mean, min
/// or max for MEASUREMENT; CATEGORICAL cannot be rolled up.
pub fn time_rule(semantics: &str, declared: Option<&str>) -> Result<&'static str, E> {
    match semantics.to_ascii_uppercase().as_str() {
        "COUNT" | "TOTAL" => Ok("sum"),
        "MEASUREMENT" => match declared {
            Some("mean") => Ok("mean"),
            Some("min") => Ok("min"),
            Some("max") => Ok("max"),
            _ => Err("MEASUREMENT needs a declared time_aggregation of mean, min or max".into()),
        },
        other => Err(format!("{other} cannot be rolled up in time").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn g(t: &str, b: Option<i64>) -> Grain {
        Grain::new(t, b).unwrap()
    }

    #[test]
    fn labels_and_intervals() {
        assert!(parse_label("2024-02-29", &g("day", None)).is_ok());
        assert!(parse_label("2023-02-29", &g("day", None)).is_err());
        assert!(parse_label("2026-11-19T02", &g("hour", Some(4))).is_err());
        assert!(parse_label("2026-11-19T04+07", &g("hour", Some(4))).is_err());
        assert!(parse_label("2026-11-19 04", &g("hour", Some(4))).is_err());
        assert!(Grain::new("day", Some(4)).is_err() && Grain::new("hour", Some(5)).is_err());
        assert!(parse_label("0000", &g("year", None)).is_err());
        let (s, e) = interval("2026-12", &g("month", None)).unwrap();
        assert_eq!(
            (label_at(s, &g("day", None)), label_at(e, &g("day", None))),
            ("2026-12-01".into(), "2027-01-01".into())
        );
        let (s, e) = interval("2026-11-19T04", &g("hour", Some(4))).unwrap();
        assert_eq!(e - s, 4 * 3600);
        assert!(interval("9999", &g("year", None)).is_err());
        assert_eq!(
            next_label("2026-11-30", &g("day", None)).unwrap(),
            "2026-12-01"
        );
        for days in [-1_000_000, -1, 0, 1, 59, 10_957, 2_932_896] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
    }

    #[test]
    fn relations_and_rollups() {
        let (y, d, h4) = (g("year", None), g("day", None), g("hour", Some(4)));
        assert_eq!(
            relation(("2026", &y), ("2026-11-19", &d)).unwrap(),
            "contains"
        );
        assert_eq!(
            relation(("2026-11-19T04", &h4), ("2026-11-19", &d)).unwrap(),
            "contained"
        );
        assert_eq!(
            relation(("2026-11-19", &d), ("2026-11-20", &d)).unwrap(),
            "disjoint"
        );
        assert_eq!(
            relation(
                ("2026-11-19T04", &g("hour", Some(4))),
                ("2026-11-19T06", &g("hour", Some(6)))
            )
            .unwrap(),
            "overlap"
        );
        assert_eq!(
            rollup_label("2026-11-19T05", &g("hour", Some(1)), &h4).unwrap(),
            "2026-11-19T04"
        );
        assert_eq!(
            rollup_label("2026-11-19", &d, &g("month", None)).unwrap(),
            "2026-11"
        );
        assert!(rollup_label("2026-11-19T04", &h4, &g("hour", Some(6))).is_err());
        assert_eq!(time_rule("COUNT", None).unwrap(), "sum");
        assert!(time_rule("MEASUREMENT", None).is_err() && time_rule("CATEGORICAL", None).is_err());
    }
}
