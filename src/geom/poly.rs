//! Polygons with holes, boolean operations and offsetting (via `geo`).

use super::{Affine, EPS, PathData, Point, Rect};
use geo::{Area, BooleanOps, Buffer};
use serde::{Deserialize, Serialize};

/// A closed ring of points. The closing point is NOT repeated.
pub type Ring = Vec<Point>;

/// Simple polygon: one outer ring and zero or more holes.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Polygon {
    pub outer: Ring,
    pub holes: Vec<Ring>,
}

/// A set of disjoint polygons.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MultiPolygon(pub Vec<Polygon>);

/// Signed area of a ring (positive = counter-clockwise in a y-up frame,
/// which is clockwise on a y-down screen).
pub fn ring_signed_area(r: &[Point]) -> f64 {
    if r.len() < 3 {
        return 0.0;
    }
    let mut a = 0.0;
    for i in 0..r.len() {
        let p = r[i];
        let q = r[(i + 1) % r.len()];
        a += p.x * q.y - q.x * p.y;
    }
    a * 0.5
}

/// Even-odd point in ring test.
pub fn ring_contains(r: &[Point], p: Point) -> bool {
    let mut inside = false;
    let n = r.len();
    if n < 3 {
        return false;
    }
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (r[i], r[j]);
        if (a.y > p.y) != (b.y > p.y) {
            let x = (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x;
            if p.x < x {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

fn open_ring(mut pts: Vec<Point>) -> Ring {
    if pts.len() > 1 && pts[0].dist(*pts.last().unwrap()) < 1e-9 {
        pts.pop();
    }
    pts.dedup_by(|a, b| a.dist(*b) < 1e-9);
    pts
}

fn to_geo_ring(r: &[Point]) -> geo::LineString<f64> {
    let mut v: Vec<geo::Coord<f64>> = r.iter().map(|p| geo::Coord { x: p.x, y: p.y }).collect();
    if let Some(&f) = v.first() {
        v.push(f);
    }
    geo::LineString::new(v)
}

fn from_geo_ring(l: &geo::LineString<f64>) -> Ring {
    open_ring(l.0.iter().map(|c| Point::new(c.x, c.y)).collect())
}

impl Polygon {
    pub fn new(outer: Ring, holes: Vec<Ring>) -> Self {
        Polygon { outer, holes }
    }
    pub fn rings(&self) -> impl Iterator<Item = &Ring> {
        std::iter::once(&self.outer).chain(self.holes.iter())
    }
    pub fn area(&self) -> f64 {
        ring_signed_area(&self.outer).abs()
            - self
                .holes
                .iter()
                .map(|h| ring_signed_area(h).abs())
                .sum::<f64>()
    }
    pub fn bounds(&self) -> Rect {
        Rect::from_points(self.outer.iter().copied())
    }
    pub fn contains(&self, p: Point) -> bool {
        ring_contains(&self.outer, p) && !self.holes.iter().any(|h| ring_contains(h, p))
    }
    pub fn to_geo(&self) -> geo::Polygon<f64> {
        geo::Polygon::new(
            to_geo_ring(&self.outer),
            self.holes.iter().map(|h| to_geo_ring(h)).collect(),
        )
    }
    pub fn from_geo(p: &geo::Polygon<f64>) -> Self {
        Polygon {
            outer: from_geo_ring(p.exterior()),
            holes: p
                .interiors()
                .iter()
                .map(from_geo_ring)
                .filter(|h| h.len() >= 3)
                .collect(),
        }
    }
    pub fn transform(&self, m: &Affine) -> Polygon {
        Polygon {
            outer: self.outer.iter().map(|p| m.apply(*p)).collect(),
            holes: self
                .holes
                .iter()
                .map(|h| h.iter().map(|p| m.apply(*p)).collect())
                .collect(),
        }
    }
    /// Perimeter of the outer ring.
    pub fn perimeter(&self) -> f64 {
        ring_perimeter(&self.outer)
    }
}

pub fn ring_perimeter(r: &[Point]) -> f64 {
    if r.len() < 2 {
        return 0.0;
    }
    let mut s = 0.0;
    for i in 0..r.len() {
        s += r[i].dist(r[(i + 1) % r.len()]);
    }
    s
}

impl MultiPolygon {
    pub fn empty() -> Self {
        MultiPolygon(Vec::new())
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn area(&self) -> f64 {
        self.0.iter().map(|p| p.area()).sum()
    }
    pub fn bounds(&self) -> Rect {
        self.0.iter().fold(Rect::EMPTY, |r, p| r.union(&p.bounds()))
    }
    pub fn contains(&self, p: Point) -> bool {
        self.0.iter().any(|poly| poly.contains(p))
    }
    pub fn to_geo(&self) -> geo::MultiPolygon<f64> {
        geo::MultiPolygon::new(self.0.iter().map(|p| p.to_geo()).collect())
    }
    pub fn from_geo(m: &geo::MultiPolygon<f64>) -> Self {
        MultiPolygon(
            m.0.iter()
                .filter(|p| p.unsigned_area() > 1e-9)
                .map(Polygon::from_geo)
                .filter(|p| p.outer.len() >= 3)
                .collect(),
        )
    }
    pub fn transform(&self, m: &Affine) -> MultiPolygon {
        MultiPolygon(self.0.iter().map(|p| p.transform(m)).collect())
    }

    /// Build a clean region from arbitrary closed rings using the even-odd
    /// rule (self-intersections and overlaps are resolved).
    pub fn from_rings_evenodd(rings: &[Vec<Point>]) -> MultiPolygon {
        let mut acc: Option<geo::MultiPolygon<f64>> = None;
        for r in rings {
            let r = open_ring(r.clone());
            if r.len() < 3 || ring_signed_area(&r).abs() < 1e-9 {
                continue;
            }
            // Normalize a possibly self-intersecting ring by unioning with empty.
            let gp = geo::MultiPolygon::new(vec![geo::Polygon::new(to_geo_ring(&r), vec![])]);
            let gp = gp.union(&geo::MultiPolygon::<f64>::new(vec![]));
            acc = Some(match acc {
                None => gp,
                Some(a) => a.xor(&gp),
            });
        }
        acc.map(|m| MultiPolygon::from_geo(&m)).unwrap_or_default()
    }

    /// Region enclosed by the closed sub-paths of a path (even-odd).
    pub fn from_path(path: &PathData, tol: f64) -> MultiPolygon {
        let rings: Vec<Vec<Point>> = path
            .subpaths
            .iter()
            .filter(|s| s.closed)
            .map(|s| s.flatten(tol))
            .collect();
        Self::from_rings_evenodd(&rings)
    }

    pub fn union(&self, o: &MultiPolygon) -> MultiPolygon {
        MultiPolygon::from_geo(&self.to_geo().union(&o.to_geo()))
    }
    pub fn intersection(&self, o: &MultiPolygon) -> MultiPolygon {
        MultiPolygon::from_geo(&self.to_geo().intersection(&o.to_geo()))
    }
    pub fn difference(&self, o: &MultiPolygon) -> MultiPolygon {
        MultiPolygon::from_geo(&self.to_geo().difference(&o.to_geo()))
    }
    pub fn xor(&self, o: &MultiPolygon) -> MultiPolygon {
        MultiPolygon::from_geo(&self.to_geo().xor(&o.to_geo()))
    }
    /// Union of many regions.
    pub fn union_all(items: &[MultiPolygon]) -> MultiPolygon {
        let mut it = items.iter().filter(|m| !m.is_empty());
        let Some(first) = it.next() else {
            return MultiPolygon::empty();
        };
        let mut acc = first.to_geo();
        for m in it {
            acc = acc.union(&m.to_geo());
        }
        MultiPolygon::from_geo(&acc)
    }

    /// Offset the region outward (`d > 0`) or inward (`d < 0`) with round joins.
    /// Inward offsets may split or remove thin parts.
    pub fn offset(&self, d: f64) -> MultiPolygon {
        if d.abs() < EPS || self.is_empty() {
            return self.clone();
        }
        let g = self.to_geo();
        let out = g.buffer(d);
        let mut res = MultiPolygon::from_geo(&out);
        // Remove slivers produced by the buffer at very small widths.
        res.0.retain(|p| p.area() > 1e-6);
        res
    }

    /// Remove polygons smaller than `min_area` (mm²).
    pub fn drop_small(mut self, min_area: f64) -> MultiPolygon {
        self.0.retain(|p| p.area() >= min_area);
        self
    }

    /// Convert back to a path (closed sub-paths).
    pub fn to_path(&self) -> PathData {
        let mut pd = PathData::new();
        for p in &self.0 {
            for r in p.rings() {
                if r.len() >= 3 {
                    pd.subpaths.push(super::SubPath::from_polyline(r, true));
                }
            }
        }
        pd
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x: f64, y: f64, s: f64) -> Vec<Point> {
        vec![
            Point::new(x, y),
            Point::new(x + s, y),
            Point::new(x + s, y + s),
            Point::new(x, y + s),
        ]
    }

    #[test]
    fn evenodd_makes_hole() {
        let m = MultiPolygon::from_rings_evenodd(&[square(0.0, 0.0, 10.0), square(2.0, 2.0, 6.0)]);
        assert_eq!(m.0.len(), 1);
        assert_eq!(m.0[0].holes.len(), 1);
        assert!((m.area() - 64.0).abs() < 1e-6);
        assert!(!m.contains(Point::new(5.0, 5.0)));
        assert!(m.contains(Point::new(1.0, 1.0)));
    }

    #[test]
    fn union_and_difference() {
        let a = MultiPolygon::from_rings_evenodd(&[square(0.0, 0.0, 10.0)]);
        let b = MultiPolygon::from_rings_evenodd(&[square(5.0, 0.0, 10.0)]);
        assert!((a.union(&b).area() - 150.0).abs() < 1e-6);
        assert!((a.difference(&b).area() - 50.0).abs() < 1e-6);
        assert!((a.intersection(&b).area() - 50.0).abs() < 1e-6);
    }

    #[test]
    fn offset_grows_and_shrinks() {
        let a = MultiPolygon::from_rings_evenodd(&[square(0.0, 0.0, 10.0)]);
        let grown = a.offset(1.0);
        let b = grown.bounds();
        assert!((b.width() - 12.0).abs() < 0.01);
        let shrunk = a.offset(-1.0);
        assert!((shrunk.area() - 64.0).abs() < 0.01);
        assert!(a.offset(-6.0).is_empty());
    }

    #[test]
    fn from_path_ellipse() {
        let p = PathData::ellipse(0.0, 0.0, 10.0, 5.0);
        let m = MultiPolygon::from_path(&p, 0.01);
        let expected = std::f64::consts::PI * 50.0;
        assert!((m.area() - expected).abs() / expected < 0.01);
    }
}
