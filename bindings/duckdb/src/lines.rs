//! Lines and points to cells, following `geosquare_v2.conversion.line_to_cells` and
//! `point_to_cell`.
//!
//! Lines: vertices are projected to the grid CRS (no densification), clipped to the
//! root square, and each cell gets `length_ratio = overlap length / total line length`.
//! Cells come in row-major order; the bounding-box candidate count is limited by
//! `max_candidate_limit`, as in the Python oracle. A segment lying exactly on an
//! internal cell edge belongs to the cell on the edge's high side, the spec 2.0 half-open
//! rule (the oracle gives the same result).

use crate::polyfill::BoxErr;
use geosquare_core::{codec, domain::Domain, geo, Cell};
use std::collections::BTreeMap;

pub type Point = (f64, f64);

fn is_wgs84(crs: &str) -> bool {
    matches!(
        crs.trim().to_ascii_uppercase().as_str(),
        "EPSG:4326" | "WGS84" | "OGC:CRS84" | "CRS84"
    )
}

fn is_grid(crs: &str) -> bool {
    matches!(
        crs.trim().to_ascii_lowercase().as_str(),
        "grid" | "projected"
    )
}

/// Project one coordinate from `source_crs` to the grid CRS.
pub fn project(domain: &Domain, p: Point, source_crs: &str) -> Result<Point, BoxErr> {
    if is_grid(source_crs) {
        return Ok(p);
    }
    if !is_wgs84(source_crs) {
        return Err(format!(
            "source_crs {source_crs:?} is not supported; use 'EPSG:4326' (lon/lat) or 'grid'"
        )
        .into());
    }
    let (lon, lat) = p;
    if !(lon.is_finite()
        && lat.is_finite()
        && (-180.0..=180.0).contains(&lon)
        && (-90.0..=90.0).contains(&lat))
    {
        return Err(format!("invalid lon/lat ({lon}, {lat})").into());
    }
    domain
        .grid_crs
        .forward(lon, lat)
        .ok_or_else(|| format!("projection is undefined at ({lon}, {lat})").into())
}

/// Cells of points, with how many points fall in each cell (first-seen order).
pub fn points_to_cells(
    domain: &Domain,
    points: &[Point],
    level: u8,
    source_crs: &str,
) -> Result<Vec<(Cell, i64)>, BoxErr> {
    let mut order: Vec<Cell> = Vec::new();
    let mut counts: BTreeMap<(u64, u64), i64> = BTreeMap::new();
    for &p in points {
        let cell = if is_grid(source_crs) {
            geo::cell_from_projected(domain, p.0, p.1, level)?
        } else {
            project(domain, p, source_crs)?; // validates lon/lat and source_crs
            geo::point_to_cell(domain, p.0, p.1, level)?
        };
        let n = counts.entry((cell.x_idx, cell.y_idx)).or_insert(0);
        if *n == 0 {
            order.push(cell);
        }
        *n += 1;
    }
    Ok(order
        .into_iter()
        .map(|c| (c, counts[&(c.x_idx, c.y_idx)]))
        .collect())
}

/// Liang-Barsky clip of segment `a`-`b` to the rectangle.
fn clip_segment(a: Point, b: Point, x0: f64, y0: f64, x1: f64, y1: f64) -> Option<(Point, Point)> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [
        (-dx, a.0 - x0),
        (dx, x1 - a.0),
        (-dy, a.1 - y0),
        (dy, y1 - a.1),
    ] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let r = q / p;
            if p < 0.0 {
                t0 = t0.max(r);
            } else {
                t1 = t1.min(r);
            }
            if t0 > t1 {
                return None;
            }
        }
    }
    Some((
        (a.0 + t0 * dx, a.1 + t0 * dy),
        (a.0 + t1 * dx, a.1 + t1 * dy),
    ))
}

/// Cells of lines (already in `source_crs`) at `level`, with length ratios.
pub fn line_to_cells(
    domain: &Domain,
    lines: &[Vec<Point>],
    level: u8,
    source_crs: &str,
    max_candidate_limit: i64,
) -> Result<Vec<(Cell, f64)>, BoxErr> {
    let count = codec::side_cell_count(level)?;
    if max_candidate_limit < 1 {
        return Err("max_candidate_limit must be a positive integer".into());
    }
    let side_root = domain.root_side_m();
    let (rx0, ry0) = (domain.origin_x_m, domain.origin_y_m);
    let (rx1, ry1) = (rx0 + side_root, ry0 + side_root);
    let mut segments = Vec::new();
    for line in lines {
        let pts: Vec<Point> = line
            .iter()
            .map(|p| project(domain, *p, source_crs))
            .collect::<Result<_, _>>()?;
        for w in pts.windows(2) {
            if let Some(seg) = clip_segment(w[0], w[1], rx0, ry0, rx1, ry1) {
                if (seg.1 .0 - seg.0 .0).hypot(seg.1 .1 - seg.0 .1) > 0.0 {
                    segments.push(seg);
                }
            }
        }
    }
    let total: f64 = segments
        .iter()
        .map(|(a, b)| (b.0 - a.0).hypot(b.1 - a.1))
        .sum();
    if segments.is_empty() || total <= 0.0 {
        return Ok(Vec::new());
    }
    let side = side_root / count as f64;
    let idx = |v: f64, o: f64| -> u64 { (((v - o) / side).floor().max(0.0) as u64).min(count - 1) };
    let (mut bx0, mut by0, mut bx1, mut by1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for (a, b) in &segments {
        bx0 = bx0.min(a.0).min(b.0);
        by0 = by0.min(a.1).min(b.1);
        bx1 = bx1.max(a.0).max(b.0);
        by1 = by1.max(a.1).max(b.1);
    }
    let candidates =
        (idx(bx1, rx0) - idx(bx0, rx0) + 1) as u128 * (idx(by1, ry0) - idx(by0, ry0) + 1) as u128;
    if candidates > max_candidate_limit as u128 {
        return Err(format!(
            "line conversion requires {candidates} candidates, exceeding max_candidate_limit={max_candidate_limit}"
        )
        .into());
    }

    // Walk every segment across the grid lines it crosses.
    let mut lengths: BTreeMap<(u64, u64), f64> = BTreeMap::new(); // key (y, x): row-major
    let mut add = |x: u64, y: u64, len: f64| *lengths.entry((y, x)).or_insert(0.0) += len;
    for (a, b) in &segments {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = dx.hypot(dy);
        let mut ts = vec![0.0, 1.0];
        let mut cross = |p0: f64, d: f64, origin: f64| {
            if d == 0.0 {
                return;
            }
            let (lo, hi) = if d > 0.0 { (p0, p0 + d) } else { (p0 + d, p0) };
            let k0 = ((lo - origin) / side).ceil() as i64;
            let k1 = ((hi - origin) / side).floor() as i64;
            for k in k0..=k1 {
                let t = (origin + k as f64 * side - p0) / d;
                if t > 0.0 && t < 1.0 {
                    ts.push(t);
                }
            }
        };
        cross(a.0, dx, rx0);
        cross(a.1, dy, ry0);
        ts.sort_by(|p, q| p.total_cmp(q));
        ts.dedup();
        for w in ts.windows(2) {
            let piece = len * (w[1] - w[0]);
            if piece <= 0.0 {
                continue;
            }
            let tm = 0.5 * (w[0] + w[1]);
            let (mx, my) = (a.0 + tm * dx, a.1 + tm * dy);
            let (cx, cy) = (idx(mx, rx0), idx(my, ry0));
            add(cx, cy, piece);
        }
    }
    Ok(lengths
        .into_iter()
        .map(|((y, x), l)| {
            (
                Cell {
                    level,
                    x_idx: x,
                    y_idx: y,
                },
                (l / total).min(1.0),
            )
        })
        .collect())
}
