//! Geometry primitives used by the text engine: points, affine transforms,
//! Bézier paths and polygons. All coordinates are millimetres, with the y
//! axis pointing down (screen convention).

mod path;
mod poly;

pub use path::{PathCmd, PathData, SubPath};
pub use poly::{MultiPolygon, Polygon, Ring, ring_contains, ring_perimeter, ring_signed_area};

use serde::{Deserialize, Serialize};
use std::ops::{Add, Div, Mul, Neg, Sub};

pub const EPS: f64 = 1e-9;

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    #[inline]
    pub const fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }
    #[inline]
    pub fn dot(self, o: Point) -> f64 {
        self.x * o.x + self.y * o.y
    }
    #[inline]
    pub fn cross(self, o: Point) -> f64 {
        self.x * o.y - self.y * o.x
    }
    #[inline]
    pub fn len(self) -> f64 {
        self.x.hypot(self.y)
    }
    #[inline]
    pub fn len_sq(self) -> f64 {
        self.x * self.x + self.y * self.y
    }
    #[inline]
    pub fn dist(self, o: Point) -> f64 {
        (self - o).len()
    }
    /// Unit vector in the same direction, or zero for a zero vector.
    pub fn normalized(self) -> Point {
        let l = self.len();
        if l < EPS { Point::default() } else { self / l }
    }
    /// Perpendicular (rotated +90° in a y-down frame this is clockwise on screen).
    #[inline]
    pub fn perp(self) -> Point {
        Point::new(-self.y, self.x)
    }
    #[inline]
    pub fn lerp(self, o: Point, t: f64) -> Point {
        self + (o - self) * t
    }
    pub fn rotate(self, angle_rad: f64) -> Point {
        let (s, c) = angle_rad.sin_cos();
        Point::new(self.x * c - self.y * s, self.x * s + self.y * c)
    }
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

impl Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        Point::new(self.x + o.x, self.y + o.y)
    }
}
impl Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }
}
impl Mul<f64> for Point {
    type Output = Point;
    fn mul(self, k: f64) -> Point {
        Point::new(self.x * k, self.y * k)
    }
}
impl Div<f64> for Point {
    type Output = Point;
    fn div(self, k: f64) -> Point {
        Point::new(self.x / k, self.y / k)
    }
}
impl Neg for Point {
    type Output = Point;
    fn neg(self) -> Point {
        Point::new(-self.x, -self.y)
    }
}

/// 2D affine transform: `x' = a*x + c*y + e`, `y' = b*x + d*y + f`
/// (same layout as SVG `matrix(a b c d e f)`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub e: f64,
    pub f: f64,
}

impl Default for Affine {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Affine {
    pub const IDENTITY: Affine = Affine {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };

    pub fn translate(dx: f64, dy: f64) -> Self {
        Affine {
            e: dx,
            f: dy,
            ..Self::IDENTITY
        }
    }
    pub fn scale(sx: f64, sy: f64) -> Self {
        Affine {
            a: sx,
            d: sy,
            ..Self::IDENTITY
        }
    }
    pub fn rotate(angle_rad: f64) -> Self {
        let (s, c) = angle_rad.sin_cos();
        Affine {
            a: c,
            b: s,
            c: -s,
            d: c,
            e: 0.0,
            f: 0.0,
        }
    }
    /// Rotation about a pivot point.
    pub fn rotate_about(angle_rad: f64, p: Point) -> Self {
        Self::translate(p.x, p.y)
            .then_after(Self::rotate(angle_rad))
            .then_after(Self::translate(-p.x, -p.y))
    }
    /// `self * other`: applies `other` first, then `self`.
    pub fn then_after(self, o: Affine) -> Affine {
        Affine {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }
    /// Applies `self` first, then `next`.
    pub fn then(self, next: Affine) -> Affine {
        next.then_after(self)
    }
    #[inline]
    pub fn apply(&self, p: Point) -> Point {
        Point::new(
            self.a * p.x + self.c * p.y + self.e,
            self.b * p.x + self.d * p.y + self.f,
        )
    }
    pub fn determinant(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }
    pub fn inverse(&self) -> Option<Affine> {
        let det = self.determinant();
        if det.abs() < 1e-12 {
            return None;
        }
        let inv = 1.0 / det;
        Some(Affine {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }
    /// Approximate uniform scale factor (geometric mean of axis scales).
    pub fn scale_factor(&self) -> f64 {
        self.determinant().abs().sqrt()
    }
    pub fn is_identity(&self) -> bool {
        *self == Self::IDENTITY
    }
}

/// Axis-aligned bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Default for Rect {
    /// Zero-size rectangle at the origin (serializable, unlike `EMPTY`).
    fn default() -> Self {
        Rect::new(0.0, 0.0, 0.0, 0.0)
    }
}

impl Rect {
    pub const EMPTY: Rect = Rect {
        min_x: f64::INFINITY,
        min_y: f64::INFINITY,
        max_x: f64::NEG_INFINITY,
        max_y: f64::NEG_INFINITY,
    };
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Rect {
            min_x,
            min_y,
            max_x,
            max_y,
        }
    }
    pub fn from_points<I: IntoIterator<Item = Point>>(pts: I) -> Rect {
        let mut r = Rect::EMPTY;
        for p in pts {
            r.include(p);
        }
        r
    }
    pub fn include(&mut self, p: Point) {
        self.min_x = self.min_x.min(p.x);
        self.min_y = self.min_y.min(p.y);
        self.max_x = self.max_x.max(p.x);
        self.max_y = self.max_y.max(p.y);
    }
    pub fn union(&self, o: &Rect) -> Rect {
        Rect {
            min_x: self.min_x.min(o.min_x),
            min_y: self.min_y.min(o.min_y),
            max_x: self.max_x.max(o.max_x),
            max_y: self.max_y.max(o.max_y),
        }
    }
    pub fn is_empty(&self) -> bool {
        !(self.min_x <= self.max_x && self.min_y <= self.max_y)
    }
    pub fn width(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.max_x - self.min_x
        }
    }
    pub fn height(&self) -> f64 {
        if self.is_empty() {
            0.0
        } else {
            self.max_y - self.min_y
        }
    }
    pub fn center(&self) -> Point {
        Point::new(
            (self.min_x + self.max_x) * 0.5,
            (self.min_y + self.max_y) * 0.5,
        )
    }
    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.min_x && p.x <= self.max_x && p.y >= self.min_y && p.y <= self.max_y
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        !(o.min_x > self.max_x
            || o.max_x < self.min_x
            || o.min_y > self.max_y
            || o.max_y < self.min_y)
    }
    pub fn inflate(&self, d: f64) -> Rect {
        Rect::new(
            self.min_x - d,
            self.min_y - d,
            self.max_x + d,
            self.max_y + d,
        )
    }
}

/// Total length of a polyline.
pub fn polyline_length(pts: &[Point]) -> f64 {
    pts.windows(2).map(|w| w[0].dist(w[1])).sum()
}

/// Ramer–Douglas–Peucker simplification of an open polyline.
pub fn simplify_rdp(pts: &[Point], tolerance: f64) -> Vec<Point> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((s, e)) = stack.pop() {
        if e <= s + 1 {
            continue;
        }
        let (a, b) = (pts[s], pts[e]);
        let ab = b - a;
        let ab_len = ab.len();
        let mut best = (0.0, s);
        for (i, &p) in pts.iter().enumerate().take(e).skip(s + 1) {
            let d = if ab_len < EPS {
                p.dist(a)
            } else {
                (ab.cross(p - a) / ab_len).abs()
            };
            if d > best.0 {
                best = (d, i);
            }
        }
        if best.0 > tolerance {
            keep[best.1] = true;
            stack.push((s, best.1));
            stack.push((best.1, e));
        }
    }
    pts.iter()
        .zip(keep)
        .filter(|(_, k)| *k)
        .map(|(p, _)| *p)
        .collect()
}

/// Resample a polyline into points spaced at most `step` apart, preserving
/// all original vertices (so corners stay sharp).
pub fn subdivide_polyline(pts: &[Point], step: f64) -> Vec<Point> {
    if pts.is_empty() || step <= EPS {
        return pts.to_vec();
    }
    let mut out = Vec::with_capacity(pts.len() * 2);
    out.push(pts[0]);
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let d = a.dist(b);
        if d < EPS {
            continue;
        }
        let n = (d / step).ceil().max(1.0) as usize;
        for i in 1..=n {
            out.push(a.lerp(b, i as f64 / n as f64));
        }
    }
    out
}

/// Point at arc-length `s` along a polyline (clamped).
pub fn point_at_length(pts: &[Point], s: f64) -> Point {
    if pts.is_empty() {
        return Point::default();
    }
    let mut acc = 0.0;
    for w in pts.windows(2) {
        let d = w[0].dist(w[1]);
        if acc + d >= s && d > EPS {
            return w[0].lerp(w[1], (s - acc) / d);
        }
        acc += d;
    }
    *pts.last().unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn affine_inverse_roundtrip() {
        let m = Affine::translate(3.0, -2.0)
            .then_after(Affine::rotate(0.7))
            .then_after(Affine::scale(2.0, 0.5));
        let inv = m.inverse().unwrap();
        let p = Point::new(1.25, -4.5);
        let q = inv.apply(m.apply(p));
        assert!(p.dist(q) < 1e-9);
    }

    #[test]
    fn affine_then_order() {
        let t = Affine::translate(10.0, 0.0);
        let s = Affine::scale(2.0, 2.0);
        // scale first then translate
        let m = s.then(t);
        assert_eq!(m.apply(Point::new(1.0, 1.0)), Point::new(12.0, 2.0));
    }

    #[test]
    fn rdp_keeps_corners() {
        let pts = vec![
            Point::new(0.0, 0.0),
            Point::new(1.0, 0.0001),
            Point::new(2.0, 0.0),
            Point::new(2.0, 2.0),
        ];
        let s = simplify_rdp(&pts, 0.01);
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn subdivide_respects_step() {
        let pts = vec![Point::new(0.0, 0.0), Point::new(10.0, 0.0)];
        let s = subdivide_polyline(&pts, 3.0);
        assert_eq!(s.len(), 5);
        for w in s.windows(2) {
            assert!(w[0].dist(w[1]) <= 3.0 + 1e-9);
        }
    }
}
