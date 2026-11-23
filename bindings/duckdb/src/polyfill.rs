//! Polygon to cells, following `geosquare_v2.polyfill.polyfill_stream` (spec 2.0, section 9).
//!
//! Steps, in the same order as the Python oracle:
//! 1. transform every vertex to the grid CRS (`always_xy`, no densification);
//! 2. clip to the mathematical root square;
//! 3. bound candidate columns/rows with `floor`, clamped to the root, and refuse more
//!    than `max_candidate_limit` candidates before enumerating;
//! 4. per cell, intersect in the grid CRS; coverage = intersection area / cell area, in the
//!    grid plane (GRID_PLANAR) or after projecting both to Equal Earth, EPSG:8857
//!    (EQUAL_AREA); clamped like `_clamp_ratio`; keep the cell when
//!    `ratio > 0 && ratio >= min_coverage`;
//! 5. yield cells in row-major order (y outer, x inner).
//!
//! Invalid input polygons are repaired first, like `make_valid` in the oracle (see
//! `valid.rs`). Boundary predicates are not supported here.

use crate::geom::{self, Polygon};
use geosquare_core::{codec, domain::Domain, Cell};

pub struct PolyfillOptions {
    pub source_crs: String,
    pub coverage_mode: String,
    pub min_coverage: f64,
    pub max_candidate_limit: i64,
    pub allow_candidate_crs: bool,
}

impl Default for PolyfillOptions {
    fn default() -> Self {
        Self {
            source_crs: "EPSG:4326".into(),
            coverage_mode: "GRID_PLANAR".into(),
            min_coverage: 0.0,
            max_candidate_limit: 100_000,
            allow_candidate_crs: false,
        }
    }
}

pub type BoxErr = Box<dyn std::error::Error>;

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

/// Transform polygons from `source_crs` to the domain grid CRS.
pub fn to_grid(
    domain: &Domain,
    polys: Vec<Polygon>,
    source_crs: &str,
    allow_candidate_crs: bool,
) -> Result<Vec<Polygon>, BoxErr> {
    if is_grid(source_crs) {
        return Ok(polys);
    }
    if !is_wgs84(source_crs) {
        return Err(format!(
            "source_crs {source_crs:?} is not supported; use 'EPSG:4326' (lon/lat) or 'grid'"
        )
        .into());
    }
    if !(domain.crs_released() || allow_candidate_crs && domain.crs_available()) {
        return Err(format!(
            "domain {} is not released for lon/lat: pass allow_candidate_crs := true for technical use",
            domain.code
        )
        .into());
    }
    let mut out = Vec::with_capacity(polys.len());
    for p in polys {
        let mut rings = Vec::with_capacity(p.rings.len());
        for r in p.rings {
            let mut ring = Vec::with_capacity(r.len());
            for (lon, lat) in r {
                if !(lon.is_finite()
                    && lat.is_finite()
                    && (-180.0..=180.0).contains(&lon)
                    && (-90.0..=90.0).contains(&lat))
                {
                    return Err(format!("invalid lon/lat vertex ({lon}, {lat})").into());
                }
                let xy = domain
                    .grid_crs
                    .forward(lon, lat)
                    .ok_or_else(|| format!("projection is undefined at ({lon}, {lat})"))?;
                ring.push(xy);
            }
            rings.push(ring);
        }
        out.push(Polygon { rings });
    }
    Ok(out)
}

fn clamp_ratio(ratio: f64) -> Result<f64, BoxErr> {
    const TOL: f64 = 1e-10;
    if (-TOL..=0.0).contains(&ratio) {
        return Ok(0.0);
    }
    if (1.0..=1.0 + TOL).contains(&ratio) {
        return Ok(1.0);
    }
    if !(0.0..=1.0).contains(&ratio) {
        return Err(format!("coverage ratio {ratio} is outside [0, 1]").into());
    }
    Ok(ratio)
}

/// Cells of `polys` (already in `source_crs`) at `level`, with coverage ratios.
pub fn polyfill(
    domain: &Domain,
    polys: Vec<Polygon>,
    level: u8,
    opt: &PolyfillOptions,
) -> Result<Vec<(Cell, f64)>, BoxErr> {
    let count = codec::side_cell_count(level)?;
    if opt.max_candidate_limit < 1 {
        return Err("max_candidate_limit must be a positive integer".into());
    }
    if !(opt.min_coverage.is_finite() && (0.0..=1.0).contains(&opt.min_coverage)) {
        return Err("min_coverage must be a finite number in [0, 1]".into());
    }
    let equal_area = match opt.coverage_mode.to_ascii_uppercase().as_str() {
        "GRID_PLANAR" => false,
        "EQUAL_AREA" => true,
        _ => return Err("coverage_mode must be GRID_PLANAR or EQUAL_AREA".into()),
    };
    let (polys, _) = crate::valid::make_valid(polys);
    let projected = to_grid(domain, polys, &opt.source_crs, opt.allow_candidate_crs)?;
    // Area of a grid-CRS ring after projecting its vertices to Equal Earth.
    let ea_area = |ring: &[geom::Point]| -> Result<f64, BoxErr> {
        let pts = ring
            .iter()
            .map(|&(x, y)| {
                domain
                    .grid_crs
                    .inverse(x, y)
                    .map(|(lon, lat)| crate::equal_area::forward(lon, lat))
                    .ok_or_else(|| {
                        BoxErr::from(format!("inverse projection is undefined at ({x}, {y})"))
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(geom::ring_area(&pts))
    };
    let side_root = domain.root_side_m();
    let (rx0, ry0) = (domain.origin_x_m, domain.origin_y_m);
    let (rx1, ry1) = (rx0 + side_root, ry0 + side_root);

    // Clip to the root square.
    let (mut s, mut a, mut b) = (Vec::new(), Vec::new(), Vec::new());
    let mut clipped: Vec<Polygon> = Vec::new();
    for p in projected {
        let mut rings = Vec::new();
        for (i, r) in p.rings.iter().enumerate() {
            geom::clip_y(r, ry0, ry1, &mut s, &mut a);
            geom::clip_x(&a, rx0, rx1, &mut s, &mut b);
            if b.len() >= 3 && geom::ring_area(&b) > 0.0 {
                rings.push(b.clone());
            } else if i == 0 {
                break; // exterior outside the root: drop the polygon
            }
        }
        if !rings.is_empty() {
            clipped.push(Polygon { rings });
        }
    }
    let Some((min_x, min_y, max_x, max_y)) = geom::bounds(&clipped) else {
        return Ok(Vec::new());
    };
    let side = side_root / count as f64;
    let idx = |v: f64, o: f64| -> u64 { (((v - o) / side).floor().max(0.0) as u64).min(count - 1) };
    let (c0, c1) = (idx(min_x, rx0), idx(max_x, rx0));
    let (r0, r1) = (idx(min_y, ry0), idx(max_y, ry0));
    let candidates = (c1 - c0 + 1) as u128 * (r1 - r0 + 1) as u128;
    if candidates > opt.max_candidate_limit as u128 {
        return Err(format!(
            "polyfill requires {candidates} candidates, exceeding max_candidate_limit={}",
            opt.max_candidate_limit
        )
        .into());
    }

    let cell_area = side * side;
    let mut out = Vec::new();
    let mut band_rings: Vec<(bool, Vec<geom::Point>)> = Vec::new();
    for row in r0..=r1 {
        let y0 = ry0 + row as f64 * side;
        let y1 = ry0 + (row + 1) as f64 * side;
        // Clip every ring to this row once; cells then only clip in x.
        band_rings.clear();
        for p in &clipped {
            for (i, r) in p.rings.iter().enumerate() {
                geom::clip_y(r, y0, y1, &mut s, &mut a);
                if a.len() >= 3 {
                    band_rings.push((i == 0, a.clone()));
                }
            }
        }
        if band_rings.is_empty() {
            continue;
        }
        for col in c0..=c1 {
            let x0 = rx0 + col as f64 * side;
            let x1 = rx0 + (col + 1) as f64 * side;
            let mut area = 0.0;
            let mut ea = 0.0;
            for (is_exterior, r) in &band_rings {
                geom::clip_x(r, x0, x1, &mut s, &mut b);
                let part = geom::ring_area(&b);
                let sign = if *is_exterior { 1.0 } else { -1.0 };
                area += sign * part;
                if equal_area && part > 0.0 {
                    ea += sign * ea_area(&b)?;
                }
            }
            if area <= 0.0 {
                continue;
            }
            let ratio = if equal_area {
                let corners = [(x0, y0), (x1, y0), (x1, y1), (x0, y1)];
                clamp_ratio(ea / ea_area(&corners)?)?
            } else {
                clamp_ratio(area / cell_area)?
            };
            if ratio > 0.0 && ratio >= opt.min_coverage {
                out.push((
                    Cell {
                        level,
                        x_idx: col,
                        y_idx: row,
                    },
                    ratio,
                ));
            }
        }
    }
    Ok(out)
}
