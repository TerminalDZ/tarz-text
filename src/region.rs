//! Glyph outlines → sewable regions: nonzero fill, bold, minimum column
//! width, small-part enlargement, Arabic join overlap, arc/path warping,
//! centring and merging.

use super::layout::{Layout, glyph_path};
use super::{
    FontRegistry, MAX_THICKEN_MM, MIN_COLUMN_MM, MIN_PART_AREA_MM2, TextOutlines, TextRegion,
};
use crate::Result;
use crate::design::{TextAlign, TextLayout, TextShape};
use crate::geom::{Affine, MultiPolygon, PathData, Point, Polygon, Rect, Ring};

/// Curve flattening tolerance (mm).
pub(crate) const FLAT_TOL: f64 = 0.01;
/// Maximum segment length before warping (mm).
const WARP_STEP: f64 = 0.25;

fn ring_area(r: &[Point]) -> f64 {
    let mut a = 0.0;
    for i in 0..r.len() {
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        a += p.x * q.y - q.x * p.y;
    }
    a * 0.5
}

/// Fill a glyph outline with the nonzero winding rule (TrueType/CFF
/// semantics). Static instances of variable fonts often keep overlapping
/// contours (counters formed by overlaps, self-intersecting loops); the
/// overlay engine computes the winding numbers with each contour's own
/// orientation.
pub(crate) fn nonzero_region(path: &PathData, tol: f64) -> MultiPolygon {
    use geo::BooleanOps;
    let polys: Vec<geo::Polygon<f64>> = path
        .subpaths
        .iter()
        .filter_map(|s| {
            let mut r = s.flatten(tol);
            if r.len() > 1 && r[0].dist(*r.last().unwrap()) < 1e-9 {
                r.pop();
            }
            if r.len() < 3 || ring_area(&r).abs() < 1e-9 {
                return None;
            }
            let mut c: Vec<geo::Coord<f64>> =
                r.iter().map(|p| geo::Coord { x: p.x, y: p.y }).collect();
            c.push(c[0]);
            Some(geo::Polygon::new(geo::LineString::new(c), vec![]))
        })
        .collect();
    if polys.is_empty() {
        return MultiPolygon::empty();
    }
    let subject = geo::MultiPolygon::new(polys);
    let out = subject.union_with_fill_rule(
        &geo::MultiPolygon::<f64>::new(vec![]),
        geo::algorithm::bool_ops::FillRule::NonZero,
    );
    let mut m = MultiPolygon::from_geo(&out);
    m.0.retain(|p| p.area() > 1e-6);
    m
}

/// Fraction of the area lost by a morphological opening of radius `r`
/// (parts thinner than `2r` disappear).
fn thin_loss(m: &MultiPolygon, r: f64) -> f64 {
    let a = m.area();
    if a <= 1e-9 {
        return 0.0;
    }
    let opened = m.offset(-r).offset(r);
    ((a - opened.area()) / a).max(0.0)
}

/// Outward offset needed so the thin strokes of `m` reach the minimum
/// column width. Tapered stroke ends and sharp corners always lose a bit of
/// area in the opening, hence the tolerance.
pub(crate) fn thicken_amount(m: &MultiPolygon) -> f64 {
    const TOLERATED_LOSS: f64 = 0.06;
    let r = MIN_COLUMN_MM / 2.0;
    let steps = [0.0, 0.05, 0.1, 0.15, 0.2, 0.25];
    for &t in &steps {
        let g = if t > 0.0 { m.offset(t) } else { m.clone() };
        if thin_loss(&g, r) < TOLERATED_LOSS {
            return t;
        }
    }
    MAX_THICKEN_MM
}

fn ring_centroid(r: &[Point]) -> Point {
    let a = ring_area(r);
    if a.abs() < 1e-12 {
        let n = r.len().max(1) as f64;
        return r.iter().fold(Point::default(), |s, p| s + *p) / n;
    }
    let (mut cx, mut cy) = (0.0, 0.0);
    for i in 0..r.len() {
        let p = r[i];
        let q = r[(i + 1) % r.len()];
        let k = p.x * q.y - q.x * p.y;
        cx += (p.x + q.x) * k;
        cy += (p.y + q.y) * k;
    }
    Point::new(cx / (6.0 * a), cy / (6.0 * a))
}

/// Scale polygons smaller than the minimum sewable area around their
/// centroid. Returns the number of enlarged parts.
fn enlarge_small(m: &mut MultiPolygon) -> usize {
    let mut n = 0;
    let mut changed = false;
    for p in &mut m.0 {
        let a = p.area();
        if a > 1e-6 && a < MIN_PART_AREA_MM2 {
            let k = (MIN_PART_AREA_MM2 / a).sqrt().min(3.0);
            let c = ring_centroid(&p.outer);
            let t = Affine::translate(c.x, c.y)
                .then_after(Affine::scale(k, k))
                .then_after(Affine::translate(-c.x, -c.y));
            *p = p.transform(&t);
            n += 1;
            changed = true;
        }
    }
    if changed && m.0.len() > 1 {
        let parts: Vec<MultiPolygon> = m.0.iter().map(|p| MultiPolygon(vec![p.clone()])).collect();
        *m = MultiPolygon::union_all(&parts);
    }
    n
}

/// Curvilinear baseline used by arc and path layouts.
enum Warp {
    Arc {
        r: f64,
        top: bool,
    },
    Path {
        pts: Vec<Point>,
        cum: Vec<f64>,
        tan: Vec<Point>,
    },
}

impl Warp {
    fn length(&self) -> f64 {
        match self {
            Warp::Arc { r, .. } => 2.0 * std::f64::consts::PI * r,
            Warp::Path { cum, .. } => *cum.last().unwrap_or(&0.0),
        }
    }
    /// Point and unit tangent at arc length `s`.
    fn frame(&self, s: f64) -> (Point, Point) {
        match self {
            Warp::Arc { r, top } => {
                let th = s / r;
                let (sn, cs) = th.sin_cos();
                if *top {
                    (Point::new(r * sn, -r * cs), Point::new(cs, sn))
                } else {
                    (Point::new(r * sn, r * cs), Point::new(cs, -sn))
                }
            }
            Warp::Path { pts, cum, tan } => {
                let n = pts.len();
                if s <= 0.0 {
                    let t = (pts[1] - pts[0]).normalized();
                    return (pts[0] + t * s, t);
                }
                let total = cum[n - 1];
                if s >= total {
                    let t = (pts[n - 1] - pts[n - 2]).normalized();
                    return (pts[n - 1] + t * (s - total), t);
                }
                let i = match cum.binary_search_by(|v| v.total_cmp(&s)) {
                    Ok(i) => i.min(n - 2),
                    Err(i) => i.saturating_sub(1).min(n - 2),
                };
                let seg = cum[i + 1] - cum[i];
                let u = if seg > 1e-12 { (s - cum[i]) / seg } else { 0.0 };
                let p = pts[i].lerp(pts[i + 1], u);
                let t = tan[i].lerp(tan[i + 1], u).normalized();
                (p, t)
            }
        }
    }
    /// Map a straight-layout point (x = arc length, y = offset below the
    /// baseline) onto the curved baseline.
    fn map(&self, p: Point) -> Point {
        let (q, t) = self.frame(p.x);
        let n = Point::new(t.y, -t.x);
        q - n * p.y
    }
    /// Rigid placement of a cluster centred at `cx` (rotated to the tangent).
    fn rigid(&self, cx: f64) -> Affine {
        let (q, t) = self.frame(cx);
        Affine {
            a: t.x,
            b: t.y,
            c: -t.y,
            d: t.x,
            e: q.x - t.x * cx,
            f: q.y - t.y * cx,
        }
    }
}

fn path_warp(path: &PathData) -> Option<Warp> {
    let sp = path.subpaths.iter().find(|s| !s.segs.is_empty())?;
    let mut pts = sp.flatten(0.02);
    if sp.closed && pts.first().map(|p| p.dist(sp.start)) > Some(1e-9) {
        pts.push(sp.start);
    }
    pts.dedup_by(|a, b| a.dist(*b) < 1e-9);
    if pts.len() < 2 {
        return None;
    }
    let mut cum = vec![0.0];
    for w in pts.windows(2) {
        cum.push(cum.last().unwrap() + w[0].dist(w[1]));
    }
    let n = pts.len();
    let seg_t: Vec<Point> = pts.windows(2).map(|w| (w[1] - w[0]).normalized()).collect();
    let mut tan = Vec::with_capacity(n);
    for i in 0..n {
        let t = if i == 0 {
            seg_t[0]
        } else if i == n - 1 {
            seg_t[n - 2]
        } else {
            (seg_t[i - 1] + seg_t[i]).normalized()
        };
        tan.push(if t.len() < 0.5 {
            seg_t[i.min(n - 2)]
        } else {
            t
        });
    }
    Some(Warp::Path { pts, cum, tan })
}

fn subdivide_ring(r: &[Point], step: f64) -> Ring {
    let mut closed = r.to_vec();
    if let Some(&f) = r.first() {
        closed.push(f);
    }
    let mut out = crate::geom::subdivide_polyline(&closed, step);
    out.pop();
    out
}

fn warp_region(m: &MultiPolygon, w: &Warp) -> MultiPolygon {
    MultiPolygon(
        m.0.iter()
            .map(|p| Polygon {
                outer: subdivide_ring(&p.outer, WARP_STEP)
                    .into_iter()
                    .map(|q| w.map(q))
                    .collect(),
                holes: p
                    .holes
                    .iter()
                    .map(|h| {
                        subdivide_ring(h, WARP_STEP)
                            .into_iter()
                            .map(|q| w.map(q))
                            .collect()
                    })
                    .collect(),
            })
            .collect(),
    )
}

struct Item {
    cluster: usize,
    is_mark: bool,
    region: MultiPolygon,
    /// Original glyph curves, valid while the region is only moved rigidly.
    curves: Option<PathData>,
}

/// How straight-layout coordinates map to the final ones (arc/path layout
/// and centring), per cluster.
pub(crate) struct Placement {
    pre: Affine,
    warp: Option<Warp>,
    /// Rigid placement per cluster (None: point-wise warp).
    rigid: Vec<Option<Affine>>,
    post: Affine,
}

impl Placement {
    /// Affine map of a cluster when it is moved rigidly (None when warped).
    pub fn affine(&self, cluster: usize) -> Option<Affine> {
        match &self.warp {
            None => Some(self.post),
            Some(_) => self.rigid[cluster].map(|m| self.post.then_after(m)),
        }
    }
    pub fn point(&self, cluster: usize, p: Point) -> Point {
        match (&self.warp, self.affine(cluster)) {
            (_, Some(m)) => m.apply(p),
            (Some(w), None) => self.post.apply(w.map(self.pre.apply(p))),
            (None, None) => p,
        }
    }
    pub fn is_warped(&self, cluster: usize) -> bool {
        self.affine(cluster).is_none()
    }
    pub fn region(&self, cluster: usize, m: &MultiPolygon) -> MultiPolygon {
        match (&self.warp, self.affine(cluster)) {
            (_, Some(a)) => m.transform(&a),
            (Some(w), None) => warp_region(&m.transform(&self.pre), w).transform(&self.post),
            (None, None) => m.clone(),
        }
    }
}

fn placement(shape: &TextShape, ink: &Rect, cluster_box: &[Rect], warp_on: &[bool]) -> Placement {
    let nc = cluster_box.len();
    let warp = match &shape.layout {
        TextLayout::Straight => None,
        TextLayout::Arc { radius_mm, inside } => {
            let r = if radius_mm.is_finite() {
                radius_mm.abs().max(1.0)
            } else {
                50.0
            };
            Some(Warp::Arc { r, top: !inside })
        }
        TextLayout::Path { path } => path_warp(path),
    };
    let (Some(w), false) = (&warp, ink.is_empty()) else {
        return Placement {
            pre: Affine::IDENTITY,
            warp: None,
            rigid: vec![None; nc],
            post: Affine::IDENTITY,
        };
    };
    let shift = match (w, shape.align) {
        (Warp::Arc { .. }, _) => -(ink.min_x + ink.max_x) / 2.0,
        (Warp::Path { .. }, TextAlign::Left) => -ink.min_x,
        (Warp::Path { .. }, TextAlign::Center) => w.length() / 2.0 - (ink.min_x + ink.max_x) / 2.0,
        (Warp::Path { .. }, TextAlign::Right) => w.length() - ink.max_x,
    };
    let pre = Affine::translate(shift, 0.0);
    let rigid = (0..nc)
        .map(|c| {
            if warp_on[c] || cluster_box[c].is_empty() {
                return None;
            }
            let b = cluster_box[c];
            Some(w.rigid((b.min_x + b.max_x) / 2.0 + shift).then_after(pre))
        })
        .collect();
    Placement {
        pre,
        warp,
        rigid,
        post: Affine::IDENTITY,
    }
}

fn mp_centroid(m: &MultiPolygon) -> Point {
    let (mut sx, mut sy, mut sa) = (0.0, 0.0, 0.0);
    for p in &m.0 {
        let a = p.area();
        let c = ring_centroid(&p.outer);
        sx += c.x * a;
        sy += c.y * a;
        sa += a;
    }
    if sa > 0.0 {
        Point::new(sx / sa, sy / sa)
    } else {
        m.bounds().center()
    }
}

/// Outlines plus what the stitch-object pass needs to follow them.
pub(crate) struct Built {
    pub out: TextOutlines,
    pub place: Placement,
    /// Join point (straight layout coordinates) between cluster `c` and the
    /// cluster it joins to.
    pub join_points: Vec<(usize, Point)>,
}

pub(crate) fn build_full(shape: &TextShape, fonts: &FontRegistry, lay: &Layout) -> Result<Built> {
    let nc = lay.clusters.len();
    let mut base_paths: Vec<PathData> = vec![PathData::new(); nc];
    let mut mark_paths: Vec<PathData> = vec![PathData::new(); nc];
    let mut base_parts: Vec<Vec<MultiPolygon>> = vec![Vec::new(); nc];
    let mut mark_parts: Vec<Vec<MultiPolygon>> = vec![Vec::new(); nc];
    let mut script_font = vec![false; nc];
    for g in &lay.glyphs {
        let p = glyph_path(fonts, g);
        if p.is_empty() {
            continue;
        }
        let region = nonzero_region(&p, FLAT_TOL);
        if fonts.fonts[g.font].style.contains("Script") {
            script_font[g.cluster] = true;
        }
        let (paths, parts) = if g.is_mark {
            (&mut mark_paths, &mut mark_parts)
        } else {
            (&mut base_paths, &mut base_parts)
        };
        paths[g.cluster].subpaths.extend(p.subpaths);
        parts[g.cluster].push(region);
    }
    let mut items: Vec<Item> = Vec::new();
    for c in 0..nc {
        for (is_mark, paths, parts) in [
            (false, &base_paths[c], &base_parts[c]),
            (true, &mark_paths[c], &mark_parts[c]),
        ] {
            if parts.is_empty() {
                continue;
            }
            let region = if parts.len() == 1 {
                parts[0].clone()
            } else {
                MultiPolygon::union_all(parts)
            };
            if region.is_empty() {
                continue;
            }
            // Curves are kept only when the even-odd reading of the curve
            // path equals the nonzero region (no overlapping contours).
            let eo = MultiPolygon::from_path(paths, FLAT_TOL).area();
            let ok = (eo - region.area()).abs() <= 1e-3 * region.area().max(1e-6);
            items.push(Item {
                cluster: c,
                is_mark,
                region,
                curves: ok.then(|| paths.clone()),
            });
        }
    }

    // 1. bold
    let bold = if shape.bold.is_finite() {
        shape.bold.clamp(-2.0, 5.0)
    } else {
        0.0
    };
    if bold.abs() > 1e-6 {
        for it in &mut items {
            it.region = it.region.offset(bold);
            it.curves = None;
        }
    }
    // 2. minimum column width (measured on the base letters)
    let mut thickened = 0.0;
    let bases: Vec<MultiPolygon> = items
        .iter()
        .filter(|i| !i.is_mark)
        .map(|i| i.region.clone())
        .collect();
    if !bases.is_empty() {
        let all = MultiPolygon::union_all(&bases);
        thickened = thicken_amount(&all);
        if thickened > 0.0 {
            for it in &mut items {
                it.region = it.region.offset(thickened);
                it.curves = None;
            }
        }
    }
    // 3. small parts
    let mut enlarged = 0;
    for it in &mut items {
        let n = enlarge_small(&mut it.region);
        if n > 0 {
            enlarged += n;
            it.curves = None;
        }
    }
    // 4. join overlap (also locates the join points, used as exit/entry of
    // pre-digitized letters)
    let mut join_points: Vec<(usize, Point)> = Vec::new();
    let h = if shape.join_overlap.is_finite() {
        (shape.join_overlap / 2.0).clamp(0.0, 2.0)
    } else {
        0.0
    }
    .max(0.02);
    {
        let base_idx: Vec<Option<usize>> = (0..nc)
            .map(|c| items.iter().position(|i| i.cluster == c && !i.is_mark))
            .collect();
        for c in 0..nc {
            let Some(next) = lay.clusters[c].joins else {
                continue;
            };
            let (Some(a), Some(b)) = (base_idx[c], base_idx[next]) else {
                continue;
            };
            let ra = items[a].region.bounds().inflate(h);
            let rb = items[b].region.bounds().inflate(h);
            if !ra.intersects(&rb) {
                continue;
            }
            let bridge = items[a]
                .region
                .offset(h)
                .intersection(&items[b].region.offset(h));
            if bridge.is_empty() {
                continue;
            }
            join_points.push((c, mp_centroid(&bridge)));
            if shape.join_overlap.is_finite() && shape.join_overlap <= 1e-4 {
                continue;
            }
            let merged = items[a].region.union(&bridge);
            if (merged.area() - items[a].region.area()).abs() > 1e-9 {
                items[a].region = merged;
                items[a].curves = None;
            }
        }
    }

    // 5. layout on a curve
    let ink = items
        .iter()
        .fold(Rect::EMPTY, |r, i| r.union(&i.region.bounds()));
    let mut cluster_box = vec![Rect::EMPTY; nc];
    for it in &items {
        cluster_box[it.cluster] = cluster_box[it.cluster].union(&it.region.bounds());
    }
    let warp_on: Vec<bool> = (0..nc)
        .map(|c| lay.clusters[c].rtl || script_font[c])
        .collect();
    let mut place = placement(shape, &ink, &cluster_box, &warp_on);
    for it in &mut items {
        it.region = place.region(it.cluster, &it.region);
        it.curves = match place.affine(it.cluster) {
            Some(m) => it.curves.as_ref().map(|c| c.transform(&m)),
            None => None,
        };
    }
    // 6. centre
    if !matches!(place.warp, Some(Warp::Path { .. })) {
        let b = items
            .iter()
            .fold(Rect::EMPTY, |r, i| r.union(&i.region.bounds()));
        if !b.is_empty() {
            let c = b.center();
            let t = Affine::translate(-c.x, -c.y);
            place.post = t;
            for it in &mut items {
                it.region = it.region.transform(&t);
                it.curves = it.curves.as_ref().map(|p| p.transform(&t));
            }
        }
    }

    // 7. sew order: line, word, bases before marks, logical letter order
    items.sort_by_key(|i| {
        let c = &lay.clusters[i.cluster];
        (c.line, c.word, i.is_mark, c.first_char)
    });

    // 8. merged outline
    let boxes: Vec<Rect> = items.iter().map(|i| i.region.bounds()).collect();
    let mut path = PathData::new();
    let mut rest: Vec<MultiPolygon> = Vec::new();
    for (k, it) in items.iter().enumerate() {
        let alone = !boxes
            .iter()
            .enumerate()
            .any(|(j, b)| j != k && b.intersects(&boxes[k]));
        match (&it.curves, alone) {
            (Some(c), true) => path.subpaths.extend(c.subpaths.iter().cloned()),
            _ => rest.push(it.region.clone()),
        }
    }
    if !rest.is_empty() {
        path.subpaths
            .extend(MultiPolygon::union_all(&rest).to_path().subpaths);
    }

    let regions = items
        .into_iter()
        .map(|it| {
            let c = &lay.clusters[it.cluster];
            TextRegion {
                region: it.region,
                text: c.text.clone(),
                line: c.line,
                word: c.word,
                is_mark: it.is_mark,
                rtl: c.rtl,
            }
        })
        .collect();
    Ok(Built {
        out: TextOutlines {
            regions,
            path,
            font: String::new(),
            fell_back: false,
            thickened_mm: thickened,
            enlarged_parts: enlarged,
            warnings: Vec::new(),
            origin: place.post.apply(Point::new(0.0, 0.0)),
        },
        place,
        join_points,
    })
}
