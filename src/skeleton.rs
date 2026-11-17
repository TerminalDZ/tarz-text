//! Stroke decomposition of letter shapes: polygon → raster →
//! exact Euclidean distance transform → Zhang–Suen skeleton (medial axis)
//! → pruned skeleton graph → one satin column per skeleton branch, with
//! rails found by casting rays perpendicular to the branch onto both
//! stroke edges. Compact parts (dots, harakat) become a single short satin
//! along their principal axis. Parts that are too wide for satin, or that
//! the columns would not cover well, stay fill regions.
//!
//! All computations are in the polygon's own units; `cell` is the raster
//! resolution and `max_width` the widest allowed satin, both in those units.

use crate::geom::{MultiPolygon, Point, Polygon, simplify_rdp};

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Piece {
    /// Satin column: rails sampled pairwise (`a[i]` faces `b[i]`).
    Satin { a: Vec<Point>, b: Vec<Point> },
    /// Fill region (one polygon with holes).
    Fill(Polygon),
}

struct Grid {
    w: usize,
    h: usize,
    ox: f64,
    oy: f64,
    cell: f64,
}

impl Grid {
    fn center(&self, x: usize, y: usize) -> Point {
        Point::new(
            self.ox + (x as f64 + 0.5) * self.cell,
            self.oy + (y as f64 + 0.5) * self.cell,
        )
    }
    fn cell_of(&self, p: Point) -> Option<(usize, usize)> {
        let x = ((p.x - self.ox) / self.cell).floor();
        let y = ((p.y - self.oy) / self.cell).floor();
        if x < 0.0 || y < 0.0 || x >= self.w as f64 || y >= self.h as f64 {
            return None;
        }
        Some((x as usize, y as usize))
    }
}

/// Scanline rasterisation (even-odd over all rings) at cell centres.
fn rasterize(poly: &Polygon, g: &Grid) -> Vec<bool> {
    let mut m = vec![false; g.w * g.h];
    let mut xs: Vec<f64> = Vec::new();
    for y in 0..g.h {
        let py = g.oy + (y as f64 + 0.5) * g.cell;
        xs.clear();
        for r in poly.rings() {
            let n = r.len();
            for i in 0..n {
                let (a, b) = (r[i], r[(i + 1) % n]);
                if (a.y > py) != (b.y > py) {
                    xs.push(a.x + (py - a.y) * (b.x - a.x) / (b.y - a.y));
                }
            }
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        for pair in xs.chunks(2) {
            if pair.len() < 2 {
                break;
            }
            let x0 = ((pair[0] - g.ox) / g.cell - 0.5).ceil().max(0.0) as usize;
            let x1 = ((pair[1] - g.ox) / g.cell - 0.5).floor();
            if x1 < 0.0 {
                continue;
            }
            let x1 = (x1 as usize).min(g.w - 1);
            for x in x0..=x1 {
                m[y * g.w + x] = true;
            }
        }
    }
    m
}

/// 1-D squared distance transform (Felzenszwalb & Huttenlocher).
fn edt_1d(f: &[f64], d: &mut [f64], v: &mut [usize], z: &mut [f64]) {
    let n = f.len();
    if n == 0 {
        return;
    }
    let mut k = 0usize;
    v[0] = 0;
    z[0] = f64::NEG_INFINITY;
    z[1] = f64::INFINITY;
    for q in 1..n {
        let mut s;
        loop {
            let p = v[k];
            s = ((f[q] + (q * q) as f64) - (f[p] + (p * p) as f64))
                / (2.0 * q as f64 - 2.0 * p as f64);
            if s <= z[k] {
                // z[0] is -inf, so k never underflows
                k -= 1;
                continue;
            }
            break;
        }
        k += 1;
        v[k] = q;
        z[k] = s;
        z[k + 1] = f64::INFINITY;
    }
    k = 0;
    for (q, dq) in d.iter_mut().enumerate().take(n) {
        while z[k + 1] < q as f64 {
            k += 1;
        }
        let p = v[k];
        *dq = (q as f64 - p as f64).powi(2) + f[p];
    }
}

/// Euclidean distance (in cells) from each inside cell to the nearest
/// outside cell centre.
fn distance_transform(m: &[bool], w: usize, h: usize) -> Vec<f64> {
    const INF: f64 = 1e20;
    let mut g: Vec<f64> = m.iter().map(|&i| if i { INF } else { 0.0 }).collect();
    let n = w.max(h);
    let (mut f, mut d) = (vec![0.0; n], vec![0.0; n]);
    let (mut v, mut z) = (vec![0usize; n], vec![0.0; n + 1]);
    for x in 0..w {
        for y in 0..h {
            f[y] = g[y * w + x];
        }
        edt_1d(&f[..h], &mut d[..h], &mut v[..h], &mut z[..h + 1]);
        for y in 0..h {
            g[y * w + x] = d[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&g[y * w..y * w + w]);
        edt_1d(&f[..w], &mut d[..w], &mut v[..w], &mut z[..w + 1]);
        g[y * w..y * w + w].copy_from_slice(&d[..w]);
    }
    g.iter_mut().for_each(|x| *x = x.sqrt());
    g
}

const NB: [(i32, i32); 8] = [
    (0, -1),
    (1, -1),
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
];

fn at(m: &[bool], w: usize, h: usize, x: i32, y: i32) -> bool {
    x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && m[y as usize * w + x as usize]
}

/// Zhang–Suen thinning followed by removal of redundant staircase pixels so
/// the skeleton is one pixel thin in 8-connectivity.
fn thin(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut m = mask.to_vec();
    let mut del = Vec::new();
    loop {
        let mut changed = false;
        for step in 0..2 {
            del.clear();
            for y in 0..h as i32 {
                for x in 0..w as i32 {
                    if !m[y as usize * w + x as usize] {
                        continue;
                    }
                    let p: Vec<bool> = NB
                        .iter()
                        .map(|&(dx, dy)| at(&m, w, h, x + dx, y + dy))
                        .collect();
                    let b = p.iter().filter(|&&v| v).count();
                    if !(2..=6).contains(&b) {
                        continue;
                    }
                    let a = (0..8).filter(|&i| !p[i] && p[(i + 1) % 8]).count();
                    if a != 1 {
                        continue;
                    }
                    // P2=N(0) P4=E(2) P6=S(4) P8=W(6)
                    let (p2, p4, p6, p8) = (p[0], p[2], p[4], p[6]);
                    let ok = if step == 0 {
                        !(p2 && p4 && p6) && !(p4 && p6 && p8)
                    } else {
                        !(p2 && p4 && p8) && !(p2 && p6 && p8)
                    };
                    if ok {
                        del.push(y as usize * w + x as usize);
                    }
                }
            }
            if !del.is_empty() {
                changed = true;
                for &i in &del {
                    m[i] = false;
                }
            }
        }
        if !changed {
            break;
        }
    }
    // staircase removal: an L-corner pixel whose neighbours stay connected
    loop {
        let mut changed = false;
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                if !m[y as usize * w + x as usize] {
                    continue;
                }
                let n = at(&m, w, h, x, y - 1);
                let s = at(&m, w, h, x, y + 1);
                let e = at(&m, w, h, x + 1, y);
                let wv = at(&m, w, h, x - 1, y);
                if !((n || s) && (e || wv)) {
                    continue;
                }
                let nbrs: Vec<(i32, i32)> = NB
                    .iter()
                    .map(|&(dx, dy)| (x + dx, y + dy))
                    .filter(|&(a, b)| at(&m, w, h, a, b))
                    .collect();
                if nbrs.len() < 2 || nbrs.len() > 4 {
                    continue;
                }
                // neighbours mutually 8-connected without p?
                let mut seen = vec![false; nbrs.len()];
                seen[0] = true;
                let mut stack = vec![0];
                while let Some(i) = stack.pop() {
                    for j in 0..nbrs.len() {
                        if !seen[j]
                            && (nbrs[i].0 - nbrs[j].0).abs() <= 1
                            && (nbrs[i].1 - nbrs[j].1).abs() <= 1
                        {
                            seen[j] = true;
                            stack.push(j);
                        }
                    }
                }
                if seen.iter().all(|&s| s) {
                    m[y as usize * w + x as usize] = false;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    m
}

fn neighbours(m: &[bool], w: usize, h: usize, i: usize) -> Vec<usize> {
    let (x, y) = ((i % w) as i32, (i / w) as i32);
    NB.iter()
        .map(|&(dx, dy)| (x + dx, y + dy))
        .filter(|&(a, b)| at(m, w, h, a, b))
        .map(|(a, b)| b as usize * w + a as usize)
        .collect()
}

/// Skeleton branches as pixel chains. Nodes are endpoints and junction
/// clusters; pure cycles are split in two.
fn branches(sk: &[bool], w: usize, h: usize) -> Vec<Vec<usize>> {
    let deg: Vec<usize> = (0..sk.len())
        .map(|i| {
            if sk[i] {
                neighbours(sk, w, h, i).len()
            } else {
                0
            }
        })
        .collect();
    let is_node = |i: usize| sk[i] && deg[i] != 2;
    let mut used = vec![false; sk.len()];
    let mut out: Vec<Vec<usize>> = Vec::new();
    let mut pair_seen = std::collections::HashSet::new();
    for s in 0..sk.len() {
        if !is_node(s) {
            continue;
        }
        for n0 in neighbours(sk, w, h, s) {
            if is_node(n0) {
                // direct node–node link: keep only links between different
                // junction clusters (skip adjacent junction pixels)
                if deg[s] == 1 || deg[n0] == 1 {
                    let key = (s.min(n0), s.max(n0));
                    if pair_seen.insert(key) {
                        out.push(vec![s, n0]);
                    }
                }
                continue;
            }
            if used[n0] {
                continue;
            }
            let mut chain = vec![s, n0];
            used[n0] = true;
            let mut prev = s;
            let mut cur = n0;
            loop {
                let next = neighbours(sk, w, h, cur)
                    .into_iter()
                    .find(|&q| q != prev && !chain[chain.len().saturating_sub(3)..].contains(&q));
                let Some(q) = next else { break };
                chain.push(q);
                if is_node(q) || used[q] {
                    break;
                }
                used[q] = true;
                prev = cur;
                cur = q;
            }
            out.push(chain);
        }
    }
    // isolated cycles
    for s in 0..sk.len() {
        if !sk[s] || used[s] || is_node(s) {
            continue;
        }
        let mut chain = vec![s];
        used[s] = true;
        let mut prev = usize::MAX;
        let mut cur = s;
        loop {
            let next = neighbours(sk, w, h, cur)
                .into_iter()
                .find(|&q| q != prev && !used[q]);
            let Some(q) = next else { break };
            used[q] = true;
            chain.push(q);
            prev = cur;
            cur = q;
        }
        if chain.len() >= 4 {
            let mid = chain.len() / 2;
            let mut first = chain[..=mid].to_vec();
            let mut second = chain[mid..].to_vec();
            second.push(chain[0]);
            if first.len() < 2 {
                first.push(chain[0]);
            }
            out.push(first);
            out.push(second);
        }
    }
    out
}

/// Remove short spurs (endpoint branches shorter than `k` × the stroke
/// radius at their junction), repeatedly.
fn prune(sk: &mut [bool], dt: &[f64], w: usize, h: usize) {
    for _ in 0..6 {
        let deg = |sk: &[bool], i: usize| neighbours(sk, w, h, i).len();
        let mut removed = false;
        let brs = branches(sk, w, h);
        if brs.len() <= 1 {
            return;
        }
        for b in brs {
            let (s, e) = (b[0], *b.last().unwrap());
            let (ds, de) = (deg(sk, s), deg(sk, e));
            let (tip, junction) = if ds == 1 && de >= 3 {
                (true, e)
            } else if de == 1 && ds >= 3 {
                (true, s)
            } else {
                (false, s)
            };
            if !tip {
                continue;
            }
            let len = b.len() as f64;
            if len < 1.6 * dt[junction] + 2.0 {
                for &p in &b {
                    if p != junction {
                        sk[p] = false;
                    }
                }
                removed = true;
            }
        }
        if !removed {
            return;
        }
    }
}

/// Nearest boundary hit of the ray `o + t*d` (t > 0) against all rings.
fn ray_hit(poly: &Polygon, o: Point, d: Point, max_t: f64) -> Option<f64> {
    let mut best: Option<f64> = None;
    for r in poly.rings() {
        let n = r.len();
        for i in 0..n {
            let (a, b) = (r[i], r[(i + 1) % n]);
            let e = b - a;
            let den = d.cross(e);
            if den.abs() < 1e-12 {
                continue;
            }
            let ao = a - o;
            let t = ao.cross(e) / den;
            let u = ao.cross(d) / den;
            if t > 1e-9
                && t <= max_t
                && (-1e-9..=1.0 + 1e-9).contains(&u)
                && best.is_none_or(|bt| t < bt)
            {
                best = Some(t);
            }
        }
    }
    best
}

fn smooth(pts: &[Point], passes: usize) -> Vec<Point> {
    let mut p = pts.to_vec();
    for _ in 0..passes {
        if p.len() < 3 {
            break;
        }
        let mut q = p.clone();
        for i in 1..p.len() - 1 {
            q[i] = (p[i - 1] + p[i] * 2.0 + p[i + 1]) / 4.0;
        }
        p = q;
    }
    p
}

fn resample(pts: &[Point], step: f64) -> Vec<Point> {
    let total = crate::geom::polyline_length(pts);
    if total < 1e-9 {
        return vec![pts[0]];
    }
    let n = (total / step).ceil().max(1.0) as usize;
    (0..=n)
        .map(|i| crate::geom::point_at_length(pts, total * i as f64 / n as f64))
        .collect()
}

#[derive(Clone, Copy, PartialEq)]
enum End {
    /// Free stroke end: extend to the outline.
    Tip,
    /// Cut at a sharp turn: extend by the stroke radius into the corner.
    Corner,
    /// Skeleton junction: stop (neighbouring columns overlap there).
    Junction,
}

/// Split a branch at sharp turns (> ~55° over a window of the stroke
/// radius) so each column keeps a sane stitch direction.
fn split_corners(chain: &[usize], g: &Grid, dt: &[f64]) -> Vec<Vec<usize>> {
    let pts: Vec<Point> = chain.iter().map(|&i| g.center(i % g.w, i / g.w)).collect();
    let n = pts.len();
    let mut cuts = Vec::new();
    let mut i = 0;
    while i < n {
        let k = (dt[chain[i]].round() as usize).clamp(3, 40);
        if i < k || i + k >= n {
            i += 1;
            continue;
        }
        let d1 = (pts[i] - pts[i - k]).normalized();
        let d2 = (pts[i + k] - pts[i]).normalized();
        let ang = d1.dot(d2).clamp(-1.0, 1.0).acos();
        if ang > 0.96 {
            // local maximum of the turn within the window
            let mut best = (ang, i);
            for j in i + 1..(i + k).min(n - k) {
                let a = (pts[j] - pts[j - k])
                    .normalized()
                    .dot((pts[j + k] - pts[j]).normalized())
                    .clamp(-1.0, 1.0)
                    .acos();
                if a > best.0 {
                    best = (a, j);
                }
            }
            cuts.push(best.1);
            i = best.1 + k;
        } else {
            i += 1;
        }
    }
    if cuts.is_empty() {
        return vec![chain.to_vec()];
    }
    let mut out = Vec::new();
    let mut s = 0;
    for c in cuts {
        out.push(chain[s..=c].to_vec());
        s = c;
    }
    out.push(chain[s..].to_vec());
    out
}

/// Satin rails along a skeleton branch.
fn branch_rails(
    poly: &Polygon,
    g: &Grid,
    dt: &[f64],
    chain: &[usize],
    extend_start: End,
    extend_end: End,
) -> Option<(Vec<Point>, Vec<Point>)> {
    let pts: Vec<Point> = chain.iter().map(|&i| g.center(i % g.w, i / g.w)).collect();
    let mut pts = smooth(&pts, 2);
    let rad_at = |p: Point| -> f64 {
        g.cell_of(p)
            .map(|(x, y)| dt[y * g.w + x] * g.cell)
            .unwrap_or(g.cell)
    };
    // extend tips to the boundary along the end tangents
    let extend = |pts: &mut Vec<Point>, at_start: bool| {
        if pts.len() < 2 {
            return;
        }
        let (p, q) = if at_start {
            (pts[0], pts[1.min(pts.len() - 1)])
        } else {
            (pts[pts.len() - 1], pts[pts.len() - 2])
        };
        let k = pts.len().min(4) - 1;
        let far = if at_start {
            pts[k]
        } else {
            pts[pts.len() - 1 - k]
        };
        let d = (p - far).normalized();
        let d = if d.len() < 0.5 {
            (p - q).normalized()
        } else {
            d
        };
        if d.len() < 0.5 {
            return;
        }
        let reach = rad_at(p) * 1.5 + 2.0 * g.cell;
        if let Some(t) = ray_hit(poly, p, d, reach) {
            let tip = p + d * (t * 0.98);
            if at_start {
                pts.insert(0, tip);
            } else {
                pts.push(tip);
            }
        }
    };
    let push_on = |pts: &mut Vec<Point>, at_start: bool| {
        if pts.len() < 2 {
            return;
        }
        let k = pts.len().min(4) - 1;
        let (p, far) = if at_start {
            (pts[0], pts[k])
        } else {
            (pts[pts.len() - 1], pts[pts.len() - 1 - k])
        };
        let d = (p - far).normalized();
        let tip = p + d * rad_at(p);
        if at_start {
            pts.insert(0, tip);
        } else {
            pts.push(tip);
        }
    };
    match extend_start {
        End::Tip => extend(&mut pts, true),
        End::Corner => push_on(&mut pts, true),
        End::Junction => {}
    }
    match extend_end {
        End::Tip => extend(&mut pts, false),
        End::Corner => push_on(&mut pts, false),
        End::Junction => {}
    }
    let len = crate::geom::polyline_length(&pts);
    if len < g.cell * 0.5 {
        return None;
    }
    let step = (g.cell * 2.0).max(len / 400.0);
    let s = resample(&pts, step);
    if s.len() < 2 {
        return None;
    }
    // typical stroke radius of this branch: rays near junctions are capped
    // to it so columns keep their width instead of fanning out
    let mut radii: Vec<f64> = s.iter().map(|p| rad_at(*p)).collect();
    radii.sort_by(|x, y| x.total_cmp(y));
    let r_med = radii[radii.len() / 2].max(g.cell);
    let mut a = Vec::with_capacity(s.len());
    let mut b = Vec::with_capacity(s.len());
    for i in 0..s.len() {
        let lo = i.saturating_sub(2);
        let hi = (i + 2).min(s.len() - 1);
        let t = (s[hi] - s[lo]).normalized();
        if t.len() < 0.5 {
            continue;
        }
        let n = t.perp();
        let r = rad_at(s[i]).max(g.cell * 0.5).min(r_med * 1.25);
        let lim = (r * 1.6 + g.cell * 2.0).min(r_med * 1.6 + g.cell * 2.0);
        let ta = ray_hit(poly, s[i], n, lim).unwrap_or(r.min(lim));
        let tb = ray_hit(poly, s[i], -n, lim).unwrap_or(r.min(lim));
        // keep the column centred on the skeleton when one side runs into
        // a junction (much longer than the other)
        let (ta, tb) = if ta > 1.8 * tb + g.cell {
            (tb.max(r), tb)
        } else if tb > 1.8 * ta + g.cell {
            (ta, ta.max(r))
        } else {
            (ta, tb)
        };
        a.push(s[i] + n * ta);
        b.push(s[i] - n * tb);
    }
    if a.len() < 2 {
        return None;
    }
    Some((a, b))
}

/// Dot / short mark: one satin along the principal axis, rails = the two
/// halves of the outline between the extreme points of that axis.
fn dot_satin(poly: &Polygon) -> Option<Piece> {
    let r = &poly.outer;
    if r.len() < 3 {
        return None;
    }
    let n = r.len() as f64;
    let c = r.iter().fold(Point::default(), |s, p| s + *p) / n;
    let (mut sxx, mut syy, mut sxy) = (0.0, 0.0, 0.0);
    for p in r {
        let d = *p - c;
        sxx += d.x * d.x;
        syy += d.y * d.y;
        sxy += d.x * d.y;
    }
    let ang = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    let axis = Point::new(ang.cos(), ang.sin());
    let proj: Vec<f64> = r.iter().map(|p| (*p - c).dot(axis)).collect();
    let imin = (0..r.len()).min_by(|&a, &b| proj[a].total_cmp(&proj[b]))?;
    let imax = (0..r.len()).max_by(|&a, &b| proj[a].total_cmp(&proj[b]))?;
    if imin == imax {
        return None;
    }
    let walk = |from: usize, to: usize, fwd: bool| -> Vec<Point> {
        let mut v = vec![r[from]];
        let mut i = from;
        while i != to {
            i = if fwd {
                (i + 1) % r.len()
            } else {
                (i + r.len() - 1) % r.len()
            };
            v.push(r[i]);
        }
        v
    };
    let ra = walk(imin, imax, true);
    let rb = walk(imin, imax, false);
    // sample both rails at the same axis positions
    let span = proj[imax] - proj[imin];
    let k = 8usize;
    let at_pos = |rail: &[Point], t: f64| -> Point {
        let target = proj[imin] + span * t;
        for w in rail.windows(2) {
            let (pa, pb) = ((w[0] - c).dot(axis), (w[1] - c).dot(axis));
            if (pa - target) * (pb - target) <= 0.0 && (pb - pa).abs() > 1e-12 {
                return w[0].lerp(w[1], (target - pa) / (pb - pa));
            }
        }
        rail[rail.len() - 1]
    };
    let a: Vec<Point> = (0..=k).map(|i| at_pos(&ra, i as f64 / k as f64)).collect();
    let b: Vec<Point> = (0..=k).map(|i| at_pos(&rb, i as f64 / k as f64)).collect();
    Some(Piece::Satin { a, b })
}

/// Satin column outline as a region (for coverage checks).
pub(crate) fn satin_region(a: &[Point], b: &[Point]) -> MultiPolygon {
    let mut ring: Vec<Point> = a.to_vec();
    ring.extend(b.iter().rev().copied());
    let quads: Vec<Vec<Point>> = (0..a.len().min(b.len()).saturating_sub(1))
        .map(|i| vec![a[i], a[i + 1], b[i + 1], b[i]])
        .collect();
    let parts: Vec<MultiPolygon> = quads
        .iter()
        .map(|q| MultiPolygon::from_rings_evenodd(std::slice::from_ref(q)))
        .filter(|m| !m.is_empty())
        .collect();
    if parts.is_empty() {
        return MultiPolygon::from_rings_evenodd(&[ring]);
    }
    MultiPolygon::union_all(&parts)
}

/// Decompose one polygon (with holes) into satin columns and fills.
/// `small_area`: parts below this area become a dot satin.
pub(crate) fn decompose(poly: &Polygon, cell: f64, max_width: f64, small_area: f64) -> Vec<Piece> {
    let area = poly.area();
    if area <= 0.0 || poly.outer.len() < 3 {
        return Vec::new();
    }
    let b = poly.bounds();
    let compact = {
        let r = b.width().min(b.height());
        let l = b.width().max(b.height());
        l < 2.2 * r.max(1e-9)
    };
    if area < small_area
        && compact
        && poly.holes.is_empty()
        && let Some(p) = dot_satin(poly)
    {
        return vec![p];
    }
    let pad = 2.0 * cell;
    let g = Grid {
        w: ((b.width() + 2.0 * pad) / cell).ceil() as usize + 1,
        h: ((b.height() + 2.0 * pad) / cell).ceil() as usize + 1,
        ox: b.min_x - pad,
        oy: b.min_y - pad,
        cell,
    };
    if g.w * g.h > 4_000_000 {
        return vec![Piece::Fill(poly.clone())];
    }
    let mask = rasterize(poly, &g);
    let dt = distance_transform(&mask, g.w, g.h);
    let max_r = dt.iter().cloned().fold(0.0, f64::max) * cell;
    if 2.0 * max_r > max_width {
        return vec![Piece::Fill(poly.clone())];
    }
    let mut sk = thin(&mask, g.w, g.h);
    prune(&mut sk, &dt, g.w, g.h);
    let brs = branches(&sk, g.w, g.h);
    if brs.is_empty() {
        return dot_satin(poly)
            .map(|p| vec![p])
            .unwrap_or_else(|| vec![Piece::Fill(poly.clone())]);
    }
    let deg = |i: usize| neighbours(&sk, g.w, g.h, i).len();
    let mut pieces = Vec::new();
    let mut regions = Vec::new();
    for br in &brs {
        let end_kind = |i: usize| if deg(i) <= 1 { End::Tip } else { End::Junction };
        let parts = split_corners(br, &g, &dt);
        let np = parts.len();
        for (k, part) in parts.iter().enumerate() {
            let s_end = if k == 0 { end_kind(br[0]) } else { End::Corner };
            let e_end = if k + 1 == np {
                end_kind(*br.last().unwrap())
            } else {
                End::Corner
            };
            if let Some((a, b)) = branch_rails(poly, &g, &dt, part, s_end, e_end) {
                let tol = cell * 0.35;
                let (a, b) = simplify_pair(&a, &b, tol);
                regions.push(satin_region(&a, &b));
                pieces.push(Piece::Satin { a, b });
            }
        }
    }
    if pieces.is_empty() {
        return vec![Piece::Fill(poly.clone())];
    }
    // drop columns that other columns already cover (tiny junction/apex
    // stubs), smallest first
    let mut order: Vec<usize> = (0..pieces.len()).collect();
    order.sort_by(|&x, &y| regions[x].area().total_cmp(&regions[y].area()));
    let mut keep = vec![true; pieces.len()];
    for &i in &order {
        let others: Vec<MultiPolygon> = (0..pieces.len())
            .filter(|&j| j != i && keep[j])
            .map(|j| regions[j].clone())
            .collect();
        if others.is_empty() {
            continue;
        }
        let own = regions[i].area();
        let rest = regions[i]
            .difference(&MultiPolygon::union_all(&others))
            .area();
        if rest < 0.1 * own {
            keep[i] = false;
        }
    }
    let mut k = keep.iter();
    pieces.retain(|_| *k.next().unwrap());
    let mut k = keep.iter();
    regions.retain(|_| *k.next().unwrap());
    // coverage check
    let target = MultiPolygon(vec![poly.clone()]);
    let covered = MultiPolygon::union_all(&regions)
        .intersection(&target)
        .area();
    if covered < 0.8 * area {
        return vec![Piece::Fill(poly.clone())];
    }
    pieces
}

/// Simplify paired rails while keeping them index-aligned: keep a sample
/// when either rail needs it.
fn simplify_pair(a: &[Point], b: &[Point], tol: f64) -> (Vec<Point>, Vec<Point>) {
    let keep_a = simplify_rdp(a, tol);
    let keep_b = simplify_rdp(b, tol);
    let mut ka = 0;
    let mut kb = 0;
    let mut ra = Vec::new();
    let mut rb = Vec::new();
    for i in 0..a.len() {
        let in_a = ka < keep_a.len() && keep_a[ka] == a[i];
        let in_b = kb < keep_b.len() && keep_b[kb] == b[i];
        if in_a {
            ka += 1;
        }
        if in_b {
            kb += 1;
        }
        if in_a || in_b || i == 0 || i == a.len() - 1 {
            ra.push(a[i]);
            rb.push(b[i]);
        }
    }
    (ra, rb)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Polygon {
        Polygon::new(
            vec![
                Point::new(x, y),
                Point::new(x + w, y),
                Point::new(x + w, y + h),
                Point::new(x, y + h),
            ],
            vec![],
        )
    }

    fn width_stats(p: &Piece) -> (f64, f64) {
        match p {
            Piece::Satin { a, b } => {
                let ws: Vec<f64> = a.iter().zip(b).map(|(p, q)| p.dist(*q)).collect();
                let mid = &ws[ws.len() / 4..ws.len() * 3 / 4];
                (
                    mid.iter().cloned().fold(f64::INFINITY, f64::min),
                    mid.iter().cloned().fold(0.0, f64::max),
                )
            }
            Piece::Fill(_) => (0.0, 0.0),
        }
    }

    #[test]
    fn edt_is_exact() {
        let (w, h) = (30, 12);
        let mut m = vec![false; w * h];
        for y in 1..11 {
            for x in 1..29 {
                m[y * w + x] = true;
            }
        }
        let d = distance_transform(&m, w, h);
        assert_eq!(d[5 * w + 15], 5.0);
        assert_eq!(d[w + 15], 1.0);
        assert_eq!(d[6 * w + 15], 5.0);
        assert!((d[3 * w + 3] - 3.0).abs() < 1e-9);
    }

    #[test]
    fn bar_is_one_satin_following_both_edges() {
        let p = rect(0.0, 0.0, 100.0, 10.0);
        let pieces = decompose(&p, 0.5, 30.0, 1.0);
        assert_eq!(pieces.len(), 1, "{pieces:?}");
        if let Piece::Satin { a, b } = &pieces[0] {
            for (p, q) in a.iter().zip(b) {
                if p.x > 10.0 && p.x < 90.0 {
                    assert!((p.dist(*q) - 10.0).abs() < 0.6, "{p:?} {q:?}");
                }
            }
        }
        if let Piece::Satin { a, b } = &pieces[0] {
            // spans (almost) the whole bar length
            let xs: Vec<f64> = a.iter().chain(b).map(|p| p.x).collect();
            let min = xs.iter().cloned().fold(f64::INFINITY, f64::min);
            let max = xs.iter().cloned().fold(0.0, f64::max);
            assert!(min < 3.0 && max > 97.0, "{min} {max}");
            // rails on the two edges
            assert!(
                a.iter()
                    .filter(|p| p.x > 10.0 && p.x < 90.0)
                    .all(|p| (p.y - 10.0).abs() < 0.6 || p.y.abs() < 0.6)
            );
        }
    }

    #[test]
    fn l_shape_gives_two_columns() {
        let outer = vec![
            Point::new(0.0, 0.0),
            Point::new(60.0, 0.0),
            Point::new(60.0, 8.0),
            Point::new(8.0, 8.0),
            Point::new(8.0, 60.0),
            Point::new(0.0, 60.0),
        ];
        let pieces = decompose(&Polygon::new(outer, vec![]), 0.5, 30.0, 1.0);
        let satins = pieces
            .iter()
            .filter(|p| matches!(p, Piece::Satin { .. }))
            .count();
        assert!((2..=3).contains(&satins), "{pieces:?}");
        for p in &pieces {
            if let Piece::Satin { a, b } = p {
                for (p, q) in a.iter().zip(b) {
                    assert!(p.dist(*q) < 12.0, "{p:?} {q:?}");
                }
            }
        }
    }

    #[test]
    fn ring_with_hole_is_covered_by_satins() {
        let mut outer = Vec::new();
        let mut hole = Vec::new();
        for i in 0..64 {
            let t = i as f64 / 64.0 * std::f64::consts::TAU;
            outer.push(Point::new(30.0 * t.cos(), 30.0 * t.sin()));
            hole.push(Point::new(22.0 * t.cos(), -22.0 * t.sin()));
        }
        let p = Polygon::new(outer, vec![hole]);
        let pieces = decompose(&p, 0.5, 30.0, 1.0);
        assert!(
            pieces.iter().all(|p| matches!(p, Piece::Satin { .. })),
            "{pieces:?}"
        );
        assert!(pieces.len() >= 2);
        for pc in &pieces {
            let (lo, hi) = width_stats(pc);
            assert!(lo > 6.0 && hi < 10.0, "{lo} {hi}");
        }
    }

    #[test]
    fn wide_blob_is_fill_and_dot_is_small_satin() {
        let big = rect(0.0, 0.0, 50.0, 50.0);
        assert!(matches!(decompose(&big, 0.5, 20.0, 1.0)[0], Piece::Fill(_)));
        let dot = rect(0.0, 0.0, 1.0, 0.8);
        let p = decompose(&dot, 0.05, 20.0, 2.0);
        assert_eq!(p.len(), 1);
        assert!(matches!(p[0], Piece::Satin { .. }));
    }
}
