//! Pre-digitized lettering: glyph pieces from the `.tzf` fonts placed with
//! the same layout as the outlines (straight / arc / path, centring), with
//! the embroidery minimums applied and entry/exit points chained so joined
//! Arabic letters sew without trims.

use super::layout::Layout;
use super::region::{Built, Placement};
use super::skeleton::satin_region;
use super::tzf::{UnitPiece, convert_glyph};
use super::{
    FontRegistry, MAX_SATIN_MM, MIN_DOT_MM, MIN_SATIN_MM, TextReport, TextStitchObject,
    TextStitchShape,
};
use crate::design::TextShape;
use crate::geom::{MultiPolygon, PathData, Point, Rect};

/// Max distance between rungs/points before warping (mm).
const WARP_STEP: f64 = 0.5;

#[derive(Debug, Clone)]
enum Kind {
    Satin(Vec<Point>, Vec<Point>),
    Fill(MultiPolygon),
    Run(Vec<Point>),
}

#[derive(Debug, Clone)]
struct Piece {
    kind: Kind,
    cluster: usize,
    is_mark: bool,
}

impl Kind {
    fn bounds(&self) -> Rect {
        match self {
            Kind::Satin(a, b) => Rect::from_points(a.iter().chain(b).copied()),
            Kind::Fill(m) => m.bounds(),
            Kind::Run(p) => Rect::from_points(p.iter().copied()),
        }
    }
    fn map(&mut self, f: &impl Fn(Point) -> Point) {
        match self {
            Kind::Satin(a, b) => {
                a.iter_mut().for_each(|p| *p = f(*p));
                b.iter_mut().for_each(|p| *p = f(*p));
            }
            Kind::Fill(m) => {
                for poly in &mut m.0 {
                    poly.outer.iter_mut().for_each(|p| *p = f(*p));
                    for h in &mut poly.holes {
                        h.iter_mut().for_each(|p| *p = f(*p));
                    }
                }
            }
            Kind::Run(p) => p.iter_mut().for_each(|q| *q = f(*q)),
        }
    }
    /// Start / end points in the current orientation.
    fn ends(&self) -> (Point, Point) {
        match self {
            Kind::Satin(a, b) => ((a[0] + b[0]) / 2.0, (a[a.len() - 1] + b[b.len() - 1]) / 2.0),
            Kind::Fill(m) => {
                let c = m.bounds().center();
                (c, c)
            }
            Kind::Run(p) => (p[0], p[p.len() - 1]),
        }
    }
    fn reverse(&mut self) {
        match self {
            Kind::Satin(a, b) => {
                a.reverse();
                b.reverse();
            }
            Kind::Run(p) => p.reverse(),
            Kind::Fill(_) => {}
        }
    }
    fn region(&self) -> MultiPolygon {
        match self {
            Kind::Satin(a, b) => satin_region(a, b),
            Kind::Fill(m) => m.clone(),
            Kind::Run(p) => MultiPolygon::union_all(
                &p.windows(2)
                    .map(|w| {
                        let n = (w[1] - w[0]).normalized().perp() * 0.1;
                        MultiPolygon::from_rings_evenodd(&[vec![
                            w[0] + n,
                            w[1] + n,
                            w[1] - n,
                            w[0] - n,
                        ]])
                    })
                    .collect::<Vec<_>>(),
            ),
        }
    }
}

fn nearest_vertex(m: &MultiPolygon, t: Point) -> Point {
    m.0.iter()
        .flat_map(|p| p.outer.iter())
        .copied()
        .min_by(|a, b| a.dist(t).total_cmp(&b.dist(t)))
        .unwrap_or(t)
}

/// Distance from `t` to the closer end of a piece (fills: nearest vertex).
fn end_dist(k: &Kind, t: Point) -> (f64, bool) {
    match k {
        Kind::Fill(m) => (nearest_vertex(m, t).dist(t), false),
        _ => {
            let (s, e) = k.ends();
            if e.dist(t) < s.dist(t) {
                (e.dist(t), true)
            } else {
                (s.dist(t), false)
            }
        }
    }
}

/// A satin piece that would sew badly: one rail much shorter than the other
/// (a fan, every stitch through nearly the same hole) or rungs that cross
/// each other (a twisted column). Such pieces are filled instead.
fn satin_degenerate(a: &[Point], b: &[Point]) -> bool {
    // extent (bounding-box diagonal), not path length: a jittery rail that
    // stays in place is still a fan hub
    let len = |r: &[Point]| {
        let bb = Rect::from_points(r.iter().copied());
        bb.width().hypot(bb.height())
    };
    let (la, lb) = (len(a), len(b));
    let (short, long) = (la.min(lb), la.max(lb));
    if long > 4.0 && short < long * FAN_RAIL_RATIO {
        return true;
    }
    let n = a.len().min(b.len());
    if n < 3 {
        return false;
    }
    let crossings = (0..n - 1)
        .filter(|&i| segments_cross(a[i], b[i], a[i + 1], b[i + 1]))
        .count();
    crossings as f64 > (n - 1) as f64 * 0.1
}

/// Proper intersection of segments p1-p2 and q1-q2 (shared end points excluded).
fn segments_cross(p1: Point, p2: Point, q1: Point, q2: Point) -> bool {
    let o = |a: Point, b: Point, c: Point| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    let (d1, d2) = (o(q1, q2, p1), o(q1, q2, p2));
    let (d3, d4) = (o(p1, p2, q1), o(p1, p2, q2));
    d1 * d2 < -1e-12 && d3 * d4 < -1e-12
}

/// Satin pieces smaller than this (bounding-box diagonal, mm) are stubs.
const STUB_MAX_MM: f64 = 2.0;
/// Fraction of a stub's area other pieces must cover for it to be dropped.
const STUB_COVERED: f64 = 0.6;

/// Rail extent ratio below which a satin piece counts as a fan (a thick
/// bowl around a small hole: stitches would pile up on the inner edge).
const FAN_RAIL_RATIO: f64 = 0.5;

fn densify_satin(a: &[Point], b: &[Point], step: f64) -> (Vec<Point>, Vec<Point>) {
    let mut ra = vec![a[0]];
    let mut rb = vec![b[0]];
    for i in 1..a.len() {
        let d = a[i].dist(a[i - 1]).max(b[i].dist(b[i - 1]));
        let n = (d / step).ceil().max(1.0) as usize;
        for k in 1..=n {
            let t = k as f64 / n as f64;
            ra.push(a[i - 1].lerp(a[i], t));
            rb.push(b[i - 1].lerp(b[i], t));
        }
    }
    (ra, rb)
}

fn scale_about(k: &mut Kind, c: Point, f: f64) {
    k.map(&|p| c + (p - c) * f);
}

pub(crate) fn build_objects(
    shape: &TextShape,
    fonts: &FontRegistry,
    lay: &Layout,
    built: &Built,
    report: &mut TextReport,
) -> Vec<TextStitchObject> {
    let place: &Placement = &built.place;
    // 1. pieces in straight layout coordinates (mm)
    let mut pieces: Vec<Piece> = Vec::new();
    let mut converted = 0usize;
    for g in &lay.glyphs {
        let Some(face) = fonts.face(g.font) else {
            continue;
        };
        let tzf = fonts.tzf(g.font);
        let glyph = match tzf.and_then(|t| t.glyph(g.gid)) {
            Some(gl) => gl.clone(),
            None => {
                converted += 1;
                let ref_units = tzf.map(|t| t.ref_units).unwrap_or_else(|| {
                    super::layout::ref_height_units(face, fonts.fonts[g.font].supports_arabic())
                });
                convert_glyph(face, g.gid, ref_units)
            }
        };
        let to_mm = |p: Point| Point::new(g.x + p.x * g.scale, g.y + p.y * g.scale);
        for tp in &glyph.pieces {
            let mut kind = match tp.decode() {
                UnitPiece::Satin(a, b) if a.len() >= 2 && a.len() == b.len() => Kind::Satin(a, b),
                UnitPiece::Satin(..) => continue,
                UnitPiece::Fill(p) => Kind::Fill(MultiPolygon(vec![p])),
                UnitPiece::Run(p) if p.len() >= 2 => Kind::Run(p),
                UnitPiece::Run(_) => continue,
            };
            kind.map(&to_mm);
            pieces.push(Piece {
                kind,
                cluster: g.cluster,
                is_mark: g.is_mark,
            });
        }
    }
    if converted > 0 {
        report.converted_glyphs += converted;
    }

    // 1b. drop tiny satin stubs (outline-to-skeleton leftovers at junctions)
    // that the other pieces of the same letter already cover: they only stack
    // extra layers onto a crowded join. Dots and marks are never covered, so
    // they stay.
    let mut drop = vec![false; pieces.len()];
    for i in 0..pieces.len() {
        let Kind::Satin(a, b) = &pieces[i].kind else {
            continue;
        };
        let bb = Rect::from_points(a.iter().chain(b.iter()).copied());
        if pieces[i].is_mark || bb.width().hypot(bb.height()) > STUB_MAX_MM {
            continue;
        }
        let own = pieces[i].kind.region();
        let own_area = own.area();
        if own_area <= 1e-9 {
            continue;
        }
        let others: Vec<MultiPolygon> = pieces
            .iter()
            .enumerate()
            .filter(|(j, q)| *j != i && !drop[*j] && q.cluster == pieces[i].cluster && !q.is_mark)
            .filter(|(_, q)| q.kind.bounds().intersects(&bb))
            .map(|(_, q)| q.kind.region())
            .collect();
        if others.is_empty() {
            continue;
        }
        let covered = own.intersection(&MultiPolygon::union_all(&others)).area();
        if covered >= own_area * STUB_COVERED {
            drop[i] = true;
        }
    }
    let mut k = 0;
    pieces.retain(|_| {
        k += 1;
        !drop[k - 1]
    });

    // 2. bold, minimum satin width, minimum dot size, maximum satin width
    let bold = if shape.bold.is_finite() {
        shape.bold.clamp(-2.0, 5.0)
    } else {
        0.0
    };
    let mut widened = 0usize;
    let mut enlarged = 0usize;
    for pc in &mut pieces {
        if bold.abs() > 1e-9 {
            match &mut pc.kind {
                Kind::Satin(a, b) => {
                    for (p, q) in a.iter_mut().zip(b.iter_mut()) {
                        let d = (*p - *q).normalized();
                        *p = *p + d * bold;
                        *q = *q - d * bold;
                    }
                }
                Kind::Fill(m) => *m = m.offset(bold),
                Kind::Run(_) => {}
            }
        }
        // dots / harakat: at least MIN_DOT_MM across
        let bb = pc.kind.bounds();
        let size = bb.width().max(bb.height());
        if size > 1e-6 && size < MIN_DOT_MM {
            scale_about(&mut pc.kind, bb.center(), MIN_DOT_MM / size);
            enlarged += 1;
        }
        if let Kind::Satin(a, b) = &mut pc.kind {
            let n = a.len();
            for i in 0..n {
                let w = a[i].dist(b[i]);
                if w < MIN_SATIN_MM {
                    let m = (a[i] + b[i]) / 2.0;
                    let mut d = (a[i] - b[i]).normalized();
                    if d.len() < 0.5 {
                        let t = (a[(i + 1).min(n - 1)] - a[i.saturating_sub(1)]).normalized();
                        d = t.perp();
                    }
                    a[i] = m + d * (MIN_SATIN_MM / 2.0);
                    b[i] = m - d * (MIN_SATIN_MM / 2.0);
                    widened += 1;
                }
            }
            let max_w = a
                .iter()
                .zip(b.iter())
                .map(|(p, q)| p.dist(*q))
                .fold(0.0, f64::max);
            if max_w > MAX_SATIN_MM || satin_degenerate(a, b) {
                pc.kind = Kind::Fill(satin_region(a, b));
            }
        }
    }
    report.widened_rungs += widened;
    report.enlarged_dots += enlarged;

    // 3. join overlap: extend the satin ends that meet at a join
    let h = if shape.join_overlap.is_finite() {
        (shape.join_overlap / 2.0).clamp(0.0, 2.0)
    } else {
        0.0
    };
    if h > 1e-6 {
        for &(c, j) in &built.join_points {
            let Some(next) = lay.clusters[c].joins else {
                continue;
            };
            for pc in pieces
                .iter_mut()
                .filter(|p| !p.is_mark && (p.cluster == c || p.cluster == next))
            {
                if let Kind::Satin(a, b) = &mut pc.kind {
                    let n = a.len();
                    for at_end in [false, true] {
                        let (i, k) = if at_end { (n - 1, n - 2) } else { (0, 1) };
                        let m = (a[i] + b[i]) / 2.0;
                        let reach = a[i].dist(b[i]).max(1.0);
                        if m.dist(j) > reach {
                            continue;
                        }
                        let d = (m - (a[k] + b[k]) / 2.0).normalized();
                        a[i] = a[i] + d * h;
                        b[i] = b[i] + d * h;
                    }
                }
            }
        }
    }

    // 4. final placement
    for pc in &mut pieces {
        let c = pc.cluster;
        if place.is_warped(c) {
            match &mut pc.kind {
                Kind::Satin(a, b) => {
                    let (da, db) = densify_satin(a, b, WARP_STEP);
                    *a = da;
                    *b = db;
                }
                Kind::Run(p) => *p = crate::geom::subdivide_polyline(p, WARP_STEP),
                Kind::Fill(_) => {}
            }
        }
        match &mut pc.kind {
            Kind::Fill(m) => *m = place.region(c, m),
            k => k.map(&|p| place.point(c, p)),
        }
    }
    let join_final: Vec<(usize, Point)> = built
        .join_points
        .iter()
        .map(|&(c, j)| (c, place.point(c, j)))
        .collect();

    // 5. order: clusters in sew order, pieces chained from entry to exit
    let mut groups: Vec<(usize, bool)> = pieces.iter().map(|p| (p.cluster, p.is_mark)).collect();
    groups.sort_by_key(|&(c, m)| {
        let cl = &lay.clusters[c];
        (cl.line, cl.word, m, cl.first_char)
    });
    groups.dedup();
    let mut out: Vec<TextStitchObject> = Vec::new();
    let mut last_exit: Option<Point> = None;
    for &(c, is_mark) in &groups {
        let cl = &lay.clusters[c];
        let mut mine: Vec<Kind> = pieces
            .iter()
            .filter(|p| p.cluster == c && p.is_mark == is_mark)
            .map(|p| p.kind.clone())
            .collect();
        let bb = mine.iter().fold(Rect::EMPTY, |r, k| r.union(&k.bounds()));
        let j_prev = (!is_mark)
            .then(|| {
                join_final
                    .iter()
                    .find(|&&(p, _)| lay.clusters[p].joins == Some(c))
                    .map(|&(_, j)| j)
            })
            .flatten();
        let j_next = (!is_mark)
            .then(|| join_final.iter().find(|&&(p, _)| p == c).map(|&(_, j)| j))
            .flatten();
        let side = |right: bool| Point::new(if right { bb.max_x } else { bb.min_x }, bb.center().y);
        let entry_target = j_prev
            .or(if is_mark { last_exit } else { None })
            .unwrap_or(side(cl.rtl));
        let exit_target = j_next.unwrap_or(side(!cl.rtl));
        // reserve the piece nearest the exit to be sewn last
        let mut last_piece: Option<Kind> = None;
        if j_next.is_some() && mine.len() > 1 {
            let k = (0..mine.len())
                .min_by(|&x, &y| {
                    end_dist(&mine[x], exit_target)
                        .0
                        .total_cmp(&end_dist(&mine[y], exit_target).0)
                })
                .unwrap();
            let mut lp = mine.remove(k);
            // orient so it ends at the exit
            if let (_, false) = end_dist(&lp, exit_target)
                && !matches!(lp, Kind::Fill(_))
            {
                lp.reverse();
            }
            last_piece = Some(lp);
        }
        let mut ordered: Vec<Kind> = Vec::new();
        let mut cur = entry_target;
        // prefer pieces touching the one just sewn, so travel stays under
        // the stitches of the same letter
        let mine_regions: Vec<MultiPolygon> =
            mine.iter().map(|k| k.region().offset(0.05)).collect();
        let mut left: Vec<usize> = (0..mine.len()).collect();
        let mut prev: Option<usize> = None;
        while !left.is_empty() {
            let touching: Vec<usize> = match prev {
                Some(p) => left
                    .iter()
                    .copied()
                    .filter(|&i| !mine_regions[p].intersection(&mine_regions[i]).is_empty())
                    .collect(),
                None => Vec::new(),
            };
            let pool = if touching.is_empty() {
                left.clone()
            } else {
                touching
            };
            let k = pool
                .into_iter()
                .min_by(|&x, &y| {
                    end_dist(&mine[x], cur)
                        .0
                        .total_cmp(&end_dist(&mine[y], cur).0)
                })
                .unwrap();
            left.retain(|&i| i != k);
            let mut pk = mine[k].clone();
            if end_dist(&pk, cur).1 {
                pk.reverse();
            }
            cur = match &pk {
                Kind::Fill(m) => nearest_vertex(m, if left.is_empty() { exit_target } else { cur }),
                k => k.ends().1,
            };
            ordered.push(pk);
            prev = Some(k);
        }
        if let Some(lp) = last_piece {
            ordered.push(lp);
        }
        // a joined letter must end at its join: orient the last piece
        if j_next.is_some()
            && let Some(lp) = ordered.last_mut()
            && !matches!(lp, Kind::Fill(_))
        {
            let (s0, e0) = lp.ends();
            if s0.dist(exit_target) < e0.dist(exit_target) {
                lp.reverse();
            }
        }
        // entry/exit points
        let n = ordered.len();
        let mut objs: Vec<(Kind, Point, Point)> = Vec::with_capacity(n);
        let mut prev_end = entry_target;
        for (i, k) in ordered.into_iter().enumerate() {
            let next_hint = if i + 1 == n { exit_target } else { prev_end };
            let (entry, exit) = match &k {
                Kind::Fill(m) => (nearest_vertex(m, prev_end), nearest_vertex(m, next_hint)),
                _ => k.ends(),
            };
            prev_end = exit;
            objs.push((k, entry, exit));
        }
        // fills: exit towards the next piece's entry
        for i in 0..objs.len().saturating_sub(1) {
            if let Kind::Fill(m) = &objs[i].0 {
                let target = objs[i + 1].1;
                objs[i].2 = nearest_vertex(m, target);
            }
        }
        // connected letter bodies (pieces that touch, transitively)
        let regions: Vec<MultiPolygon> = objs.iter().map(|o| o.0.region().offset(0.05)).collect();
        let mut comp: Vec<usize> = (0..regions.len()).collect();
        fn root(c: &mut [usize], i: usize) -> usize {
            let mut r = i;
            while c[r] != r {
                r = c[r];
            }
            c[i] = r;
            r
        }
        for i in 0..regions.len() {
            for j in i + 1..regions.len() {
                if regions[i].bounds().intersects(&regions[j].bounds())
                    && !regions[i].intersection(&regions[j]).is_empty()
                {
                    let (a, b) = (root(&mut comp, i), root(&mut comp, j));
                    comp[a] = b;
                }
            }
        }
        let comp: Vec<usize> = (0..regions.len()).map(|i| root(&mut comp, i)).collect();
        for (i, (k, entry, exit)) in objs.into_iter().enumerate() {
            let joined_to_next = if i + 1 < regions.len() {
                // same letter body: the engine can travel under the stitches
                comp[i] == comp[i + 1]
            } else {
                !is_mark && cl.joins.is_some() && j_next.is_some()
            };
            last_exit = Some(exit);
            out.push(TextStitchObject {
                shape: match k {
                    Kind::Satin(a, b) => {
                        let rungs = a.iter().copied().zip(b.iter().copied()).collect();
                        TextStitchShape::Satin {
                            rail_a: PathData::from_polyline(&a, false),
                            rail_b: PathData::from_polyline(&b, false),
                            rungs,
                        }
                    }
                    Kind::Fill(m) => TextStitchShape::Fill { region: m },
                    Kind::Run(p) => TextStitchShape::Run {
                        path: PathData::from_polyline(&p, false),
                    },
                },
                entry,
                exit,
                joined_to_next,
                text: cl.text.clone(),
                line: cl.line,
                word: cl.word,
                is_mark,
                rtl: cl.rtl,
            });
        }
    }
    // a joined flag across clusters only holds when the next object really
    // is the joined letter
    for i in 0..out.len() {
        if out[i].joined_to_next && (i + 1 == out.len() || out[i + 1].is_mark != out[i].is_mark) {
            out[i].joined_to_next = false;
        }
    }
    out
}
