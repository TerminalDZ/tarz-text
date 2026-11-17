//! Shaping (rustybuzz) and straight line layout: bidi runs, kashida,
//! letter spacing, alignment, line spacing. Produces positioned glyphs in mm
//! (y down, first baseline at y = 0).

use super::FontRegistry;
use super::bidi::{self, Joining};
use crate::Result;
use crate::design::{TextAlign, TextShape};
use crate::geom::{PathCmd, PathData, Point, SubPath};
use rustybuzz::ttf_parser::{self, GlyphId};

pub(crate) struct Glyph {
    pub font: usize,
    pub gid: u16,
    /// Glyph origin in mm (y down).
    pub x: f64,
    pub y: f64,
    /// mm per font unit.
    pub scale: f64,
    pub cluster: usize,
    pub is_mark: bool,
}

pub(crate) struct Cluster {
    pub line: usize,
    pub text: String,
    pub word: usize,
    pub rtl: bool,
    /// Logical position in the line (first char index).
    pub first_char: usize,
    /// Cluster this one joins to (next letter in logical order).
    pub joins: Option<usize>,
}

pub(crate) struct Layout {
    pub glyphs: Vec<Glyph>,
    pub clusters: Vec<Cluster>,
}

/// Units of the measure that `size_mm` maps to (see module docs): the ink
/// height (from the baseline, or the lowest point when it dips below) of the
/// shaped isolated alef for Arabic text, the capital height otherwise.
pub(crate) fn ref_height_units(face: &rustybuzz::Face, arabic: bool) -> f64 {
    let shaped_height = |c: char| -> Option<f64> {
        face.glyph_index(c)?;
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(&c.to_string());
        buf.guess_segment_properties();
        let out = rustybuzz::shape(face, &[], buf);
        let g = out.glyph_infos().first()?.glyph_id as u16;
        // bbox of the actual outline (header boxes of composites can lie)
        let mut b = Builder {
            ox: 0.0,
            oy: 0.0,
            s: 1.0,
            cur: None,
            out: PathData::new(),
        };
        face.outline_glyph(GlyphId(g), &mut b);
        if let Some(sp) = b.cur.take() {
            b.out.subpaths.push(sp);
        }
        let r = b.out.bounds(); // y down: top = -min_y
        let (top, bottom) = (-r.min_y, -r.max_y);
        let h = top - bottom.min(0.0);
        (!r.is_empty() && top > 0.0 && h > 0.0).then_some(h)
    };
    let upm = face.units_per_em() as f64;
    if arabic && let Some(h) = shaped_height('\u{0627}') {
        return h;
    }
    if let Some(h) = face.capital_height().filter(|&h| h > 0) {
        return h as f64;
    }
    shaped_height('H').unwrap_or(upm * 0.7)
}

fn covers(face: &ttf_parser::Face, s: &str) -> bool {
    s.chars().all(|c| {
        c.is_whitespace()
            || c.is_control()
            || c == '\u{200C}'
            || c == '\u{200D}'
            || face.glyph_index(c).is_some()
    })
}

fn is_mark_glyph(face: &ttf_parser::Face, gid: u16) -> bool {
    face.tables()
        .gdef
        .and_then(|g| g.glyph_class(GlyphId(gid)))
        .map(|c| c == ttf_parser::gdef::GlyphClass::Mark)
        .unwrap_or(false)
}

pub(crate) fn layout(shape: &TextShape, fonts: &FontRegistry, primary: usize) -> Result<Layout> {
    let arabic_text = bidi::has_arabic(&shape.text);
    let mut scales: Vec<Option<f64>> = vec![None; fonts.fonts.len()];
    let mut scale_of = |idx: usize| -> f64 {
        if let Some(s) = scales[idx] {
            return s;
        }
        let face = fonts.face(idx).expect("checked face");
        let s = shape.size_mm / ref_height_units(face, arabic_text).max(1.0);
        scales[idx] = Some(s);
        s
    };
    let pface = fonts.face(primary).expect("checked face");
    let pscale = scale_of(primary);
    let line_units =
        (pface.ascender() as f64 - pface.descender() as f64 + pface.line_gap() as f64).max(1.0);
    let line_spacing = if shape.line_spacing.is_finite() && shape.line_spacing > 0.0 {
        shape.line_spacing
    } else {
        1.2
    };
    let line_height = line_units * pscale * line_spacing;
    let spacing = if shape.letter_spacing.is_finite() {
        shape.letter_spacing
    } else {
        0.0
    };
    // Lines without letters (dates, numbers) follow the font: RTL with an
    // Arabic-capable font (as typed in an Arabic interface), LTR with a
    // Latin-only font.
    let arabic_font = fonts
        .fonts
        .get(primary)
        .map(|f| f.supports_arabic())
        .unwrap_or(true);
    let default_rtl =
        arabic_text || (arabic_font && !shape.text.chars().any(|c| c.is_alphabetic()));

    let mut glyphs: Vec<Glyph> = Vec::new();
    let mut clusters: Vec<Cluster> = Vec::new();
    let mut line_extents: Vec<(usize, f64)> = Vec::new(); // (first glyph, width)
    let mut word = 0usize;
    let mut in_word = false;
    let text = shape.text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    for (li, raw) in lines.iter().enumerate() {
        let line = bidi::insert_kashida(raw, shape.kashida.min(64));
        let chars: Vec<char> = line.chars().collect();
        let joins = bidi::joins_next(&chars);
        // word id per char
        let mut words = vec![0usize; chars.len()];
        for (i, &c) in chars.iter().enumerate() {
            if c.is_whitespace() {
                if in_word {
                    word += 1;
                    in_word = false;
                }
            } else {
                in_word = true;
            }
            words[i] = word;
        }
        if in_word {
            word += 1;
            in_word = false;
        }
        let para_rtl = bidi::paragraph_rtl(&line, default_rtl);
        let runs = bidi::visual_runs(&chars, para_rtl);
        let line_first_glyph = glyphs.len();
        let mut pen = 0.0f64;
        let mut first_in_line = true;
        for run in runs {
            let run_str: String = chars[run.start..run.end].iter().collect();
            // font for this run
            let mut font = primary;
            if !covers(pface, &run_str) {
                let need_arabic = bidi::has_arabic(&run_str);
                if let Some(i) = (0..fonts.fonts.len()).find(|&i| {
                    let info = &fonts.fonts[i];
                    // Arabic runs need an Arabic font; other runs (Latin,
                    // Tifinagh…) take the first font that covers them.
                    let ok_script = !need_arabic || info.supports_arabic();
                    ok_script && fonts.face(i).map(|f| covers(f, &run_str)).unwrap_or(false)
                }) {
                    font = i;
                }
            }
            let face = fonts.face(font).expect("checked face");
            let scale = scale_of(font);
            let mut buf = rustybuzz::UnicodeBuffer::new();
            buf.push_str(&run_str);
            buf.guess_segment_properties();
            buf.set_direction(if run.rtl() {
                rustybuzz::Direction::RightToLeft
            } else {
                rustybuzz::Direction::LeftToRight
            });
            let out = rustybuzz::shape(face, &[], buf);
            // byte offset → char index (within run)
            let mut byte_to_char = vec![0usize; run_str.len() + 1];
            for (ci, (b, _)) in run_str.char_indices().enumerate() {
                byte_to_char[b] = ci;
            }
            byte_to_char[run_str.len()] = run.end - run.start;
            // cluster starts (char index in line), sorted
            let mut starts: Vec<usize> = out
                .glyph_infos()
                .iter()
                .map(|g| run.start + byte_to_char[g.cluster as usize])
                .collect();
            starts.sort_unstable();
            starts.dedup();
            let base_cluster = clusters.len();
            for (k, &s) in starts.iter().enumerate() {
                let e = starts.get(k + 1).copied().unwrap_or(run.end);
                clusters.push(Cluster {
                    line: li,
                    text: chars[s..e].iter().collect(),
                    word: words[s],
                    rtl: run.rtl(),
                    first_char: s,
                    joins: None,
                });
            }
            let cluster_of_char = |ci: usize| -> Option<usize> {
                if ci < run.start || ci >= run.end {
                    return None;
                }
                let k = match starts.binary_search(&ci) {
                    Ok(k) => k,
                    Err(k) => k.checked_sub(1)?,
                };
                Some(base_cluster + k)
            };
            for (k, &s) in starts.iter().enumerate() {
                let e = starts.get(k + 1).copied().unwrap_or(run.end);
                let last = (s..e)
                    .rev()
                    .find(|&i| bidi::joining(chars[i]) != Joining::T);
                if let Some(last) = last
                    && joins[last]
                {
                    let next = (e..run.end).find(|&i| bidi::joining(chars[i]) != Joining::T);
                    if let Some(nc) = next.and_then(cluster_of_char) {
                        clusters[base_cluster + k].joins = Some(nc);
                    }
                }
            }
            let infos = out.glyph_infos();
            let pos = out.glyph_positions();
            let mut prev_cluster: Option<usize> = None;
            for (gi, p) in infos.iter().zip(pos.iter()) {
                let cl = cluster_of_char(run.start + byte_to_char[gi.cluster as usize])
                    .unwrap_or(base_cluster);
                if let Some(pc) = prev_cluster {
                    if pc != cl {
                        // logical order: in RTL runs the visually previous
                        // glyph comes later in the text.
                        let (a, b) = if run.rtl() { (cl, pc) } else { (pc, cl) };
                        if clusters[a].joins != Some(b) {
                            pen += spacing;
                        }
                    }
                } else if !first_in_line {
                    pen += spacing;
                }
                prev_cluster = Some(cl);
                first_in_line = false;
                let gid = gi.glyph_id as u16;
                let is_mark = is_mark_glyph(face, gid)
                    || (p.x_advance == 0 && clusters[cl].text.chars().any(bidi::is_mark));
                glyphs.push(Glyph {
                    font,
                    gid,
                    x: pen + p.x_offset as f64 * scale,
                    y: li as f64 * line_height - p.y_offset as f64 * scale,
                    scale,
                    cluster: cl,
                    is_mark,
                });
                pen += p.x_advance as f64 * scale;
            }
        }
        line_extents.push((line_first_glyph, pen));
    }
    // alignment
    let maxw = line_extents.iter().map(|e| e.1).fold(0.0, f64::max);
    for (k, &(first, w)) in line_extents.iter().enumerate() {
        let end = line_extents.get(k + 1).map(|e| e.0).unwrap_or(glyphs.len());
        let dx = match shape.align {
            TextAlign::Left => 0.0,
            TextAlign::Center => (maxw - w) / 2.0,
            TextAlign::Right => maxw - w,
        };
        for g in &mut glyphs[first..end] {
            g.x += dx;
        }
    }
    Ok(Layout { glyphs, clusters })
}

pub(crate) struct Builder {
    ox: f64,
    oy: f64,
    s: f64,
    cur: Option<SubPath>,
    out: PathData,
}

impl Builder {
    pub(crate) fn new(ox: f64, oy: f64, s: f64) -> Self {
        Builder {
            ox,
            oy,
            s,
            cur: None,
            out: PathData::new(),
        }
    }
    pub(crate) fn finish(mut self) -> PathData {
        if let Some(sp) = self.cur.take() {
            self.out.subpaths.push(sp);
        }
        self.out.subpaths.retain(|s| !s.segs.is_empty());
        self.out
    }
    fn p(&self, x: f32, y: f32) -> Point {
        Point::new(self.ox + x as f64 * self.s, self.oy - y as f64 * self.s)
    }
}

impl ttf_parser::OutlineBuilder for Builder {
    fn move_to(&mut self, x: f32, y: f32) {
        if let Some(sp) = self.cur.take() {
            self.out.subpaths.push(sp);
        }
        self.cur = Some(SubPath::new(self.p(x, y)));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.p(x, y);
        if let Some(sp) = self.cur.as_mut() {
            sp.segs.push(PathCmd::L { p });
        }
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (c, p) = (self.p(x1, y1), self.p(x, y));
        if let Some(sp) = self.cur.as_mut() {
            sp.segs.push(PathCmd::Q { c, p });
        }
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (c1, c2, p) = (self.p(x1, y1), self.p(x2, y2), self.p(x, y));
        if let Some(sp) = self.cur.as_mut() {
            sp.segs.push(PathCmd::C { c1, c2, p });
        }
    }
    fn close(&mut self) {
        if let Some(mut sp) = self.cur.take() {
            sp.closed = true;
            self.out.subpaths.push(sp);
        }
    }
}

/// Outline of a placed glyph in mm (y down), closed sub-paths.
pub(crate) fn glyph_path(fonts: &FontRegistry, g: &Glyph) -> PathData {
    let Some(face) = fonts.face(g.font) else {
        return PathData::new();
    };
    let mut b = Builder {
        ox: g.x,
        oy: g.y,
        s: g.scale,
        cur: None,
        out: PathData::new(),
    };
    face.outline_glyph(GlyphId(g.gid), &mut b);
    if let Some(mut sp) = b.cur.take() {
        sp.closed = true;
        b.out.subpaths.push(sp);
    }
    for sp in &mut b.out.subpaths {
        sp.closed = true;
    }
    b.out.subpaths.retain(|s| !s.segs.is_empty());
    b.out
}

#[cfg(test)]
mod digit_direction_tests {
    use super::layout;
    use crate::FontRegistry;
    use crate::design::TextShape;

    /// x of the first glyph of the cluster that starts at logical char `i`.
    fn x_of(text: &str, font: &str, i: usize) -> f64 {
        let fonts = FontRegistry::bundled();
        let t = TextShape {
            text: text.into(),
            font: font.into(),
            size_mm: 10.0,
            ..Default::default()
        };
        let idx = fonts.index_of(font).unwrap();
        let l = layout(&t, &fonts, idx).unwrap();
        let c = l.clusters.iter().position(|c| c.first_char == i).unwrap();
        l.glyphs.iter().find(|g| g.cluster == c).unwrap().x
    }

    #[test]
    fn latin_font_digits_read_left_to_right() {
        assert!(x_of("14 08 2026", "cinzel", 0) < x_of("14 08 2026", "cinzel", 6));
    }

    #[test]
    fn arabic_font_digits_keep_rtl_default() {
        assert!(x_of("14 08 2026", "amiri", 0) > x_of("14 08 2026", "amiri", 6));
    }
}
