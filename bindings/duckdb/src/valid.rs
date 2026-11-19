//! Polygon validity and repair, matching `shapely.make_valid` (GEOS "linework") as used by
//! `geosquare_v2.polyfill.normalize_polygonal_geometry`.
//!
//! GEOS rebuilds an invalid polygonal geometry from its noded linework with the even-odd
//! (parity) rule over all rings together: a bow-tie becomes two triangles, a hole outside
//! its shell becomes a filled part, overlapping multipolygon parts become their XOR, and
//! zero-area spikes vanish. Valid input is returned unchanged, so results on valid
//! polygons stay exact.

use crate::geom::{ring_area, Point, Polygon};
use i_overlay::{core::fill_rule::FillRule, float::simplify::SimplifyShape};

/// Segments `(a, b)` intersect; `proper` excludes contact only at shared endpoints.
fn orient(a: Point, b: Point, c: Point) -> f64 {
    (b.0 - a.0) * (c.1 - a.1) - (b.1 - a.1) * (c.0 - a.0)
}

fn on_segment(a: Point, b: Point, p: Point) -> bool {
    p.0 >= a.0.min(b.0) && p.0 <= a.0.max(b.0) && p.1 >= a.1.min(b.1) && p.1 <= a.1.max(b.1)
}

/// How two segments meet: None, at a single shared endpoint, or otherwise (crossing,
/// T-contact, or collinear overlap).
#[derive(PartialEq)]
enum Contact {
    None,
    Endpoint,
    Other,
}

fn contact(p1: Point, p2: Point, q1: Point, q2: Point) -> Contact {
    let (d1, d2) = (orient(q1, q2, p1), orient(q1, q2, p2));
    let (d3, d4) = (orient(p1, p2, q1), orient(p1, p2, q2));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return Contact::Other;
    }
    let touches = [
        (d1 == 0.0 && on_segment(q1, q2, p1), p1),
        (d2 == 0.0 && on_segment(q1, q2, p2), p2),
        (d3 == 0.0 && on_segment(p1, p2, q1), q1),
        (d4 == 0.0 && on_segment(p1, p2, q2), q2),
    ];
    let points: Vec<Point> = touches
        .iter()
        .filter(|(t, _)| *t)
        .map(|(_, p)| *p)
        .collect();
    if points.is_empty() {
        return Contact::None;
    }
    let shared = |p: &Point| (*p == p1 || *p == p2) && (*p == q1 || *p == q2);
    let distinct: Vec<&Point> = points.iter().fold(Vec::new(), |mut v, p| {
        if !v.contains(&p) {
            v.push(p);
        }
        v
    });
    if distinct.len() == 1 && shared(distinct[0]) {
        Contact::Endpoint
    } else {
        Contact::Other
    }
}

/// Point strictly inside an open ring (even-odd ray cast); points on the boundary are not.
fn inside(ring: &[Point], p: Point) -> Option<bool> {
    let n = ring.len();
    let mut c = false;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        if orient(a, b, p) == 0.0 && on_segment(a, b, p) {
            return None; // on the boundary
        }
        if (a.1 > p.1) != (b.1 > p.1) && p.0 < (b.0 - a.0) * (p.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    Some(c)
}

/// First vertex of `ring` that is not on `other`'s boundary, classified against it.
fn ring_inside(ring: &[Point], other: &[Point]) -> Option<bool> {
    ring.iter().find_map(|p| inside(other, *p))
}

struct Seg {
    a: Point,
    b: Point,
    ring: usize,
    idx: usize,
    len: usize,
}

/// Simple OGC-style validity: simple non-degenerate rings, rings meeting at most at single
/// points, holes inside their shell and outside each other, parts not overlapping.
pub fn is_valid(polys: &[Polygon]) -> bool {
    let mut segs = Vec::new();
    let mut ring_id = 0;
    for p in polys {
        for r in &p.rings {
            // Repeated consecutive vertices are allowed; index segments on the cleaned ring.
            let mut clean = r.clone();
            clean.dedup();
            while clean.len() > 1 && clean.first() == clean.last() {
                clean.pop();
            }
            if clean.len() < 3 || ring_area(&clean) <= 0.0 {
                return false;
            }
            for i in 0..clean.len() {
                let (a, b) = (clean[i], clean[(i + 1) % clean.len()]);
                segs.push(Seg {
                    a,
                    b,
                    ring: ring_id,
                    idx: i,
                    len: clean.len(),
                });
            }
            ring_id += 1;
        }
    }
    // Sweep over x to find contacts between segments.
    segs.sort_by(|s, t| s.a.0.min(s.b.0).total_cmp(&t.a.0.min(t.b.0)));
    let mut active: Vec<usize> = Vec::new();
    for i in 0..segs.len() {
        let s = &segs[i];
        let x0 = s.a.0.min(s.b.0);
        active.retain(|&j| segs[j].a.0.max(segs[j].b.0) >= x0);
        for &j in &active {
            let t = &segs[j];
            if s.a.1.max(s.b.1) < t.a.1.min(t.b.1) || t.a.1.max(t.b.1) < s.a.1.min(s.b.1) {
                continue;
            }
            let c = contact(s.a, s.b, t.a, t.b);
            if c == Contact::None {
                continue;
            }
            if s.ring == t.ring {
                let adjacent = (s.idx + 1) % s.len == t.idx || (t.idx + 1) % t.len == s.idx;
                if !(adjacent && c == Contact::Endpoint) {
                    return false; // self-intersection or self-touch
                }
            } else if c == Contact::Other {
                return false; // rings cross or overlap
            }
        }
        active.push(i);
    }
    // Holes inside their shell and not inside each other; parts not overlapping.
    for p in polys {
        let shell = &p.rings[0];
        for (i, h) in p.rings.iter().enumerate().skip(1) {
            if ring_inside(h, shell) == Some(false) {
                return false;
            }
            for g in p.rings.iter().skip(i + 1) {
                if ring_inside(g, h) == Some(true) || ring_inside(h, g) == Some(true) {
                    return false;
                }
            }
        }
    }
    for (i, p) in polys.iter().enumerate() {
        for q in polys.iter().skip(i + 1) {
            for (a, b) in [(p, q), (q, p)] {
                // a part of b inside a's shell but not in one of a's holes overlaps a
                if ring_inside(&b.rings[0], &a.rings[0]) == Some(true)
                    && !a
                        .rings
                        .iter()
                        .skip(1)
                        .any(|h| ring_inside(&b.rings[0], h) == Some(true))
                {
                    return false;
                }
            }
        }
    }
    true
}

/// Valid input unchanged; invalid input rebuilt with the even-odd rule over all rings.
pub fn make_valid(polys: Vec<Polygon>) -> (Vec<Polygon>, bool) {
    if is_valid(&polys) {
        return (polys, false);
    }
    let contours: Vec<Vec<[f64; 2]>> = polys
        .iter()
        .flat_map(|p| p.rings.iter())
        .map(|r| r.iter().map(|&(x, y)| [x, y]).collect())
        .collect();
    let shapes = contours.simplify_shape(FillRule::EvenOdd);
    let out = shapes
        .into_iter()
        .map(|shape| Polygon {
            rings: shape
                .into_iter()
                .map(|c| c.into_iter().map(|[x, y]| (x, y)).collect())
                .collect(),
        })
        .filter(|p: &Polygon| !p.rings.is_empty() && ring_area(&p.rings[0]) > 0.0)
        .collect();
    (out, true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::parse_wkt;

    fn area(polys: &[Polygon]) -> f64 {
        polys
            .iter()
            .map(|p| {
                ring_area(&p.rings[0]) - p.rings.iter().skip(1).map(|r| ring_area(r)).sum::<f64>()
            })
            .sum()
    }

    #[test]
    fn matches_geos_make_valid() {
        // (WKT, valid?, area after make_valid in GEOS 3.13)
        let cases = [
            ("POLYGON ((0 0, 2 2, 2 0, 0 2, 0 0))", false, 2.0),
            ("POLYGON ((0 0, 4 0, 4 4, 1 4, 1 1, 3 1, 3 3, 0 3, 0 0))", false, 11.0),
            ("POLYGON ((0 0, 2 0, 2 2, 0 2, 0 0), (3 0, 4 0, 4 1, 3 1, 3 0))", false, 5.0),
            ("POLYGON ((0 0, 4 0, 4 4, 0 4, 0 0), (3 1, 5 1, 5 2, 3 2, 3 1))", false, 16.0),
            ("MULTIPOLYGON (((0 0, 2 0, 2 2, 0 2, 0 0)), ((1 1, 3 1, 3 3, 1 3, 1 1)))", false, 6.0),
            ("POLYGON ((0 0, 2 0, 2 2, 1 2, 1 3, 1 2, 0 2, 0 0))", false, 4.0),
            ("POLYGON ((0 0, 2 0, 2 0, 2 2, 0 2, 0 0))", true, 4.0),
            ("POLYGON ((0 0, 4 0, 4 4, 0 4, 0 0), (1 1, 3 1, 3 3, 1 3, 1 1), (1.5 1.5, 2.5 1.5, 2.5 2.5, 1.5 2.5, 1.5 1.5))", false, 13.0),
            ("POLYGON ((0 0, 10 0, 10 10, 0 10, 0 0), (2 2, 4 2, 4 4, 2 4, 2 2))", true, 96.0),
            ("MULTIPOLYGON (((0 0, 10 0, 10 10, 0 10, 0 0), (2 2, 8 2, 8 8, 2 8, 2 2)), ((3 3, 7 3, 7 7, 3 7, 3 3)))", true, 80.0),
            ("POLYGON ((0 0, 4 0, 4 4, 0 4, 0 0), (0 0, 2 1, 1 2, 0 0))", true, 14.5),
        ];
        for (wkt, valid, want) in cases {
            let polys = parse_wkt(wkt).unwrap();
            assert_eq!(is_valid(&polys), valid, "validity of {wkt}");
            let (fixed, repaired) = make_valid(polys);
            assert_eq!(repaired, !valid);
            assert!(
                (area(&fixed) - want).abs() < 1e-6,
                "{wkt}: area {} want {want}",
                area(&fixed)
            );
        }
    }
}
