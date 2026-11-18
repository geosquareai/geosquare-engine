//! Points, cells and cell polygons on a domain grid (spec 2.0, sections 5 and 8).
//!
//! A point is projected into its domain's grid CRS, then indexed with integer arithmetic.
//! A cell's projected shape is an exact square. Its WGS84 polygon is that square, densified
//! before the inverse projection, because a straight edge in the grid CRS is curved in
//! lon/lat.

use std::fmt;

use crate::codec::{self, Cell, CodecError};
use crate::domain::Domain;

/// Errors from point and cell operations. Invalid input is always an error.
#[derive(Debug, Clone, PartialEq)]
pub enum GeoError {
    /// Longitude or latitude is not finite, or is outside -180..=180 or -90..=90.
    InvalidLonLat {
        lon: f64,
        lat: f64,
    },
    /// The domain's explicit datum operation is not released for lon/lat work.
    CrsNotReleased {
        code: &'static str,
    },
    /// The projection is not defined at this longitude and latitude. For a transverse
    /// Mercator, that is more than 90 degrees from the central meridian.
    ForwardUndefined {
        lon: f64,
        lat: f64,
    },
    /// The projected point has no finite lon/lat on the globe.
    InverseUndefined {
        x_m: f64,
        y_m: f64,
    },
    /// The projected point is outside the root square of the domain.
    OutOfDomain {
        x_m: f64,
        y_m: f64,
    },
    /// The maximum polygon segment length must be a positive finite number of metres.
    InvalidSegmentLength(f64),
    Codec(CodecError),
}

impl fmt::Display for GeoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GeoError::InvalidLonLat { lon, lat } => {
                write!(f, "({lon}, {lat}) is not a valid longitude and latitude")
            }
            GeoError::CrsNotReleased { code } => {
                write!(
                    f,
                    "domain {code} is not released for lon/lat: its explicit datum operation is blocked"
                )
            }
            GeoError::ForwardUndefined { lon, lat } => {
                write!(f, "the projection is not defined at ({lon}, {lat})")
            }
            GeoError::InverseUndefined { x_m, y_m } => {
                write!(f, "({x_m}, {y_m}) m has no lon/lat on the globe")
            }
            GeoError::OutOfDomain { x_m, y_m } => {
                write!(f, "({x_m}, {y_m}) m is outside the domain root square")
            }
            GeoError::InvalidSegmentLength(length) => {
                write!(
                    f,
                    "maximum segment length {length} m must be positive and finite"
                )
            }
            GeoError::Codec(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for GeoError {}

impl From<CodecError> for GeoError {
    fn from(error: CodecError) -> Self {
        GeoError::Codec(error)
    }
}

/// A rectangle in grid CRS metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

fn check_lon_lat(lon: f64, lat: f64) -> Result<(), GeoError> {
    let valid = lon.is_finite()
        && lat.is_finite()
        && (-180.0..=180.0).contains(&lon)
        && (-90.0..=90.0).contains(&lat);
    if valid {
        Ok(())
    } else {
        Err(GeoError::InvalidLonLat { lon, lat })
    }
}

/// Index a projected point at `level`. The point must lie in the closed root square.
///
/// The maximum edge of the root belongs to the last cell. Every other cell interval is
/// half-open, as spec 2.0 section 5 requires.
pub fn cell_from_projected(
    domain: &Domain,
    x_m: f64,
    y_m: f64,
    level: u8,
) -> Result<Cell, GeoError> {
    let count = codec::side_cell_count(level)?;
    let x0 = domain.origin_x_m;
    let y0 = domain.origin_y_m;
    let side_root = domain.root_side_m();
    let (x1, y1) = (x0 + side_root, y0 + side_root);
    // Written as a negated test so that NaN is rejected too.
    if !(x0 <= x_m && x_m <= x1 && y0 <= y_m && y_m <= y1) {
        return Err(GeoError::OutOfDomain { x_m, y_m });
    }
    if level == 0 {
        return Ok(Cell {
            level: 0,
            x_idx: 0,
            y_idx: 0,
        });
    }
    let side = side_root / count as f64;
    let index = |value: f64, origin: f64, max: f64| -> u64 {
        if value == max {
            count - 1
        } else {
            // Rounding can put a value just below `max` on the next index. Clamp it back.
            (((value - origin) / side).floor() as u64).min(count - 1)
        }
    };
    Ok(Cell {
        level,
        x_idx: index(x_m, x0, x1),
        y_idx: index(y_m, y0, y1),
    })
}

/// Index a longitude and latitude in degrees at `level`.
pub fn point_to_cell(domain: &Domain, lon: f64, lat: f64, level: u8) -> Result<Cell, GeoError> {
    check_lon_lat(lon, lat)?;
    released(domain)?;
    let (x_m, y_m) = domain
        .grid_crs
        .forward(lon, lat)
        .ok_or(GeoError::ForwardUndefined { lon, lat })?;
    cell_from_projected(domain, x_m, y_m, level)
}

/// Refuse lon/lat work for a domain whose CRS is not verified against pyproj.
fn released(domain: &Domain) -> Result<(), GeoError> {
    if domain.crs_released() {
        Ok(())
    } else {
        Err(GeoError::CrsNotReleased { code: domain.code })
    }
}

/// The exact projected square of a cell, in grid CRS metres (spec 2.0, section 8).
pub fn cell_bounds(domain: &Domain, cell: Cell) -> Result<Bounds, GeoError> {
    let count = codec::validate(cell)?;
    let side = domain.root_side_m() / count as f64;
    let x_idx = cell.x_idx as f64;
    let y_idx = cell.y_idx as f64;
    Ok(Bounds {
        min_x: domain.origin_x_m + x_idx * side,
        min_y: domain.origin_y_m + y_idx * side,
        max_x: domain.origin_x_m + (x_idx + 1.0) * side,
        max_y: domain.origin_y_m + (y_idx + 1.0) * side,
    })
}

/// The cell polygon in WGS84, as closed `(lon, lat)` degrees.
///
/// Each edge of the projected square is split into equal pieces no longer than
/// `max_segment_length_m` before the inverse projection. The ring starts and ends at the
/// same vertex.
pub fn cell_polygon_wgs84(
    domain: &Domain,
    cell: Cell,
    max_segment_length_m: f64,
) -> Result<Vec<(f64, f64)>, GeoError> {
    released(domain)?;
    if !(max_segment_length_m > 0.0 && max_segment_length_m.is_finite()) {
        return Err(GeoError::InvalidSegmentLength(max_segment_length_m));
    }
    let b = cell_bounds(domain, cell)?;
    let corners = [
        (b.min_x, b.min_y),
        (b.max_x, b.min_y),
        (b.max_x, b.max_y),
        (b.min_x, b.max_y),
    ];
    let mut ring = Vec::new();
    for (i, &(ax, ay)) in corners.iter().enumerate() {
        let (bx, by) = corners[(i + 1) % corners.len()];
        let length = (bx - ax).hypot(by - ay);
        let pieces = (length / max_segment_length_m).ceil().max(1.0) as usize;
        for k in 0..pieces {
            let t = k as f64 / pieces as f64;
            let (x, y) = (ax + t * (bx - ax), ay + t * (by - ay));
            let vertex = domain
                .grid_crs
                .inverse(x, y)
                .ok_or(GeoError::InverseUndefined { x_m: x, y_m: y })?;
            ring.push(vertex);
        }
    }
    let first = ring[0];
    ring.push(first);
    Ok(ring)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{self, DOMAINS};

    fn id() -> &'static Domain {
        domain::by_code("ID").unwrap()
    }

    #[test]
    fn root_edges_belong_to_the_last_cell() {
        let d = id();
        let level = 1;
        let first = cell_from_projected(d, -25_000_000.0, -25_000_000.0, level).unwrap();
        assert_eq!((first.x_idx, first.y_idx), (0, 0));
        let last = cell_from_projected(d, 25_000_000.0, 25_000_000.0, level).unwrap();
        assert_eq!((last.x_idx, last.y_idx), (4, 4));
    }

    #[test]
    fn cell_interior_is_half_open() {
        let d = id();
        // Level 1 cells are 10,000 km wide. The boundary at -15,000,000 starts index 1.
        let on_edge = cell_from_projected(d, -15_000_000.0, -25_000_000.0, 1).unwrap();
        assert_eq!(on_edge.x_idx, 1);
        let just_below = cell_from_projected(d, -15_000_000.0 - 1e-6, -25_000_000.0, 1).unwrap();
        assert_eq!(just_below.x_idx, 0);
    }

    #[test]
    fn points_outside_the_root_are_rejected() {
        let d = id();
        for (x, y) in [
            (-25_000_001.0, 0.0),
            (25_000_001.0, 0.0),
            (0.0, 25_000_001.0),
            (f64::NAN, 0.0),
        ] {
            assert!(matches!(
                cell_from_projected(d, x, y, 5),
                Err(GeoError::OutOfDomain { .. })
            ));
        }
    }

    #[test]
    fn level_zero_is_the_root_and_bad_levels_fail() {
        let d = id();
        assert_eq!(
            cell_from_projected(d, 0.0, 0.0, 0),
            Ok(Cell {
                level: 0,
                x_idx: 0,
                y_idx: 0
            })
        );
        assert!(matches!(
            cell_from_projected(d, 0.0, 0.0, 15),
            Err(GeoError::Codec(CodecError::InvalidLevel(15)))
        ));
    }

    #[test]
    fn invalid_lon_lat_is_rejected_before_projecting() {
        let d = id();
        for (lon, lat) in [
            (f64::NAN, 0.0),
            (0.0, f64::INFINITY),
            (181.0, 0.0),
            (0.0, -90.5),
        ] {
            assert!(matches!(
                point_to_cell(d, lon, lat, 5),
                Err(GeoError::InvalidLonLat { .. })
            ));
        }
    }

    #[test]
    fn cell_centre_round_trips_through_lon_lat_in_every_domain() {
        // Cells near the middle of each root square lie inside the projection's valid area.
        // Corner cells can be far outside a transverse Mercator's range, so they are tested
        // separately.
        for domain in DOMAINS.iter().filter(|d| d.crs_released()) {
            for level in [5u8, 9, 12] {
                let count = codec::side_cell_count(level).unwrap();
                let mid = count / 2;
                for (x_idx, y_idx) in [(mid, mid), (mid + 3, mid - 2), (mid - 1, mid + 4)] {
                    let cell = Cell {
                        level,
                        x_idx,
                        y_idx,
                    };
                    let b = cell_bounds(domain, cell).unwrap();
                    let (cx, cy) = ((b.min_x + b.max_x) / 2.0, (b.min_y + b.max_y) / 2.0);
                    let (lon, lat) = domain.grid_crs.inverse(cx, cy).unwrap();
                    let back = point_to_cell(domain, lon, lat, level).unwrap();
                    assert_eq!(back, cell, "{} level {level}", domain.code);
                }
            }
        }
    }

    #[test]
    fn lcc_root_corners_have_wrapped_longitudes() {
        let domain = id();
        let (lon, lat) = domain
            .grid_crs
            .inverse(domain.origin_x_m, domain.origin_y_m)
            .unwrap();
        assert!((-180.0..=180.0).contains(&lon), "lon {lon}");
        assert!((-90.0..=90.0).contains(&lat), "lat {lat}");
    }

    #[test]
    fn points_beyond_the_transverse_mercator_range_fail_loudly() {
        let sg = domain::by_code("SG").unwrap();
        // 103.85 + 95 wraps to a longitude 95 degrees away from the central meridian.
        assert!(matches!(
            point_to_cell(sg, -100.0, 1.0, 5),
            Err(GeoError::ForwardUndefined { .. })
        ));
    }

    #[test]
    fn provisional_vn_operation_is_available() {
        let d = domain::by_code("VN").unwrap();
        assert!(d.crs_released());
        assert!(point_to_cell(d, 107.868144, 15.981051, 5).is_ok());
        assert!(cell_polygon_wgs84(
            d,
            Cell {
                level: 3,
                x_idx: 0,
                y_idx: 0
            },
            1_000.0
        )
        .is_ok());
    }

    #[test]
    fn cell_bounds_are_the_exact_square() {
        let d = id();
        let b = cell_bounds(
            d,
            Cell {
                level: 12,
                x_idx: 475_168,
                y_idx: 491_792,
            },
        )
        .unwrap();
        assert!((b.max_x - b.min_x - 50.0).abs() < 1e-6);
        assert!((b.max_y - b.min_y - 50.0).abs() < 1e-6);
    }

    #[test]
    fn polygon_is_closed_and_its_vertices_project_back_to_the_square() {
        let d = domain::by_code("SG").unwrap();
        // The central cell of the SG root, where the transverse Mercator is well defined.
        let cell = Cell {
            level: 7,
            x_idx: 2500,
            y_idx: 2500,
        };
        let ring = cell_polygon_wgs84(d, cell, 1_000.0).unwrap();
        assert_eq!(ring.first(), ring.last());
        // A 10 km cell split at 1 km gives 40 vertices plus the closing vertex.
        assert_eq!(ring.len(), 41);
        let b = cell_bounds(d, cell).unwrap();
        for (lon, lat) in &ring {
            let (x, y) = d.grid_crs.forward(*lon, *lat).unwrap();
            assert!(x >= b.min_x - 1e-3 && x <= b.max_x + 1e-3, "x {x}");
            assert!(y >= b.min_y - 1e-3 && y <= b.max_y + 1e-3, "y {y}");
        }
    }

    #[test]
    fn polygon_rejects_bad_segment_lengths() {
        let d = domain::by_code("SG").unwrap();
        let cell = Cell {
            level: 3,
            x_idx: 0,
            y_idx: 0,
        };
        for length in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(matches!(
                cell_polygon_wgs84(d, cell, length),
                Err(GeoError::InvalidSegmentLength(_))
            ));
        }
    }
}
