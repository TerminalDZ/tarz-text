//! Quality checks of a converted embroidery font (`.tzf` + outline font),
//! used by the Tarz font-library pipeline and by the tests of every
//! generated font.
//!
//! [`check_font`] never panics on bad input (the sample renders run under
//! `catch_unwind`) and reports:
//! - **validity**: the pair registers exactly like a store font
//!   ([`FontRegistry::register_tzf`]: size caps, structure, matching font);
//! - **coverage** of the script's letters, digits, French accents;
//! - **satin widths** at the reference size and the recommended letter
//!   height range derived from them (thinnest typical column ≥
//!   [`SMALL_COLUMN_MM`] at the minimum size; median column ≤
//!   [`MAX_SATIN_MM`] at the maximum size);
//! - **Arabic joins**: joining letters carry their connection anchors;
//! - **renders**: sample text at 20 mm and 5 mm produces stitch objects,
//!   entirely from pre-digitized glyphs;
//! - a **stitch style** label (`satin`, `satin+fill`, `fill`).

use crate::design::TextShape;
use crate::tzf::{FRENCH, TzfFont, TzfPiece, decode_points};
use crate::{FontRegistry, MAX_SATIN_MM, RuntimeFontMeta, TextStitchShape, bidi};
use serde::{Deserialize, Serialize};

/// Narrowest typical satin column accepted at the recommended minimum size
/// (mm). Thinner columns are widened by the engine, which fills counters.
pub const SMALL_COLUMN_MM: f64 = 0.55;
/// Letter height at or below which a font counts as small-text capable (mm).
pub const SMALL_TEXT_LIMIT_MM: f64 = 5.0;

const LATIN: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const DIGITS: &str = "0123456789";
/// The 28 letters of the Arabic alphabet plus hamza forms and taa marbuta.
const ARABIC: &str = "ءآأؤإئابةتثجحخدذرزسشصضطظعغفقكلمنهوىي";
/// Dual-joining Arabic letters (they connect on both sides).
const ARABIC_DUAL: &str = "بتثجحخسشصضطظعغفقكلمنهي";
/// Right-joining letters (they connect to the previous letter only).
const ARABIC_RIGHT: &str = "اأإآدذرزوة";
/// IRCAM Neo-Tifinagh letters (U+2D30–U+2D6F used for Tamazight).
const TIFINAGH: &str = "ⴰⴱⴳⴷⴹⴻⴼⴽⵀⵃⵄⵅⵇⵉⵊⵍⵎⵏⵓⵔⵕⵖⵙⵚⵛⵜⵟⵡⵢⵣⵥ";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    /// Share of the letters present (0..1) and the missing ones.
    pub ratio: f64,
    pub missing: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct JoinCheck {
    /// Joining forms checked / with the expected anchors.
    pub checked: usize,
    pub ok: usize,
    pub ratio: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SatinWidths {
    /// Typical column widths (per piece median) at the 20 mm reference
    /// size: 15th percentile, median, 95th percentile, widest rung (mm).
    pub p15: f64,
    pub median: f64,
    pub p95: f64,
    pub max: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QualityReport {
    pub id: String,
    /// The font passed every blocking check.
    pub ok: bool,
    /// Blocking problems (`code: detail`).
    pub errors: Vec<String>,
    /// Non-blocking findings (`code: detail`).
    pub warnings: Vec<String>,
    /// Engine script of the font (`arabic`, `latin`, `both`, `tifinagh`).
    pub script: String,
    pub glyphs: usize,
    pub satin_pieces: usize,
    pub fill_pieces: usize,
    pub run_pieces: usize,
    pub latin: Coverage,
    pub digits: Coverage,
    pub french: Coverage,
    pub arabic: Coverage,
    pub tifinagh: Coverage,
    /// Arabic fonts only: letter joins sewn without a trim in joined words.
    pub joins: Option<JoinCheck>,
    /// Arabic fonts only: joining forms whose `.tzf` glyph carries its
    /// connection anchors (informational).
    #[serde(default)]
    pub anchors: Option<JoinCheck>,
    pub satin: SatinWidths,
    /// Share of the stitched area sewn as fill (0..1).
    pub fill_ratio: f64,
    /// `satin`, `satin+fill` or `fill`.
    pub stitch: String,
    /// Recommended letter height range (mm).
    pub min_size_mm: f64,
    pub max_size_mm: f64,
    /// Usable for text of [`SMALL_TEXT_LIMIT_MM`] or less.
    pub small_text: bool,
    /// Stitch objects of the 20 mm / 5 mm sample renders.
    pub sample_objects: usize,
    pub small_sample_objects: usize,
}

fn coverage(face: &rustybuzz::Face, set: &str) -> Coverage {
    let total = set.chars().count().max(1);
    let missing: String = set
        .chars()
        .filter(|&c| face.glyph_index(c).is_none())
        .collect();
    Coverage {
        ratio: 1.0 - missing.chars().count() as f64 / total as f64,
        missing,
    }
}

fn ring_area(r: &[crate::geom::Point]) -> f64 {
    crate::geom::ring_signed_area(r).abs()
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

fn round_to(v: f64, step: f64) -> f64 {
    (v / step).round() * step
}

/// Glyph of `c` shaped in its joining context: `"ـcـ"` (medial) or `"ـc"`.
fn contextual_glyph(face: &rustybuzz::Face, c: char, medial: bool) -> Option<u16> {
    let t = bidi::TATWEEL;
    let s = if medial {
        format!("{t}{c}{t}")
    } else {
        format!("{t}{c}")
    };
    let mut buf = rustybuzz::UnicodeBuffer::new();
    buf.push_str(&s);
    buf.guess_segment_properties();
    let out = rustybuzz::shape(face, &[], buf);
    let byte = t.len_utf8() as u32;
    out.glyph_infos()
        .iter()
        .find(|g| g.cluster == byte)
        .map(|g| g.glyph_id as u16)
        .filter(|&g| g != 0)
}

fn join_check(face: &rustybuzz::Face, font: &TzfFont) -> JoinCheck {
    let mut j = JoinCheck::default();
    let mut check = |c: char, medial: bool| {
        if face.glyph_index(c).is_none() {
            return;
        }
        let Some(gid) = contextual_glyph(face, c, medial) else {
            return;
        };
        j.checked += 1;
        let Some(g) = font.glyph(gid) else { return };
        // every joining form connects on its right (entry); medial forms
        // also on their left (exit)
        if g.right.is_some() && (!medial || g.left.is_some()) {
            j.ok += 1;
        }
    };
    for c in ARABIC_DUAL.chars() {
        check(c, true);
    }
    for c in ARABIC_RIGHT.chars() {
        check(c, false);
    }
    j.ratio = if j.checked == 0 {
        0.0
    } else {
        j.ok as f64 / j.checked as f64
    };
    j
}

/// Sample text of the font's script (the library preview uses it too).
pub fn sample_text(script: &str) -> &'static str {
    match script {
        "arabic" | "both" => "طرز ستوديو",
        "tifinagh" => "ⵜⴰⵔⵣ ⴰⵣⵓⵍ",
        _ => "Tarz Studio 2026",
    }
}

fn render_count(
    reg: &FontRegistry,
    font: &str,
    text: &str,
    size: f64,
) -> Result<(usize, usize), String> {
    let shape = TextShape {
        text: text.to_string(),
        font: font.to_string(),
        size_mm: size,
        ..Default::default()
    };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::glyph_objects_report(&shape, reg)
    }));
    match r {
        Err(_) => Err("render_panic".into()),
        Ok(Err(e)) => Err(format!("render_failed: {e}")),
        Ok(Ok(res)) => {
            if res.report.fell_back {
                return Err("render_fell_back".into());
            }
            Ok((res.objects.len(), res.report.converted_glyphs))
        }
    }
}

/// Words whose letters all join (every dual-joining letter connects to the
/// next one): the sewn-join check.
const JOIN_WORDS: &str = "بسم محمد سلسبيل فلسطين تيسير لبيب عبقري نحن";

/// Sewn joins: in [`JOIN_WORDS`] at 20 mm, every boundary after a
/// dual-joining letter must be sewn without a trim (`joined_to_next`).
fn sewn_join_check(reg: &FontRegistry, font: &str) -> Result<JoinCheck, String> {
    let shape = TextShape {
        text: JOIN_WORDS.to_string(),
        font: font.to_string(),
        size_mm: 20.0,
        ..Default::default()
    };
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::glyph_objects_report(&shape, reg)
    }))
    .map_err(|_| "render_panic (joins)".to_string())?
    .map_err(|e| format!("render_failed (joins): {e}"))?;
    let base: Vec<&crate::TextStitchObject> = res.objects.iter().filter(|o| !o.is_mark).collect();
    let mut j = JoinCheck::default();
    for (i, w) in base.windows(2).enumerate() {
        let (a, b) = (w[0], w[1]);
        if a.word != b.word || a.text == b.text {
            continue;
        }
        let Some(last) = a.text.chars().last() else {
            continue;
        };
        if !ARABIC_DUAL.contains(last) {
            continue;
        }
        j.checked += 1;
        // the boundary is sewn through when the last piece of the letter
        // chains on (pieces are in sew order)
        if base[i].joined_to_next {
            j.ok += 1;
        }
    }
    j.ratio = if j.checked == 0 {
        0.0
    } else {
        j.ok as f64 / j.checked as f64
    };
    Ok(j)
}

/// Check a converted font. `id` names the font in the report only.
pub fn check_font(id: &str, tzf: &[u8], outline: &[u8]) -> QualityReport {
    let mut q = QualityReport {
        id: id.to_string(),
        ..Default::default()
    };
    let mut reg = FontRegistry::empty();
    let info = match reg.register_tzf(
        tzf,
        outline,
        &RuntimeFontMeta {
            key: format!("q-{id}"),
            ..Default::default()
        },
    ) {
        Ok(i) => i,
        Err(e) => {
            q.errors.push(format!("invalid: {e}"));
            return q;
        }
    };
    q.script = info.script.clone();
    let (Ok(font), Some(face)) = (
        TzfFont::decode(tzf),
        rustybuzz::Face::from_slice(outline, 0),
    ) else {
        q.errors.push("invalid: cannot decode".into());
        return q;
    };
    q.glyphs = font.glyphs.len();
    // the pieces must belong to this outline font: compare the boxes of a
    // sample of glyphs (pieces vs. outline), y down vs. y up
    let (mut sampled, mut matching) = (0usize, 0usize);
    for g in font.glyphs.iter().filter(|g| !g.pieces.is_empty()).take(60) {
        let Some(bb) = face.glyph_bounding_box(rustybuzz::ttf_parser::GlyphId(g.gid)) else {
            continue;
        };
        let pts: Vec<crate::geom::Point> = g
            .pieces
            .iter()
            .flat_map(|p| match p {
                TzfPiece::Satin { a, b } => [decode_points(a), decode_points(b)].concat(),
                TzfPiece::Fill { rings } => rings.iter().flat_map(|r| decode_points(r)).collect(),
                TzfPiece::Run { pts } => decode_points(pts),
            })
            .collect();
        if pts.is_empty() {
            continue;
        }
        let (x0, x1) = pts
            .iter()
            .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(p.x), a.1.max(p.x)));
        let (y0, y1) = pts
            .iter()
            .fold((f64::MAX, f64::MIN), |a, p| (a.0.min(-p.y), a.1.max(-p.y)));
        let (ox0, ox1, oy0, oy1) = (
            bb.x_min as f64,
            bb.x_max as f64,
            bb.y_min as f64,
            bb.y_max as f64,
        );
        let inter = (x1.min(ox1) - x0.max(ox0)).max(0.0) * (y1.min(oy1) - y0.max(oy0)).max(0.0);
        let union = (x1 - x0) * (y1 - y0) + (ox1 - ox0) * (oy1 - oy0) - inter;
        sampled += 1;
        if union > 0.0 && inter / union > 0.5 {
            matching += 1;
        }
    }
    if sampled > 0 && (matching as f64) < sampled as f64 * 0.8 {
        q.errors.push(format!(
            "outline_mismatch: only {matching} of {sampled} sampled glyphs match the outline font"
        ));
        return q;
    }

    // ---- coverage
    q.latin = coverage(&face, LATIN);
    q.digits = coverage(&face, DIGITS);
    q.french = coverage(&face, FRENCH);
    q.arabic = coverage(&face, ARABIC);
    q.tifinagh = coverage(&face, TIFINAGH);
    match info.script.as_str() {
        "arabic" | "both" => {
            if q.arabic.ratio < 1.0 {
                q.errors
                    .push(format!("coverage_arabic: missing {}", q.arabic.missing));
            }
        }
        "tifinagh" => {
            if q.tifinagh.ratio < 0.9 {
                q.errors
                    .push(format!("coverage_tifinagh: missing {}", q.tifinagh.missing));
            }
        }
        _ => {
            if q.latin.ratio < 1.0 {
                q.errors
                    .push(format!("coverage_latin: missing {}", q.latin.missing));
            }
        }
    }
    if q.digits.ratio < 1.0 {
        q.warnings
            .push(format!("coverage_digits: missing {}", q.digits.missing));
    }
    if info.supports_latin() && q.french.ratio < 1.0 {
        q.warnings
            .push(format!("coverage_french: missing {}", q.french.missing));
    }

    // ---- pieces and satin widths at the reference size
    let mm = font.ref_size_mm / font.ref_units.max(1.0);
    let mut piece_widths: Vec<f64> = Vec::new();
    let (mut satin_area, mut fill_area) = (0.0, 0.0);
    let mut widest: f64 = 0.0;
    for g in &font.glyphs {
        for p in &g.pieces {
            match p {
                TzfPiece::Satin { a, b } => {
                    q.satin_pieces += 1;
                    let (a, b) = (decode_points(a), decode_points(b));
                    let mut w: Vec<f64> = a.iter().zip(&b).map(|(p, q)| p.dist(*q) * mm).collect();
                    if w.is_empty() {
                        continue;
                    }
                    w.sort_by(|x, y| x.total_cmp(y));
                    widest = widest.max(*w.last().unwrap_or(&0.0));
                    piece_widths.push(percentile(&w, 0.5));
                    let mut ring = a.clone();
                    ring.extend(b.iter().rev());
                    satin_area += ring_area(&ring) * mm * mm;
                }
                TzfPiece::Fill { rings } => {
                    q.fill_pieces += 1;
                    let mut it = rings.iter().map(|r| ring_area(&decode_points(r)));
                    let outer = it.next().unwrap_or(0.0);
                    fill_area += (outer - it.sum::<f64>()).max(0.0) * mm * mm;
                }
                TzfPiece::Run { .. } => q.run_pieces += 1,
            }
        }
    }
    piece_widths.sort_by(|x, y| x.total_cmp(y));
    q.satin = SatinWidths {
        p15: round_to(percentile(&piece_widths, 0.15), 0.01),
        median: round_to(percentile(&piece_widths, 0.5), 0.01),
        p95: round_to(percentile(&piece_widths, 0.95), 0.01),
        max: round_to(widest, 0.01),
    };
    // a few rungs at stroke junctions may exceed the limit; the engine sews
    // any satin wider than MAX_SATIN_MM as a fill, so this never blocks
    if q.satin.max > font.max_satin_mm * 1.5 {
        q.warnings.push(format!(
            "satin_wide_rungs: {:.2} mm rung at the reference size (sewn as fill above {:.1} mm)",
            q.satin.max, font.max_satin_mm
        ));
    }
    if q.satin.p95 > font.max_satin_mm + 0.05 {
        q.warnings.push(format!(
            "satin_wide: typical columns of {:.2} mm at the reference size (above {:.1} mm they are sewn as fills)",
            q.satin.p95, font.max_satin_mm
        ));
    }
    let total = satin_area + fill_area;
    q.fill_ratio = if total > 0.0 {
        round_to(fill_area / total, 0.001)
    } else {
        0.0
    };
    q.stitch = if q.satin_pieces == 0 && q.fill_pieces == 0 {
        "run".into()
    } else if q.fill_ratio < 0.15 {
        "satin".into()
    } else if q.fill_ratio < 0.7 {
        "satin+fill".into()
    } else {
        "fill".into()
    };
    // recommended range: thinnest typical column >= SMALL_COLUMN_MM at the
    // minimum, median column <= MAX_SATIN_MM at the maximum
    let r = font.ref_size_mm;
    q.min_size_mm = if q.satin.p15 > 0.0 {
        round_to((r * SMALL_COLUMN_MM / q.satin.p15).clamp(3.0, 40.0), 0.5)
    } else {
        8.0
    };
    q.max_size_mm = if q.satin.median > 0.0 {
        round_to((r * MAX_SATIN_MM / q.satin.median).clamp(40.0, 300.0), 5.0)
    } else {
        300.0
    };
    if q.max_size_mm < q.min_size_mm * 2.0 {
        q.max_size_mm = round_to(q.min_size_mm * 2.0, 5.0);
    }
    q.small_text = q.min_size_mm <= SMALL_TEXT_LIMIT_MM;
    if q.min_size_mm >= 20.0 {
        q.warnings.push(format!(
            "large_only: recommended from {:.0} mm (very thin strokes)",
            q.min_size_mm
        ));
    }

    // ---- Arabic joins
    if info.supports_arabic() {
        // pre-digitized connection anchors (informational: the runtime also
        // joins letters through the join overlap)
        q.anchors = Some(join_check(&face, &font));
        match sewn_join_check(&reg, &info.id) {
            Ok(j) => {
                if j.checked == 0 || j.ratio < 0.9 {
                    q.errors.push(format!(
                        "arabic_joins: {} of {} letter joins are sewn without a trim",
                        j.ok, j.checked
                    ));
                } else if j.ratio < 1.0 {
                    q.warnings.push(format!(
                        "arabic_joins: {} of {} letter joins are sewn without a trim",
                        j.ok, j.checked
                    ));
                }
                q.joins = Some(j);
            }
            Err(e) => q.errors.push(e),
        }
    }

    // ---- sample renders (no panic, real stitch objects, from the .tzf)
    let sample = sample_text(&info.script);
    match render_count(&reg, &info.id, sample, 20.0) {
        Ok((n, converted)) => {
            q.sample_objects = n;
            if n == 0 {
                q.errors.push("render_empty: no stitch objects".into());
            }
            if converted > 0 {
                q.warnings.push(format!(
                    "glyphs_not_predigitized: {converted} sample glyphs converted on the fly"
                ));
            }
        }
        Err(e) => q.errors.push(e),
    }
    match render_count(&reg, &info.id, sample, 5.0) {
        Ok((n, _)) => q.small_sample_objects = n,
        Err(e) => q.errors.push(format!("{e} (5 mm)")),
    }
    q.ok = q.errors.is_empty();
    q
}

/// SVG preview of `text` sewn with the font's pre-digitized pieces (satin
/// columns and fills as filled shapes, runs as strokes), `height_px` tall.
/// `None` when the font cannot be registered or renders nothing.
pub fn preview_svg(tzf: &[u8], outline: &[u8], text: &str, height_px: f64) -> Option<String> {
    let mut reg = FontRegistry::empty();
    let info = reg
        .register_tzf(
            tzf,
            outline,
            &RuntimeFontMeta {
                key: "preview".into(),
                ..Default::default()
            },
        )
        .ok()?;
    let shape = TextShape {
        text: text.to_string(),
        font: info.id.clone(),
        size_mm: 10.0,
        ..Default::default()
    };
    let objects = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        crate::glyph_objects(&shape, &reg)
    }))
    .ok()?
    .ok()?;
    let mut d = String::new();
    let mut runs = String::new();
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    let mut ring = |pts: &[crate::geom::Point], out: &mut String| {
        if pts.len() < 2 {
            return;
        }
        for (i, p) in pts.iter().enumerate() {
            x0 = x0.min(p.x);
            y0 = y0.min(p.y);
            x1 = x1.max(p.x);
            y1 = y1.max(p.y);
            out.push_str(&format!(
                "{}{:.2} {:.2}",
                if i == 0 { "M" } else { "L" },
                p.x,
                p.y
            ));
        }
    };
    for o in &objects {
        match &o.shape {
            TextStitchShape::Satin { rail_a, rail_b, .. } => {
                let a: Vec<_> = rail_a.flatten(0.05).into_iter().flatten().collect();
                let mut b: Vec<_> = rail_b.flatten(0.05).into_iter().flatten().collect();
                b.reverse();
                let mut pts = a;
                pts.extend(b);
                ring(&pts, &mut d);
                d.push('Z');
            }
            TextStitchShape::Fill { region } => {
                for p in &region.0 {
                    for r in p.rings() {
                        ring(r, &mut d);
                        d.push('Z');
                    }
                }
            }
            TextStitchShape::Run { path } => {
                for sp in path.flatten(0.05) {
                    ring(&sp, &mut runs);
                }
            }
        }
    }
    if d.is_empty() && runs.is_empty() {
        return None;
    }
    let pad = 0.6;
    let (w, h) = (x1 - x0 + 2.0 * pad, y1 - y0 + 2.0 * pad);
    let width_px = (height_px * w / h.max(1e-6)).round();
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{:.2} {:.2} {:.2} {:.2}\" width=\"{width_px}\" height=\"{height_px}\">",
        x0 - pad,
        y0 - pad,
        w,
        h
    );
    if !d.is_empty() {
        svg.push_str(&format!(
            "<path d=\"{d}\" fill=\"currentColor\" fill-rule=\"nonzero\"/>"
        ));
    }
    if !runs.is_empty() {
        svg.push_str(&format!(
            "<path d=\"{runs}\" fill=\"none\" stroke=\"currentColor\" stroke-width=\"0.3\"/>"
        ));
    }
    svg.push_str("</svg>");
    Some(svg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bundled(id: &str) -> (Vec<u8>, &'static [u8]) {
        let b = FontRegistry::bundled();
        let i = b.index_of(id).unwrap();
        (b.tzf(i).unwrap().encode().unwrap(), b.font_data(i).unwrap())
    }

    #[test]
    fn arabic_font_passes_with_joins_and_sizes() {
        let (tzf, ttf) = bundled("amiri");
        let q = check_font("amiri", &tzf, ttf);
        assert!(q.ok, "{:?}", q.errors);
        assert_eq!(q.script, "both");
        assert_eq!(q.arabic.ratio, 1.0);
        let j = q.joins.clone().unwrap();
        assert!(j.checked >= 12 && j.ratio >= 0.9, "{j:?}");
        assert!(q.anchors.is_some());
        assert!(q.min_size_mm >= 3.0 && q.min_size_mm < q.max_size_mm);
        assert!(q.sample_objects > 0 && q.small_sample_objects > 0);
        assert!(["satin", "satin+fill", "fill"].contains(&q.stitch.as_str()));
    }

    #[test]
    fn join_flag_tells_joined_from_separate_letters() {
        // the sewn-join check relies on `joined_to_next` being false where
        // Arabic letters do not join (after dal, alef, raa…)
        let reg = FontRegistry::bundled();
        let objs = |text: &str| {
            crate::glyph_objects(
                &TextShape {
                    text: text.into(),
                    font: "amiri".into(),
                    size_mm: 20.0,
                    ..Default::default()
                },
                &reg,
            )
            .unwrap()
        };
        let boundaries = |text: &str| -> Vec<bool> {
            let o: Vec<_> = objs(text).into_iter().filter(|o| !o.is_mark).collect();
            o.windows(2)
                .filter(|w| w[0].text != w[1].text)
                .map(|w| w[0].joined_to_next)
                .collect()
        };
        assert!(boundaries("بسم").iter().all(|&j| j), "joined word");
        assert!(
            boundaries("دار").iter().all(|&j| !j),
            "د and ا do not join the next letter"
        );
    }

    #[test]
    fn latin_font_reports_coverage_and_small_text() {
        let (tzf, ttf) = bundled("montserrat");
        let q = check_font("montserrat", &tzf, ttf);
        assert!(q.ok, "{:?}", q.errors);
        assert!(q.joins.is_none());
        assert_eq!(q.latin.ratio, 1.0);
        assert_eq!(q.french.ratio, 1.0, "{}", q.french.missing);
        assert!(q.satin.p15 > 0.0 && q.satin.p15 <= q.satin.median);
        assert!(q.satin.p95 <= MAX_SATIN_MM + 0.05);
    }

    #[test]
    fn garbage_is_reported_not_panicking() {
        let (tzf, ttf) = bundled("oswald");
        let q = check_font("x", b"TZF1junk", ttf);
        assert!(!q.ok && q.errors[0].starts_with("invalid"));
        let q = check_font("x", &tzf, b"not a font");
        assert!(!q.ok);
        // another font's outline
        let (_, amiri) = bundled("amiri");
        assert!(!check_font("x", &tzf, amiri).ok);
    }

    #[test]
    fn preview_is_svg_with_shapes() {
        let (tzf, ttf) = bundled("reem-kufi");
        let svg = preview_svg(&tzf, ttf, "طرز Tarz", 64.0).unwrap();
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        assert!(svg.contains("<path d=\"M"));
        assert!(preview_svg(b"x", ttf, "a", 64.0).is_none());
    }
}
