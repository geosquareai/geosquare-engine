//! Operational country boundary (geosquare-v2 docs/boundary-policy.md, reference
//! `geosquare_v2.boundary.OperationalBoundary`): a polygonal boundary in the domain's grid
//! CRS and the four rules COVERS_POINT, CENTROID_COVERED, INTERSECTS and MIN_COVERAGE.
//!
//! A boundary is GeoJSON (Polygon, MultiPolygon, Feature, FeatureCollection,
//! GeometryCollection) or WKT text in lon/lat, or the path of such a file, optionally checked
//! against its SHA-256. Country boundaries have hundreds of thousands of vertices, so each
//! polygon keeps its edges in a bucket grid: a cell no edge reaches is wholly inside or
//! outside (one point test), and only cells the boundary line crosses are clipped.

use crate::geom::{self, Point, Polygon};
use geosquare_core::{domain::Domain, geo, Cell};
use serde_json::Value as Json;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

type E = Box<dyn std::error::Error + Send + Sync>;

/// One polygon of the boundary with its edge index.
struct Part {
    rings: Vec<Vec<Point>>,
    ring_bbox: Vec<(f64, f64, f64, f64)>,
    /// Every edge (a, b) of every ring.
    edges: Vec<(Point, Point)>,
    bbox: (f64, f64, f64, f64),
    nx: usize,
    ny: usize,
    buckets: Vec<Vec<u32>>,
}

impl Part {
    fn new(rings: Vec<Vec<Point>>) -> Option<Part> {
        let mut edges = Vec::new();
        let mut ring_bbox = Vec::new();
        for r in &rings {
            let n = r.len();
            let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
            for i in 0..n {
                let (a, c) = (r[i], r[(i + 1) % n]);
                b = (b.0.min(a.0), b.1.min(a.1), b.2.max(a.0), b.3.max(a.1));
                if a != c {
                    edges.push((a, c));
                }
            }
            ring_bbox.push(b);
        }
        let bbox = *ring_bbox.first()?;
        let side = ((edges.len() as f64).sqrt() as usize).clamp(1, 1024);
        let (nx, ny) = (side, side);
        let mut buckets = vec![Vec::new(); nx * ny];
        let mut p = Part { rings, ring_bbox, edges: Vec::new(), bbox, nx, ny, buckets: Vec::new() };
        for (k, (a, b)) in edges.iter().enumerate() {
            let (c0, c1) = p.cols(a.0.min(b.0), a.0.max(b.0));
            let (r0, r1) = p.rows(a.1.min(b.1), a.1.max(b.1));
            for r in r0..=r1 {
                for c in c0..=c1 {
                    buckets[r * nx + c].push(k as u32);
                }
            }
        }
        p.edges = edges;
        p.buckets = buckets;
        Some(p)
    }

    fn col(&self, x: f64) -> usize {
        let w = (self.bbox.2 - self.bbox.0).max(f64::MIN_POSITIVE);
        (((x - self.bbox.0) / w * self.nx as f64).floor().max(0.0) as usize).min(self.nx - 1)
    }
    fn row(&self, y: f64) -> usize {
        let h = (self.bbox.3 - self.bbox.1).max(f64::MIN_POSITIVE);
        (((y - self.bbox.1) / h * self.ny as f64).floor().max(0.0) as usize).min(self.ny - 1)
    }
    fn cols(&self, x0: f64, x1: f64) -> (usize, usize) {
        (self.col(x0), self.col(x1))
    }
    fn rows(&self, y0: f64, y1: f64) -> (usize, usize) {
        (self.row(y0), self.row(y1))
    }

    /// Whether the polygon covers `p` (inside, or on an edge).
    fn covers(&self, p: Point) -> bool {
        let b = self.bbox;
        if p.0 < b.0 || p.0 > b.2 || p.1 < b.1 || p.1 > b.3 {
            return false;
        }
        let r = self.row(p.1);
        let c0 = self.col(p.0);
        // On an edge: covered (boundary-inclusive, as shapely `covers`).
        for &k in &self.buckets[r * self.nx + c0] {
            if on_segment(p, self.edges[k as usize]) {
                return true;
            }
        }
        // Even-odd crossings of a ray towards +x, through the buckets of this row. An edge
        // in several buckets counts only in the bucket of its crossing point.
        let mut inside = false;
        for c in c0..self.nx {
            for &k in &self.buckets[r * self.nx + c] {
                let (a, e) = self.edges[k as usize];
                if (a.1 > p.1) != (e.1 > p.1) {
                    let x = a.0 + (p.1 - a.1) * (e.0 - a.0) / (e.1 - a.1);
                    if x > p.0 && self.col(x) == c {
                        inside = !inside;
                    }
                }
            }
        }
        inside
    }

    /// Area of the polygon inside the rectangle.
    fn area_in(&self, r: (f64, f64, f64, f64)) -> f64 {
        let b = self.bbox;
        if r.2 <= b.0 || r.0 >= b.2 || r.3 <= b.1 || r.1 >= b.3 {
            return 0.0;
        }
        let (c0, c1) = self.cols(r.0, r.2);
        let (r0, r1) = self.rows(r.1, r.3);
        let crossed = (r0..=r1).any(|row| {
            (c0..=c1).any(|c| {
                self.buckets[row * self.nx + c].iter().any(|&k| {
                    let (a, e) = self.edges[k as usize];
                    a.0.max(e.0) >= r.0 && a.0.min(e.0) <= r.2 && a.1.max(e.1) >= r.1 && a.1.min(e.1) <= r.3
                })
            })
        });
        let full = (r.2 - r.0) * (r.3 - r.1);
        if !crossed {
            return if self.covers(((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0)) { full } else { 0.0 };
        }
        let (mut s, mut t, mut u) = (Vec::new(), Vec::new(), Vec::new());
        let mut area = 0.0;
        for (i, ring) in self.rings.iter().enumerate() {
            let rb = self.ring_bbox[i];
            if r.2 <= rb.0 || r.0 >= rb.2 || r.3 <= rb.1 || r.1 >= rb.3 {
                continue;
            }
            geom::clip_x(ring, r.0, r.2, &mut s, &mut t);
            geom::clip_y(&t, r.1, r.3, &mut s, &mut u);
            let a = geom::ring_area(&u);
            area += if i == 0 { a } else { -a };
        }
        area.clamp(0.0, full)
    }
}

fn on_segment(p: Point, (a, b): (Point, Point)) -> bool {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    if len2 == 0.0 {
        return p == a;
    }
    let cross = (p.0 - a.0) * dy - (p.1 - a.1) * dx;
    let scale = len2.sqrt() * (1.0 + a.0.abs().max(a.1.abs()));
    if cross.abs() > 1e-12 * scale {
        return false;
    }
    let t = ((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2;
    (0.0..=1.0).contains(&t)
}

/// A boundary in one domain's grid CRS.
pub struct Boundary {
    parts: Vec<Part>,
    domain: &'static Domain,
}

impl Boundary {
    /// COVERS_POINT for a lon/lat point.
    pub fn covers_lonlat(&self, lon: f64, lat: f64) -> Result<bool, E> {
        let p = self
            .domain
            .grid_crs
            .forward(lon, lat)
            .ok_or_else(|| format!("projection is undefined at ({lon}, {lat})"))?;
        Ok(self.covers(p))
    }

    pub fn covers(&self, p: Point) -> bool {
        self.parts.iter().any(|part| part.covers(p))
    }

    /// Share of the cell inside the boundary, in [0, 1].
    pub fn coverage(&self, c: Cell) -> Result<f64, E> {
        let b = geo::cell_bounds(self.domain, c).map_err(|e| e.to_string())?;
        let r = (b.min_x, b.min_y, b.max_x, b.max_y);
        let full = (r.2 - r.0) * (r.3 - r.1);
        let a: f64 = self.parts.iter().map(|p| p.area_in(r)).sum();
        Ok((a / full).clamp(0.0, 1.0))
    }

    /// The cell rule: CENTROID_COVERED, INTERSECTS or MIN_COVERAGE (with `min_coverage`).
    pub fn cell_matches(&self, c: Cell, rule: Rule) -> Result<bool, E> {
        Ok(match rule {
            Rule::CoversPoint => return Err("COVERS_POINT is for points; use a cell predicate for cells".into()),
            Rule::CentroidCovered => {
                let b = geo::cell_bounds(self.domain, c).map_err(|e| e.to_string())?;
                self.covers(((b.min_x + b.max_x) / 2.0, (b.min_y + b.max_y) / 2.0))
            }
            Rule::Intersects => self.coverage(c)? > 0.0,
            Rule::MinCoverage(t) => self.coverage(c)? >= t,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rule {
    CoversPoint,
    CentroidCovered,
    Intersects,
    MinCoverage(f64),
}

impl Rule {
    pub fn parse(name: &str, min_coverage: Option<f64>) -> Result<Rule, E> {
        Ok(match name.trim().to_ascii_uppercase().as_str() {
            "COVERS_POINT" => Rule::CoversPoint,
            "CENTROID_COVERED" => Rule::CentroidCovered,
            "INTERSECTS" => Rule::Intersects,
            "MIN_COVERAGE" => {
                let t = min_coverage.ok_or("min_coverage is required for MIN_COVERAGE")?;
                if !(0.0..=1.0).contains(&t) {
                    return Err("min_coverage must be in [0, 1]".into());
                }
                Rule::MinCoverage(t)
            }
            _ => {
                return Err("boundary_predicate must be COVERS_POINT, CENTROID_COVERED, INTERSECTS, or MIN_COVERAGE".into())
            }
        })
    }
}

fn polygons_from_geojson(v: &Json, out: &mut Vec<Polygon>) -> Result<(), E> {
    let ring = |r: &Json| -> Result<Vec<Point>, E> {
        let mut pts: Vec<Point> = r
            .as_array()
            .ok_or("bad GeoJSON ring")?
            .iter()
            .map(|p| match p.as_array().map(Vec::as_slice) {
                Some([x, y, ..]) => Ok((x.as_f64().ok_or("bad coordinate")?, y.as_f64().ok_or("bad coordinate")?)),
                _ => Err(E::from("bad GeoJSON position")),
            })
            .collect::<Result<_, _>>()?;
        if pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        Ok(pts)
    };
    let poly = |c: &Json| -> Result<Polygon, E> {
        Ok(Polygon { rings: c.as_array().ok_or("bad GeoJSON polygon")?.iter().map(ring).collect::<Result<_, _>>()? })
    };
    match v.get("type").and_then(Json::as_str) {
        Some("Polygon") => out.push(poly(&v["coordinates"])?),
        Some("MultiPolygon") => {
            for c in v["coordinates"].as_array().ok_or("bad GeoJSON MultiPolygon")? {
                out.push(poly(c)?);
            }
        }
        Some("Feature") => polygons_from_geojson(&v["geometry"], out)?,
        Some("FeatureCollection") => {
            for f in v["features"].as_array().ok_or("bad FeatureCollection")? {
                polygons_from_geojson(f, out)?;
            }
        }
        Some("GeometryCollection") => {
            for g in v["geometries"].as_array().ok_or("bad GeometryCollection")? {
                polygons_from_geojson(g, out)?;
            }
        }
        // Lines and points in a collection have no area: left out, as make_valid does.
        Some("Point" | "MultiPoint" | "LineString" | "MultiLineString") => {}
        other => return Err(format!("unsupported GeoJSON type {other:?}").into()),
    }
    Ok(())
}

/// Parse boundary text (GeoJSON or WKT, lon/lat) into polygons.
pub fn parse_text(text: &str) -> Result<Vec<Polygon>, E> {
    let t = text.trim_start();
    let polys = if t.starts_with('{') {
        let mut out = Vec::new();
        polygons_from_geojson(&serde_json::from_str(t)?, &mut out)?;
        out
    } else {
        match geom::parse_geometry(t)? {
            geom::Geometry::Polygons(p) => p,
            _ => return Err("boundary must be polygonal (Polygon or MultiPolygon)".into()),
        }
    };
    if polys.is_empty() {
        return Err("boundary must not be empty".into());
    }
    Ok(polys)
}

fn looks_like_text(s: &str) -> bool {
    let t = s.trim_start();
    t.starts_with('{') || {
        let u: String = t.chars().take(16).collect::<String>().to_ascii_uppercase();
        u.starts_with("POLYGON") || u.starts_with("MULTIPOLYGON") || u.starts_with("SRID=")
    }
}

type Cache = Mutex<HashMap<String, Arc<Boundary>>>;
fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The boundary for `spec` (text, or a file path) in domain `d`, parsed once per process.
/// A path is read with `read` (local or remote); `sha256` checks the file's bytes.
pub fn load(
    d: &'static Domain,
    spec: &str,
    sha256: Option<&str>,
    read: impl FnOnce(&str) -> Result<Vec<u8>, E>,
) -> Result<Arc<Boundary>, E> {
    // Long texts are keyed by their length and ends, not hashed whole on every call.
    let key = if spec.len() > 1024 {
        format!("{}|{}|{}|{}|{:?}", d.code, spec.len(), &spec[..256], &spec[spec.len() - 256..], sha256)
    } else {
        format!("{}|{spec}|{sha256:?}", d.code)
    };
    if let Some(b) = cache().lock().unwrap().get(&key) {
        return Ok(b.clone());
    }
    let bytes: Vec<u8> = if looks_like_text(spec) { spec.as_bytes().to_vec() } else { read(spec)? };
    if let Some(want) = sha256 {
        use sha2::{Digest, Sha256};
        let got: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        if !got.eq_ignore_ascii_case(want.trim()) {
            return Err(format!("boundary SHA-256 mismatch: expected {want}, got {got}").into());
        }
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| "boundary file is not UTF-8 text")?;
    let polys = crate::polyfill::to_grid(d, parse_text(text)?, "EPSG:4326", true).map_err(|e| e.to_string())?;
    let parts: Vec<Part> = polys.into_iter().filter_map(|p| Part::new(p.rings)).collect();
    let b = Arc::new(Boundary { parts, domain: d });
    cache().lock().unwrap().insert(key, b.clone());
    Ok(b)
}

/// Read a boundary file: local paths directly, anything else (http, s3) through DuckDB.
pub fn read_file(path: &str) -> Result<Vec<u8>, E> {
    #[cfg(not(target_family = "wasm"))]
    if !path.contains("://") {
        return std::fs::read(path).map_err(|e| format!("boundary file {path} cannot be read: {e}").into());
    }
    let lease = crate::sql::lease().map_err(|e| e.to_string())?;
    let rows = crate::sql::query_text_on(&lease, &format!("SELECT content FROM read_text({})", crate::sql::lit(path)))
        .map_err(|e| format!("boundary file {path} cannot be read: {e}"))?;
    rows.into_iter()
        .next()
        .and_then(|r| r.into_iter().next().flatten())
        .map(String::into_bytes)
        .ok_or_else(|| format!("boundary file {path} is empty").into())
}

/// SHA-256 of a file's bytes, hex.
pub fn file_sha256(path: &str) -> Result<String, E> {
    use sha2::{Digest, Sha256};
    Ok(Sha256::digest(read_file(path)?).iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn point_and_area() {
        let sq = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let hole = vec![(4.0, 4.0), (6.0, 4.0), (6.0, 6.0), (4.0, 6.0)];
        let p = Part::new(vec![sq, hole]).unwrap();
        assert!(p.covers((1.0, 1.0)) && p.covers((0.0, 5.0)) && !p.covers((5.0, 5.0)) && !p.covers((11.0, 5.0)));
        assert!(p.covers((4.0, 5.0)));
        assert!((p.area_in((0.0, 0.0, 10.0, 10.0)) - 96.0).abs() < 1e-9);
        assert!((p.area_in((9.0, 9.0, 11.0, 11.0)) - 1.0).abs() < 1e-9);
        assert!((p.area_in((1.0, 1.0, 2.0, 2.0)) - 1.0).abs() < 1e-9);
    }
}
