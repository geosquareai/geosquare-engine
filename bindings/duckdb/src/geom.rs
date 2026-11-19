//! Minimal planar geometry for polyfill: WKT polygons, ring clipping, and area.
//!
//! Only what `polygon_to_cells` needs. Rings are clipped against axis-aligned
//! rectangles with Sutherland-Hodgman; the clipped area of a simple ring is exact
//! even when the ring is concave, so polygon area inside a cell is
//! `|exterior ∩ cell| - Σ|hole ∩ cell|` for a valid polygon.

use std::fmt;

pub type Point = (f64, f64);
pub type Ring = Vec<Point>;

/// One polygon: the exterior ring first, then holes.
#[derive(Debug, Clone)]
pub struct Polygon {
    pub rings: Vec<Ring>,
}

#[derive(Debug)]
pub struct GeomError(pub String);

impl fmt::Display for GeomError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for GeomError {}

fn err<T>(msg: impl Into<String>) -> Result<T, GeomError> {
    Err(GeomError(msg.into()))
}

/// A parsed WKT geometry. Points and line vertices keep their input coordinates.
#[derive(Debug, Clone)]
pub enum Geometry {
    Points(Vec<Point>),
    Lines(Vec<Vec<Point>>),
    Polygons(Vec<Polygon>),
}

impl Geometry {
    pub fn kind(&self) -> &'static str {
        match self {
            Geometry::Points(_) => "point",
            Geometry::Lines(_) => "line",
            Geometry::Polygons(_) => "polygon",
        }
    }
}

/// Parse WKT POINT, MULTIPOINT, LINESTRING, MULTILINESTRING, POLYGON or MULTIPOLYGON.
/// An optional `SRID=n;` prefix and Z/M ordinates are accepted and ignored.
pub fn parse_geometry(text: &str) -> Result<Geometry, GeomError> {
    let mut s = text.trim();
    if let Some(pos) = s.find(';') {
        if s[..pos].trim().to_ascii_uppercase().starts_with("SRID=") {
            s = s[pos + 1..].trim();
        }
    }
    let upper = s.to_ascii_uppercase();
    let kinds = [
        "MULTIPOLYGON",
        "POLYGON",
        "MULTILINESTRING",
        "LINESTRING",
        "MULTIPOINT",
        "POINT",
    ];
    let Some(kind) = kinds.iter().find(|k| upper.starts_with(**k)).copied() else {
        return err("geometry must be WKT POINT, MULTIPOINT, LINESTRING, MULTILINESTRING, POLYGON or MULTIPOLYGON");
    };
    let mut rest = s[kind.len()..].trim_start();
    for tag in ["ZM", "Z", "M"] {
        if rest.to_ascii_uppercase().starts_with(tag)
            && rest[tag.len()..].trim_start().starts_with('(')
        {
            rest = rest[tag.len()..].trim_start();
            break;
        }
    }
    if rest.to_ascii_uppercase().starts_with("EMPTY") {
        return err("geometry must not be empty");
    }
    let mut p = Parser {
        b: rest.as_bytes(),
        i: 0,
    };
    let list = |p: &mut Parser,
                item: &dyn Fn(&mut Parser) -> Result<(), GeomError>|
     -> Result<(), GeomError> {
        p.expect(b'(')?;
        item(p)?;
        while p.peek() == Some(b',') {
            p.i += 1;
            item(p)?;
        }
        p.expect(b')')
    };
    let g = match kind {
        "POINT" => {
            p.expect(b'(')?;
            let pt = p.point()?;
            p.expect(b')')?;
            Geometry::Points(vec![pt])
        }
        "MULTIPOINT" => {
            // Both MULTIPOINT (1 2, 3 4) and MULTIPOINT ((1 2), (3 4)).
            let pts = std::cell::RefCell::new(Vec::new());
            list(&mut p, &|p: &mut Parser| {
                let pt = if p.peek() == Some(b'(') {
                    p.i += 1;
                    let v = p.point()?;
                    p.expect(b')')?;
                    v
                } else {
                    p.point()?
                };
                pts.borrow_mut().push(pt);
                Ok(())
            })?;
            Geometry::Points(pts.into_inner())
        }
        "LINESTRING" => Geometry::Lines(vec![p.line()?]),
        "MULTILINESTRING" => {
            let lines = std::cell::RefCell::new(Vec::new());
            list(&mut p, &|p: &mut Parser| {
                lines.borrow_mut().push(p.line()?);
                Ok(())
            })?;
            Geometry::Lines(lines.into_inner())
        }
        "POLYGON" => Geometry::Polygons(vec![p.polygon()?]),
        _ => {
            let polys = std::cell::RefCell::new(Vec::new());
            list(&mut p, &|p: &mut Parser| {
                polys.borrow_mut().push(p.polygon()?);
                Ok(())
            })?;
            Geometry::Polygons(polys.into_inner())
        }
    };
    p.skip_ws();
    if p.i != p.b.len() {
        return err("unexpected trailing characters in WKT");
    }
    Ok(g)
}

/// Parse a WKT `POLYGON` or `MULTIPOLYGON`; other geometry types are rejected.
pub fn parse_wkt(text: &str) -> Result<Vec<Polygon>, GeomError> {
    match parse_geometry(text)? {
        Geometry::Polygons(p) => Ok(p),
        g => err(format!(
            "geometry must be a WKT POLYGON or MULTIPOLYGON, got a {}",
            g.kind()
        )),
    }
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while self.i < self.b.len() && self.b[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.b.get(self.i).copied()
    }
    fn expect(&mut self, c: u8) -> Result<(), GeomError> {
        if self.peek() == Some(c) {
            self.i += 1;
            Ok(())
        } else {
            err(format!(
                "invalid WKT: expected '{}' at byte {}",
                c as char, self.i
            ))
        }
    }
    fn number(&mut self) -> Result<f64, GeomError> {
        self.skip_ws();
        let start = self.i;
        while self.i < self.b.len()
            && matches!(
                self.b[self.i],
                b'0'..=b'9' | b'+' | b'-' | b'.' | b'e' | b'E'
            )
        {
            self.i += 1;
        }
        let txt = std::str::from_utf8(&self.b[start..self.i]).unwrap_or("");
        txt.parse::<f64>()
            .ok()
            .filter(|v| v.is_finite())
            .ok_or_else(|| GeomError(format!("invalid WKT number at byte {start}")))
    }
    /// One coordinate; Z/M ordinates are read and ignored.
    fn point(&mut self) -> Result<Point, GeomError> {
        let x = self.number()?;
        let y = self.number()?;
        while !matches!(self.peek(), Some(b',') | Some(b')') | None) {
            self.number()?;
        }
        Ok((x, y))
    }
    /// `(x y, x y, ...)`
    fn coords(&mut self) -> Result<Vec<Point>, GeomError> {
        self.expect(b'(')?;
        let mut out = vec![self.point()?];
        while self.peek() == Some(b',') {
            self.i += 1;
            out.push(self.point()?);
        }
        self.expect(b')')?;
        Ok(out)
    }
    fn line(&mut self) -> Result<Vec<Point>, GeomError> {
        let line = self.coords()?;
        if line.len() < 2 {
            return err("a LINESTRING needs at least 2 points");
        }
        Ok(line)
    }
    fn ring(&mut self) -> Result<Ring, GeomError> {
        let mut ring = self.coords()?;
        if ring.len() < 4 || ring.first() != ring.last() {
            return err("each polygon ring must be closed and have at least 4 points");
        }
        ring.pop();
        Ok(ring)
    }
    fn polygon(&mut self) -> Result<Polygon, GeomError> {
        self.expect(b'(')?;
        let mut rings = vec![self.ring()?];
        while self.peek() == Some(b',') {
            self.i += 1;
            rings.push(self.ring()?);
        }
        self.expect(b')')?;
        Ok(Polygon { rings })
    }
}

/// Absolute shoelace area of an open ring.
///
/// Coordinates are taken relative to the first vertex. Grid CRS values are around
/// 1e7 m, so the plain formula loses a small sliver to cancellation (a 1.6e-4 m²
/// triangle in a 5 m cell came out as 0).
pub fn ring_area(ring: &[Point]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let (ox, oy) = ring[0];
    let mut s = 0.0;
    for i in 1..n - 1 {
        let (x1, y1) = (ring[i].0 - ox, ring[i].1 - oy);
        let (x2, y2) = (ring[i + 1].0 - ox, ring[i + 1].1 - oy);
        s += x1 * y2 - x2 * y1;
    }
    (s * 0.5).abs()
}

#[derive(Clone, Copy)]
enum Edge {
    Left(f64),
    Right(f64),
    Bottom(f64),
    Top(f64),
}

impl Edge {
    fn inside(self, p: Point) -> bool {
        match self {
            Edge::Left(v) => p.0 >= v,
            Edge::Right(v) => p.0 <= v,
            Edge::Bottom(v) => p.1 >= v,
            Edge::Top(v) => p.1 <= v,
        }
    }
    fn cross(self, a: Point, b: Point) -> Point {
        match self {
            Edge::Left(v) | Edge::Right(v) => {
                let t = (v - a.0) / (b.0 - a.0);
                (v, a.1 + t * (b.1 - a.1))
            }
            Edge::Bottom(v) | Edge::Top(v) => {
                let t = (v - a.1) / (b.1 - a.1);
                (a.0 + t * (b.0 - a.0), v)
            }
        }
    }
}

fn clip_edge(input: &[Point], e: Edge, out: &mut Vec<Point>) {
    out.clear();
    let n = input.len();
    if n == 0 {
        return;
    }
    let mut prev = input[n - 1];
    let mut prev_in = e.inside(prev);
    for &cur in input {
        let cur_in = e.inside(cur);
        if cur_in {
            if !prev_in {
                out.push(e.cross(prev, cur));
            }
            out.push(cur);
        } else if prev_in {
            out.push(e.cross(prev, cur));
        }
        prev = cur;
        prev_in = cur_in;
    }
}

/// Clip an open ring to the x band `[x0, x1]`.
pub fn clip_x(ring: &[Point], x0: f64, x1: f64, scratch: &mut Vec<Point>, out: &mut Vec<Point>) {
    clip_edge(ring, Edge::Left(x0), scratch);
    clip_edge(scratch, Edge::Right(x1), out);
}

/// Clip an open ring to the y band `[y0, y1]`.
pub fn clip_y(ring: &[Point], y0: f64, y1: f64, scratch: &mut Vec<Point>, out: &mut Vec<Point>) {
    clip_edge(ring, Edge::Bottom(y0), scratch);
    clip_edge(scratch, Edge::Top(y1), out);
}

/// Bounding box `(min_x, min_y, max_x, max_y)` of all exterior rings.
pub fn bounds(polys: &[Polygon]) -> Option<(f64, f64, f64, f64)> {
    let mut b: Option<(f64, f64, f64, f64)> = None;
    for p in polys {
        for &(x, y) in &p.rings[0] {
            b = Some(match b {
                None => (x, y, x, y),
                Some((a, c, d, e)) => (a.min(x), c.min(y), d.max(x), e.max(y)),
            });
        }
    }
    b
}

/// Write polygons as WKT.
pub fn to_wkt(rings: &[Ring]) -> String {
    let body: Vec<String> = rings
        .iter()
        .map(|r| {
            let mut pts: Vec<String> = r.iter().map(|(x, y)| format!("{x} {y}")).collect();
            if let Some((x, y)) = r.first() {
                pts.push(format!("{x} {y}"));
            }
            format!("({})", pts.join(", "))
        })
        .collect();
    format!("POLYGON ({})", body.join(", "))
}

/// Write one polygon ring set as a GeoJSON geometry object.
pub fn to_geojson(rings: &[Ring]) -> String {
    let body: Vec<String> = rings
        .iter()
        .map(|r| {
            let mut pts: Vec<String> = r.iter().map(|(x, y)| format!("[{x},{y}]")).collect();
            if let Some((x, y)) = r.first() {
                pts.push(format!("[{x},{y}]"));
            }
            format!("[{}]", pts.join(","))
        })
        .collect();
    format!(
        "{{\"type\":\"Polygon\",\"coordinates\":[{}]}}",
        body.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_area() {
        let p = parse_wkt("POLYGON ((0 0, 10 0, 10 10, 0 10, 0 0), (2 2, 4 2, 4 4, 2 4, 2 2))")
            .unwrap();
        assert_eq!(p.len(), 1);
        assert_eq!(ring_area(&p[0].rings[0]) - ring_area(&p[0].rings[1]), 96.0);
        let m = parse_wkt(
            "SRID=4326;MULTIPOLYGON Z (((0 0 1, 1 0 1, 1 1 1, 0 0 1)), ((5 5, 6 5, 6 6, 5 5)))",
        )
        .unwrap();
        assert_eq!(m.len(), 2);
        assert!(parse_wkt("POINT (1 2)").is_err());
        assert!(
            matches!(parse_geometry("MULTIPOINT ((1 2), (3 4))").unwrap(), Geometry::Points(p) if p.len() == 2)
        );
        assert!(
            matches!(parse_geometry("MULTIPOINT (1 2, 3 4, 5 6)").unwrap(), Geometry::Points(p) if p.len() == 3)
        );
        assert!(
            matches!(parse_geometry("LINESTRING Z (0 0 1, 1 1 1)").unwrap(), Geometry::Lines(l) if l[0].len() == 2)
        );
        assert!(
            matches!(parse_geometry("MULTILINESTRING ((0 0, 1 1), (2 2, 3 3, 4 4))").unwrap(), Geometry::Lines(l) if l.len() == 2)
        );
        assert!(parse_geometry("LINESTRING (0 0)").is_err());
        assert!(parse_wkt("POLYGON EMPTY").is_err());
    }

    #[test]
    fn concave_clip_area() {
        // U shape: area inside [0,3]x[0,3] minus notch [1,2]x[1,3].
        let u = vec![
            (0.0, 0.0),
            (3.0, 0.0),
            (3.0, 3.0),
            (2.0, 3.0),
            (2.0, 1.0),
            (1.0, 1.0),
            (1.0, 3.0),
            (0.0, 3.0),
        ];
        let (mut s, mut a, mut b) = (Vec::new(), Vec::new(), Vec::new());
        clip_y(&u, 0.0, 3.0, &mut s, &mut a);
        clip_x(&a, 0.0, 3.0, &mut s, &mut b);
        assert!((ring_area(&b) - 7.0).abs() < 1e-12);
        clip_x(&u, 1.0, 2.0, &mut s, &mut b);
        assert!((ring_area(&b) - 1.0).abs() < 1e-12);
    }
}

#[cfg(test)]
mod precision_tests {
    use super::*;

    #[test]
    fn sliver_far_from_origin() {
        // The case that came out as 0 before: a corner sliver of a 5 m cell near x = 1e7 m.
        let (x0, y0) = (10_123_455.0, -9_876_540.0);
        let tri = vec![
            (x0 + 5.0, y0 + 5.0),
            (x0 + 5.0, y0 + 4.940771),
            (x0 + 4.994713, y0 + 5.0),
        ];
        let a = ring_area(&tri);
        assert!((a - 0.5 * 0.059229 * 0.005287).abs() < 1e-9, "{a}");
    }
}
