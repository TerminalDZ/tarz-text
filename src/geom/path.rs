//! Bézier path model (SVG-like) with parsing, serialization and flattening.

use super::{Affine, EPS, Point, Rect};
use serde::{Deserialize, Serialize};

/// One drawing command. Coordinates are absolute (mm).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum PathCmd {
    /// Line to.
    L { p: Point },
    /// Quadratic Bézier.
    Q { c: Point, p: Point },
    /// Cubic Bézier.
    C { c1: Point, c2: Point, p: Point },
}

impl PathCmd {
    pub fn end(&self) -> Point {
        match *self {
            PathCmd::L { p } | PathCmd::Q { p, .. } | PathCmd::C { p, .. } => p,
        }
    }
    fn map(&self, f: impl Fn(Point) -> Point) -> PathCmd {
        match *self {
            PathCmd::L { p } => PathCmd::L { p: f(p) },
            PathCmd::Q { c, p } => PathCmd::Q { c: f(c), p: f(p) },
            PathCmd::C { c1, c2, p } => PathCmd::C {
                c1: f(c1),
                c2: f(c2),
                p: f(p),
            },
        }
    }
}

/// A continuous sub-path: a start point and a sequence of segments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct SubPath {
    pub start: Point,
    pub segs: Vec<PathCmd>,
    pub closed: bool,
}

impl SubPath {
    pub fn new(start: Point) -> Self {
        SubPath {
            start,
            segs: Vec::new(),
            closed: false,
        }
    }
    pub fn from_polyline(pts: &[Point], closed: bool) -> Self {
        let mut sp = SubPath::new(pts.first().copied().unwrap_or_default());
        for &p in pts.iter().skip(1) {
            sp.segs.push(PathCmd::L { p });
        }
        sp.closed = closed;
        sp
    }
    pub fn end(&self) -> Point {
        self.segs.last().map(|s| s.end()).unwrap_or(self.start)
    }
    /// All anchor and control points.
    pub fn all_points(&self) -> impl Iterator<Item = Point> + '_ {
        std::iter::once(self.start).chain(self.segs.iter().flat_map(|s| {
            let v: Vec<Point> = match *s {
                PathCmd::L { p } => vec![p],
                PathCmd::Q { c, p } => vec![c, p],
                PathCmd::C { c1, c2, p } => vec![c1, c2, p],
            };
            v.into_iter()
        }))
    }
    /// Flatten to a polyline with the given chord tolerance (mm).
    /// For closed sub-paths the result is closed (last point == first point).
    pub fn flatten(&self, tol: f64) -> Vec<Point> {
        let tol = tol.max(1e-4);
        let mut out = vec![self.start];
        let mut cur = self.start;
        for s in &self.segs {
            match *s {
                PathCmd::L { p } => out.push(p),
                PathCmd::Q { c, p } => {
                    // Elevate to cubic.
                    let c1 = cur + (c - cur) * (2.0 / 3.0);
                    let c2 = p + (c - p) * (2.0 / 3.0);
                    flatten_cubic(cur, c1, c2, p, tol, &mut out);
                }
                PathCmd::C { c1, c2, p } => flatten_cubic(cur, c1, c2, p, tol, &mut out),
            }
            cur = s.end();
        }
        if self.closed && out.len() > 1 && out[0].dist(*out.last().unwrap()) > EPS {
            out.push(out[0]);
        }
        dedup(&mut out);
        out
    }
    pub fn transform(&self, m: &Affine) -> SubPath {
        SubPath {
            start: m.apply(self.start),
            segs: self.segs.iter().map(|s| s.map(|p| m.apply(p))).collect(),
            closed: self.closed,
        }
    }
    pub fn reversed(&self) -> SubPath {
        let mut pts = vec![self.start];
        for s in &self.segs {
            pts.push(s.end());
        }
        let mut sp = SubPath::new(*pts.last().unwrap());
        for (i, s) in self.segs.iter().enumerate().rev() {
            let from = pts[i];
            sp.segs.push(match *s {
                PathCmd::L { .. } => PathCmd::L { p: from },
                PathCmd::Q { c, .. } => PathCmd::Q { c, p: from },
                PathCmd::C { c1, c2, .. } => PathCmd::C {
                    c1: c2,
                    c2: c1,
                    p: from,
                },
            });
        }
        sp.closed = self.closed;
        sp
    }
}

fn dedup(pts: &mut Vec<Point>) {
    pts.dedup_by(|a, b| a.dist(*b) < 1e-7);
}

fn flatten_cubic(p0: Point, p1: Point, p2: Point, p3: Point, tol: f64, out: &mut Vec<Point>) {
    // Number of segments from the second-difference bound.
    let dd = ((p0 - p1 * 2.0 + p2).len()).max((p1 - p2 * 2.0 + p3).len());
    let n = ((dd * 0.75 / tol).sqrt().ceil() as usize).clamp(1, 512);
    for i in 1..=n {
        let t = i as f64 / n as f64;
        let mt = 1.0 - t;
        let p = p0 * (mt * mt * mt)
            + p1 * (3.0 * mt * mt * t)
            + p2 * (3.0 * mt * t * t)
            + p3 * (t * t * t);
        out.push(p);
    }
}

/// A full path: one or more sub-paths. Closed sub-paths define regions with
/// the even-odd rule (holes are simply inner sub-paths).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PathData {
    pub subpaths: Vec<SubPath>,
}

impl PathData {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn is_empty(&self) -> bool {
        self.subpaths.is_empty()
    }
    pub fn from_polyline(pts: &[Point], closed: bool) -> Self {
        PathData {
            subpaths: vec![SubPath::from_polyline(pts, closed)],
        }
    }
    pub fn rect(x: f64, y: f64, w: f64, h: f64) -> Self {
        Self::from_polyline(
            &[
                Point::new(x, y),
                Point::new(x + w, y),
                Point::new(x + w, y + h),
                Point::new(x, y + h),
            ],
            true,
        )
    }
    /// Ellipse built from 4 cubic arcs.
    pub fn ellipse(cx: f64, cy: f64, rx: f64, ry: f64) -> Self {
        const K: f64 = 0.552_284_749_830_793_4;
        let mut sp = SubPath::new(Point::new(cx + rx, cy));
        let pts = [
            (
                Point::new(cx + rx, cy + ry * K),
                Point::new(cx + rx * K, cy + ry),
                Point::new(cx, cy + ry),
            ),
            (
                Point::new(cx - rx * K, cy + ry),
                Point::new(cx - rx, cy + ry * K),
                Point::new(cx - rx, cy),
            ),
            (
                Point::new(cx - rx, cy - ry * K),
                Point::new(cx - rx * K, cy - ry),
                Point::new(cx, cy - ry),
            ),
            (
                Point::new(cx + rx * K, cy - ry),
                Point::new(cx + rx, cy - ry * K),
                Point::new(cx + rx, cy),
            ),
        ];
        for (c1, c2, p) in pts {
            sp.segs.push(PathCmd::C { c1, c2, p });
        }
        sp.closed = true;
        PathData { subpaths: vec![sp] }
    }
    pub fn transform(&self, m: &Affine) -> PathData {
        PathData {
            subpaths: self.subpaths.iter().map(|s| s.transform(m)).collect(),
        }
    }
    pub fn flatten(&self, tol: f64) -> Vec<Vec<Point>> {
        self.subpaths
            .iter()
            .map(|s| s.flatten(tol))
            .filter(|p| p.len() >= 2)
            .collect()
    }
    /// Bounding box of the flattened geometry.
    pub fn bounds(&self) -> Rect {
        Rect::from_points(self.flatten(0.05).into_iter().flatten())
    }
    pub fn has_closed(&self) -> bool {
        self.subpaths.iter().any(|s| s.closed)
    }

    /// Parse SVG path data (`d` attribute). Supports M L H V C S Q T A Z in
    /// both absolute and relative forms. Arcs are converted to cubics.
    pub fn from_svg(d: &str) -> Result<PathData, String> {
        SvgParser::new(d).parse()
    }

    /// Serialize to SVG path data with absolute commands.
    pub fn to_svg(&self) -> String {
        let mut s = String::new();
        let f = |v: f64| {
            let r = (v * 1000.0).round() / 1000.0;
            if r == 0.0 {
                "0".to_string()
            } else {
                format!("{}", r)
            }
        };
        for sp in &self.subpaths {
            s.push_str(&format!("M{} {}", f(sp.start.x), f(sp.start.y)));
            for seg in &sp.segs {
                match *seg {
                    PathCmd::L { p } => s.push_str(&format!("L{} {}", f(p.x), f(p.y))),
                    PathCmd::Q { c, p } => {
                        s.push_str(&format!("Q{} {} {} {}", f(c.x), f(c.y), f(p.x), f(p.y)))
                    }
                    PathCmd::C { c1, c2, p } => s.push_str(&format!(
                        "C{} {} {} {} {} {}",
                        f(c1.x),
                        f(c1.y),
                        f(c2.x),
                        f(c2.y),
                        f(p.x),
                        f(p.y)
                    )),
                }
            }
            if sp.closed {
                s.push('Z');
            }
        }
        s
    }
}

struct SvgParser<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> SvgParser<'a> {
    fn new(d: &'a str) -> Self {
        SvgParser {
            b: d.as_bytes(),
            i: 0,
        }
    }
    fn skip_ws(&mut self) {
        while self.i < self.b.len()
            && (self.b[self.i].is_ascii_whitespace() || self.b[self.i] == b',')
        {
            self.i += 1;
        }
    }
    fn peek_cmd(&mut self) -> Option<u8> {
        self.skip_ws();
        self.b
            .get(self.i)
            .copied()
            .filter(|c| c.is_ascii_alphabetic())
    }
    fn has_number(&mut self) -> bool {
        self.skip_ws();
        matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit() || *c == b'-' || *c == b'+' || *c == b'.')
    }
    fn number(&mut self) -> Result<f64, String> {
        self.skip_ws();
        let start = self.i;
        let b = self.b;
        if self.i < b.len() && (b[self.i] == b'-' || b[self.i] == b'+') {
            self.i += 1;
        }
        let mut seen_dot = false;
        while self.i < b.len() && (b[self.i].is_ascii_digit() || (b[self.i] == b'.' && !seen_dot)) {
            if b[self.i] == b'.' {
                seen_dot = true;
            }
            self.i += 1;
        }
        if self.i < b.len() && (b[self.i] == b'e' || b[self.i] == b'E') {
            let save = self.i;
            self.i += 1;
            if self.i < b.len() && (b[self.i] == b'-' || b[self.i] == b'+') {
                self.i += 1;
            }
            if self.i < b.len() && b[self.i].is_ascii_digit() {
                while self.i < b.len() && b[self.i].is_ascii_digit() {
                    self.i += 1;
                }
            } else {
                self.i = save;
            }
        }
        let s = std::str::from_utf8(&b[start..self.i]).map_err(|e| e.to_string())?;
        s.parse::<f64>()
            .map_err(|_| format!("bad number at {}", start))
    }
    fn flag(&mut self) -> Result<bool, String> {
        self.skip_ws();
        match self.b.get(self.i) {
            Some(b'0') => {
                self.i += 1;
                Ok(false)
            }
            Some(b'1') => {
                self.i += 1;
                Ok(true)
            }
            _ => Err(format!("bad arc flag at {}", self.i)),
        }
    }
    fn pt(&mut self, rel: bool, cur: Point) -> Result<Point, String> {
        let x = self.number()?;
        let y = self.number()?;
        Ok(if rel {
            Point::new(cur.x + x, cur.y + y)
        } else {
            Point::new(x, y)
        })
    }

    fn parse(mut self) -> Result<PathData, String> {
        let mut out = PathData::new();
        let mut cur = Point::default();
        let mut sp: Option<SubPath> = None;
        let mut last_ctrl: Option<(u8, Point)> = None;
        let mut cmd: u8 = 0;
        loop {
            if let Some(c) = self.peek_cmd() {
                cmd = c;
                self.i += 1;
            } else if self.i >= self.b.len() {
                break;
            } else if cmd == 0 {
                return Err("path must start with a command".into());
            } else if !self.has_number() {
                return Err(format!("unexpected character at {}", self.i));
            }
            let rel = cmd.is_ascii_lowercase();
            let up = cmd.to_ascii_uppercase();
            match up {
                b'M' => {
                    if let Some(s) = sp.take() {
                        out.subpaths.push(s);
                    }
                    cur = self.pt(rel, cur)?;
                    sp = Some(SubPath::new(cur));
                    last_ctrl = None;
                    // subsequent pairs are implicit line-tos
                    cmd = if rel { b'l' } else { b'L' };
                    continue;
                }
                b'Z' => {
                    if let Some(s) = sp.as_mut() {
                        s.closed = true;
                        let start = s.start;
                        out.subpaths.push(sp.take().unwrap());
                        // A command after Z without M starts at the same point.
                        sp = None;
                        cur = start;
                    }
                    last_ctrl = None;
                    cmd = 0;
                    continue;
                }
                _ => {}
            }
            let s = sp.get_or_insert_with(|| SubPath::new(cur));
            match up {
                b'L' => {
                    let p = self.pt(rel, cur)?;
                    s.segs.push(PathCmd::L { p });
                    cur = p;
                    last_ctrl = None;
                }
                b'H' => {
                    let x = self.number()?;
                    let p = Point::new(if rel { cur.x + x } else { x }, cur.y);
                    s.segs.push(PathCmd::L { p });
                    cur = p;
                    last_ctrl = None;
                }
                b'V' => {
                    let y = self.number()?;
                    let p = Point::new(cur.x, if rel { cur.y + y } else { y });
                    s.segs.push(PathCmd::L { p });
                    cur = p;
                    last_ctrl = None;
                }
                b'C' => {
                    let c1 = self.pt(rel, cur)?;
                    let c2 = self.pt(rel, cur)?;
                    let p = self.pt(rel, cur)?;
                    s.segs.push(PathCmd::C { c1, c2, p });
                    last_ctrl = Some((b'C', c2));
                    cur = p;
                }
                b'S' => {
                    let c1 = match last_ctrl {
                        Some((b'C', c)) => cur * 2.0 - c,
                        _ => cur,
                    };
                    let c2 = self.pt(rel, cur)?;
                    let p = self.pt(rel, cur)?;
                    s.segs.push(PathCmd::C { c1, c2, p });
                    last_ctrl = Some((b'C', c2));
                    cur = p;
                }
                b'Q' => {
                    let c = self.pt(rel, cur)?;
                    let p = self.pt(rel, cur)?;
                    s.segs.push(PathCmd::Q { c, p });
                    last_ctrl = Some((b'Q', c));
                    cur = p;
                }
                b'T' => {
                    let c = match last_ctrl {
                        Some((b'Q', c)) => cur * 2.0 - c,
                        _ => cur,
                    };
                    let p = self.pt(rel, cur)?;
                    s.segs.push(PathCmd::Q { c, p });
                    last_ctrl = Some((b'Q', c));
                    cur = p;
                }
                b'A' => {
                    let rx = self.number()?;
                    let ry = self.number()?;
                    let rot = self.number()?;
                    let large = self.flag()?;
                    let sweep = self.flag()?;
                    let p = self.pt(rel, cur)?;
                    arc_to_cubics(cur, rx, ry, rot, large, sweep, p, &mut s.segs);
                    cur = p;
                    last_ctrl = None;
                }
                _ => return Err(format!("unsupported command '{}'", up as char)),
            }
        }
        if let Some(s) = sp.take()
            && !s.segs.is_empty()
        {
            out.subpaths.push(s);
        }
        Ok(out)
    }
}

/// Convert an SVG elliptical arc to cubic segments (SVG spec F.6).
#[allow(clippy::too_many_arguments)]
pub(crate) fn arc_to_cubics(
    p0: Point,
    rx: f64,
    ry: f64,
    rot_deg: f64,
    large: bool,
    sweep: bool,
    p1: Point,
    out: &mut Vec<PathCmd>,
) {
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx < EPS || ry < EPS || p0.dist(p1) < EPS {
        out.push(PathCmd::L { p: p1 });
        return;
    }
    let phi = rot_deg.to_radians();
    let (sphi, cphi) = phi.sin_cos();
    let dx = (p0.x - p1.x) / 2.0;
    let dy = (p0.y - p1.y) / 2.0;
    let x1p = cphi * dx + sphi * dy;
    let y1p = -sphi * dx + cphi * dy;
    let lambda = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry);
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
    }
    let num = rx * rx * ry * ry - rx * rx * y1p * y1p - ry * ry * x1p * x1p;
    let den = rx * rx * y1p * y1p + ry * ry * x1p * x1p;
    let mut coef = if den.abs() < EPS {
        0.0
    } else {
        (num / den).max(0.0).sqrt()
    };
    if large == sweep {
        coef = -coef;
    }
    let cxp = coef * (rx * y1p / ry);
    let cyp = coef * (-ry * x1p / rx);
    let cx = cphi * cxp - sphi * cyp + (p0.x + p1.x) / 2.0;
    let cy = sphi * cxp + cphi * cyp + (p0.y + p1.y) / 2.0;
    let ang = |ux: f64, uy: f64, vx: f64, vy: f64| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let theta1 = ang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dtheta = ang(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && dtheta > 0.0 {
        dtheta -= std::f64::consts::TAU;
    } else if sweep && dtheta < 0.0 {
        dtheta += std::f64::consts::TAU;
    }
    let n = (dtheta.abs() / (std::f64::consts::FRAC_PI_2))
        .ceil()
        .max(1.0) as usize;
    let delta = dtheta / n as f64;
    let k = 4.0 / 3.0 * (delta / 4.0).tan();
    let map = |x: f64, y: f64| {
        Point::new(
            cphi * rx * x - sphi * ry * y + cx,
            sphi * rx * x + cphi * ry * y + cy,
        )
    };
    let mut t = theta1;
    for i in 0..n {
        let (s1, c1) = t.sin_cos();
        let (s2, c2) = (t + delta).sin_cos();
        let cp1 = map(c1 - k * s1, s1 + k * c1);
        let cp2 = map(c2 + k * s2, s2 - k * c2);
        let end = if i == n - 1 { p1 } else { map(c2, s2) };
        out.push(PathCmd::C {
            c1: cp1,
            c2: cp2,
            p: end,
        });
        t += delta;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic_and_roundtrip() {
        let p = PathData::from_svg("M0 0 L10 0 L10 10 Z m 20 0 h 5 v 5 h -5 z").unwrap();
        assert_eq!(p.subpaths.len(), 2);
        assert!(p.subpaths[0].closed);
        assert_eq!(p.subpaths[1].start, Point::new(20.0, 0.0));
        assert_eq!(p.subpaths[1].segs[1].end(), Point::new(25.0, 5.0));
        let again = PathData::from_svg(&p.to_svg()).unwrap();
        assert_eq!(again, p);
    }

    #[test]
    fn parse_implicit_lineto_and_compact_numbers() {
        let p = PathData::from_svg("M1,2 3,4-6.5.5").unwrap();
        let sp = &p.subpaths[0];
        assert_eq!(sp.segs.len(), 2);
        assert_eq!(sp.segs[1].end(), Point::new(-6.5, 0.5));
    }

    #[test]
    fn arc_circle_is_round() {
        let p = PathData::from_svg("M10 0 A10 10 0 1 1 -10 0 A10 10 0 1 1 10 0Z").unwrap();
        for pt in p.flatten(0.01).concat() {
            assert!((pt.len() - 10.0).abs() < 0.05, "{:?}", pt);
        }
    }

    #[test]
    fn ellipse_bounds() {
        let b = PathData::ellipse(5.0, 5.0, 4.0, 2.0).bounds();
        assert!((b.width() - 8.0).abs() < 0.01);
        assert!((b.height() - 4.0).abs() < 0.01);
    }

    #[test]
    fn reversed_keeps_geometry() {
        let p = PathData::from_svg("M0 0 C 0 10 10 10 10 0 L 20 0").unwrap();
        let r = p.subpaths[0].reversed();
        assert_eq!(r.start, Point::new(20.0, 0.0));
        assert_eq!(r.end(), Point::new(0.0, 0.0));
        let a = p.subpaths[0].flatten(0.01);
        let mut b = r.flatten(0.01);
        b.reverse();
        assert!(
            (super::super::polyline_length(&a) - super::super::polyline_length(&b)).abs() < 1e-6
        );
    }
}
