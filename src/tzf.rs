//! `.tzf` — Tarz embroidery font: every glyph stored as PRE-DIGITIZED
//! pieces (satin columns given by their two rails, fill regions, running
//! paths) plus left/right connection anchors, so joined Arabic letters can
//! be sewn as one path without trims.
//!
//! # File format (version 1)
//! ```text
//! b"TZF1"  then  deflate( JSON(TzfFont) )
//! ```
//! Coordinates are integer font units, x right, **y down**, origin at the
//! glyph origin on the baseline. Point lists are delta-encoded flat arrays
//! `[x0, y0, dx1, dy1, …]` (small numbers compress well).
//!
//! The pieces are decided at the reference size [`TZF_REF_SIZE_MM`] (letter
//! body height, see the size rule in [`super`]): strokes up to
//! [`MAX_SATIN_MM`] wide become satin columns, wider areas fills, compact
//! dots/harakat a short satin across them. The converter
//! ([`convert_font`]) runs at development time
//! (`cargo run --release --example build_tzf`); glyphs missing from a
//! `.tzf` are converted on the fly with the same algorithm.

use super::MAX_SATIN_MM;
use super::bidi;
use super::layout::{Builder, ref_height_units};
use super::region::nonzero_region;
use super::skeleton::{Piece, decompose};
use crate::geom::{PathData, Point, Polygon};
use crate::{Error, Result};
use rustybuzz::ttf_parser::GlyphId;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

pub const TZF_MAGIC: &[u8; 4] = b"TZF1";
pub const TZF_VERSION: u32 = 1;
/// Letter-body height at which satin/fill decisions are made (mm).
pub const TZF_REF_SIZE_MM: f64 = 20.0;
/// Raster resolution of the converter (cells per em).
const CELLS_PER_EM: f64 = 220.0;
/// Parts smaller than this at the reference size (mm²) are dots/marks.
const DOT_AREA_MM2: f64 = 7.0;

/// One pre-digitized piece, font units, y down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum TzfPiece {
    /// Satin column: `a[i]` faces `b[i]` (delta-encoded point lists).
    Satin { a: Vec<i32>, b: Vec<i32> },
    /// Fill region: outer ring then holes (delta-encoded rings).
    Fill { rings: Vec<Vec<i32>> },
    /// Running stitch path.
    Run { pts: Vec<i32> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TzfGlyph {
    pub gid: u16,
    /// Pieces in a sensible sew order (right to left).
    pub pieces: Vec<TzfPiece>,
    /// Connection anchor on the left edge (where a joined letter continues
    /// to the left, i.e. the exit of an Arabic letter), `[x, y]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub left: Option<[i32; 2]>,
    /// Connection anchor on the right edge (entry of an Arabic letter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub right: Option<[i32; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TzfFont {
    pub version: u32,
    pub font_id: String,
    pub units_per_em: u16,
    /// Font units of the size measure (alef height / capital height).
    pub ref_units: f64,
    pub ref_size_mm: f64,
    pub max_satin_mm: f64,
    /// Glyphs sorted by `gid`.
    pub glyphs: Vec<TzfGlyph>,
}

impl TzfFont {
    pub fn glyph(&self, gid: u16) -> Option<&TzfGlyph> {
        self.glyphs
            .binary_search_by_key(&gid, |g| g.gid)
            .ok()
            .map(|i| &self.glyphs[i])
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        let json = serde_json::to_vec(self).map_err(|e| Error::Font(e.to_string()))?;
        let mut out = TZF_MAGIC.to_vec();
        let mut enc = flate2::write::DeflateEncoder::new(&mut out, flate2::Compression::best());
        enc.write_all(&json)
            .map_err(|e| Error::Font(e.to_string()))?;
        enc.finish().map_err(|e| Error::Font(e.to_string()))?;
        Ok(out)
    }

    pub fn decode(data: &[u8]) -> Result<TzfFont> {
        Self::decode_limited(data, u64::MAX)
    }

    /// [`TzfFont::decode`] that refuses files inflating to more than
    /// `max_inflated` bytes (decompression bombs in untrusted files).
    pub fn decode_limited(data: &[u8], max_inflated: u64) -> Result<TzfFont> {
        if data.len() < 4 || &data[..4] != TZF_MAGIC {
            return Err(Error::format("tzf", "bad magic"));
        }
        let mut json = Vec::new();
        flate2::read::DeflateDecoder::new(&data[4..])
            .take(max_inflated.saturating_add(1))
            .read_to_end(&mut json)
            .map_err(|e| Error::format("tzf", e.to_string()))?;
        if json.len() as u64 > max_inflated {
            return Err(Error::format("tzf", "file inflates beyond the size limit"));
        }
        let f: TzfFont =
            serde_json::from_slice(&json).map_err(|e| Error::format("tzf", e.to_string()))?;
        if f.version != TZF_VERSION {
            return Err(Error::format(
                "tzf",
                format!("unsupported version {}", f.version),
            ));
        }
        Ok(f)
    }
}

pub fn encode_points(pts: &[Point]) -> Vec<i32> {
    let mut out = Vec::with_capacity(pts.len() * 2);
    let (mut px, mut py) = (0i32, 0i32);
    for p in pts {
        let (x, y) = (p.x.round() as i32, p.y.round() as i32);
        out.push(x - px);
        out.push(y - py);
        px = x;
        py = y;
    }
    out
}

pub fn decode_points(v: &[i32]) -> Vec<Point> {
    let mut out = Vec::with_capacity(v.len() / 2);
    let (mut x, mut y) = (0i32, 0i32);
    for c in v.as_chunks::<2>().0 {
        x += c[0];
        y += c[1];
        out.push(Point::new(x as f64, y as f64));
    }
    out
}

/// Decoded piece in font units (y down).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum UnitPiece {
    Satin(Vec<Point>, Vec<Point>),
    Fill(Polygon),
    Run(Vec<Point>),
}

impl TzfPiece {
    pub(crate) fn decode(&self) -> UnitPiece {
        match self {
            TzfPiece::Satin { a, b } => UnitPiece::Satin(decode_points(a), decode_points(b)),
            TzfPiece::Fill { rings } => {
                let mut rs = rings.iter().map(|r| decode_points(r));
                let outer = rs.next().unwrap_or_default();
                UnitPiece::Fill(Polygon::new(outer, rs.collect()))
            }
            TzfPiece::Run { pts } => UnitPiece::Run(decode_points(pts)),
        }
    }
}

/// Glyph outline in font units (x right, y down).
pub(crate) fn glyph_units_path(face: &rustybuzz::Face, gid: u16) -> PathData {
    let mut b = Builder::new(0.0, 0.0, 1.0);
    face.outline_glyph(GlyphId(gid), &mut b);
    let mut p = b.finish();
    for sp in &mut p.subpaths {
        sp.closed = true;
    }
    p
}

/// Connection anchor on the vertical line `x`: middle of the outline span
/// crossing it nearest the baseline.
fn edge_anchor(polys: &[Polygon], x: f64) -> Option<Point> {
    let mut ys: Vec<f64> = Vec::new();
    for p in polys {
        for r in p.rings() {
            let n = r.len();
            for i in 0..n {
                let (a, b) = (r[i], r[(i + 1) % n]);
                if (a.x > x) != (b.x > x) {
                    ys.push(a.y + (x - a.x) * (b.y - a.y) / (b.x - a.x));
                }
            }
        }
    }
    ys.sort_by(|a, b| a.total_cmp(b));
    ys.as_chunks::<2>()
        .0
        .iter()
        .map(|c| (c[0] + c[1]) / 2.0)
        .min_by(|a, b| a.abs().total_cmp(&b.abs()))
        .map(|y| Point::new(x, y))
}

/// Convert one glyph to pre-digitized pieces (font units, y down).
pub fn convert_glyph(face: &rustybuzz::Face, gid: u16, ref_units: f64) -> TzfGlyph {
    let upm = face.units_per_em() as f64;
    let path = glyph_units_path(face, gid);
    let region = nonzero_region(&path, upm / 4000.0);
    let mm_per_unit = TZF_REF_SIZE_MM / ref_units.max(1.0);
    let cell = upm / CELLS_PER_EM;
    let max_w = MAX_SATIN_MM / mm_per_unit;
    let dot_area = DOT_AREA_MM2 / (mm_per_unit * mm_per_unit);
    let mut pieces: Vec<(Point, TzfPiece)> = Vec::new();
    for poly in &region.0 {
        for pc in decompose(poly, cell, max_w, dot_area) {
            match pc {
                Piece::Satin { a, b } => {
                    let c = (a[0] + b[0] + a[a.len() - 1] + b[b.len() - 1]) / 4.0;
                    pieces.push((
                        c,
                        TzfPiece::Satin {
                            a: encode_points(&a),
                            b: encode_points(&b),
                        },
                    ));
                }
                Piece::Fill(p) => {
                    let c = p.bounds().center();
                    let rings = p.rings().map(|r| encode_points(r)).collect();
                    pieces.push((c, TzfPiece::Fill { rings }));
                }
            }
        }
    }
    // default order: right to left (the runtime re-chains from the real
    // entry point)
    pieces.sort_by(|a, b| b.0.x.total_cmp(&a.0.x));
    let adv = face.glyph_hor_advance(GlyphId(gid)).unwrap_or(0) as f64;
    let b = region.bounds();
    let tol = cell * 2.0;
    let right = (!b.is_empty() && b.max_x >= adv - tol && adv > 0.0)
        .then(|| edge_anchor(&region.0, (adv - cell).min(b.max_x - cell * 0.5)))
        .flatten();
    let left = (!b.is_empty() && b.min_x <= tol)
        .then(|| edge_anchor(&region.0, cell.max(b.min_x + cell * 0.5)))
        .flatten();
    let q = |p: Point| [p.x.round() as i32, p.y.round() as i32];
    TzfGlyph {
        gid,
        pieces: pieces.into_iter().map(|p| p.1).collect(),
        left: left.map(q),
        right: right.map(q),
    }
}

/// Accented letters of French (all of them) plus common Western European
/// ones (team and family names).
pub const FRENCH: &str = "àâäæçéèêëîïôöœùûüÿÀÂÄÆÇÉÈÊËÎÏÔÖŒÙÛÜŸ";

/// Glyphs reached when shaping typical text: Arabic letters in all
/// positional forms and in pairs (ligatures, contextual forms), harakat,
/// digits, Latin and punctuation.
pub fn corpus_glyphs(face: &rustybuzz::Face) -> Vec<u16> {
    let mut set = std::collections::BTreeSet::new();
    let mut shape_into = |s: &str| {
        let mut buf = rustybuzz::UnicodeBuffer::new();
        buf.push_str(s);
        buf.guess_segment_properties();
        let out = rustybuzz::shape(face, &[], buf);
        for g in out.glyph_infos() {
            if g.glyph_id != 0 {
                set.insert(g.glyph_id as u16);
            }
        }
    };
    let ascii: String = (0x20u8..0x7f).map(|c| c as char).collect();
    shape_into(&ascii);
    // Latin pairs (script fonts have contextual alternates)
    let latin: Vec<char> = ('a'..='z').chain('A'..='Z').collect();
    for &a in &latin {
        for b in 'a'..='z' {
            shape_into(&format!("{a}{b}"));
        }
    }
    for w in [
        "fi fl ff ffi Th",
        "«»“”‘’–—…•€£°±×÷",
        "٠١٢٣٤٥٦٧٨٩ ۰۱۲۳۴۵۶۷۸۹",
        "،؛؟٪",
    ] {
        shape_into(w);
    }
    // French (and common Western European) accents, each on its own and
    // after a capital / small letter (contextual forms of script fonts)
    for c in FRENCH.chars() {
        shape_into(&c.to_string());
        shape_into(&format!("a{c}"));
    }
    // Tifinagh (Neo-Tifinagh / IRCAM letters, labialisation mark)
    if face.glyph_index('\u{2D30}').is_some() {
        let tfng: String = (0x2D30u32..=0x2D7F).filter_map(char::from_u32).collect();
        shape_into(&tfng);
        shape_into("ⵜⴰⵎⴰⵣⵉⵖⵜ ⴰⵣⵓⵍ ⵜⴰⵔⵣ");
    }
    let arabic_letters: Vec<char> = (0x0621u32..=0x064A)
        .chain([
            0x0671, 0x067E, 0x0686, 0x0698, 0x06A4, 0x06A9, 0x06AF, 0x06BE, 0x06C1, 0x06CC, 0x06D2,
        ])
        .filter_map(char::from_u32)
        .filter(|&c| face.glyph_index(c).is_some() && c != bidi::TATWEEL)
        .collect();
    if arabic_letters.is_empty() {
        return set.into_iter().collect();
    }
    let t = bidi::TATWEEL;
    for &a in &arabic_letters {
        for s in [
            format!("{a}"),
            format!("{t}{a}"),
            format!("{a}{t}"),
            format!("{t}{a}{t}"),
        ] {
            shape_into(&s);
        }
        for m in [
            '\u{064B}', '\u{064C}', '\u{064D}', '\u{064E}', '\u{064F}', '\u{0650}', '\u{0651}',
            '\u{0652}', '\u{0670}',
        ] {
            shape_into(&format!("{t}{a}{m}{t}"));
            shape_into(&format!("{a}\u{0651}{m}"));
        }
        for &b in &arabic_letters {
            for s in [
                format!("{a}{b}"),
                format!("{t}{a}{b}{t}"),
                format!("{t}{a}{b}"),
                format!("{a}{b}{t}"),
            ] {
                shape_into(&s);
            }
        }
    }
    for w in [
        "الله",
        "لله",
        "بسم الله الرحمن الرحيم",
        "محمد",
        "صلى الله عليه وسلم",
        "ﷺ",
        "ﷲ",
    ] {
        shape_into(w);
    }
    set.into_iter().collect()
}

/// Convert a whole font (corpus glyphs) to `.tzf`.
pub fn convert_font(font_id: &str, data: &[u8], arabic: bool) -> Result<TzfFont> {
    let face = rustybuzz::Face::from_slice(data, 0)
        .ok_or_else(|| Error::Font(format!("cannot parse font '{}'", font_id)))?;
    let ref_units = ref_height_units(&face, arabic);
    let glyphs = corpus_glyphs(&face)
        .into_iter()
        .map(|g| convert_glyph(&face, g, ref_units))
        .collect();
    Ok(TzfFont {
        version: TZF_VERSION,
        font_id: font_id.to_string(),
        units_per_em: face.units_per_em() as u16,
        ref_units,
        ref_size_mm: TZF_REF_SIZE_MM,
        max_satin_mm: MAX_SATIN_MM,
        glyphs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn points_roundtrip() {
        let pts = vec![
            Point::new(10.0, -5.0),
            Point::new(12.0, 3.0),
            Point::new(-4.0, 0.0),
        ];
        assert_eq!(decode_points(&encode_points(&pts)), pts);
    }

    #[test]
    fn file_roundtrip_and_rejects_garbage() {
        let f = TzfFont {
            version: TZF_VERSION,
            font_id: "x".into(),
            units_per_em: 1000,
            ref_units: 700.0,
            ref_size_mm: TZF_REF_SIZE_MM,
            max_satin_mm: MAX_SATIN_MM,
            glyphs: vec![TzfGlyph {
                gid: 3,
                pieces: vec![TzfPiece::Run {
                    pts: vec![1, 2, 3, 4],
                }],
                left: Some([0, 0]),
                right: None,
            }],
        };
        let bytes = f.encode().unwrap();
        assert_eq!(&bytes[..4], TZF_MAGIC);
        assert_eq!(TzfFont::decode(&bytes).unwrap(), f);
        assert!(TzfFont::decode(b"TZF1garbage").is_err());
        assert!(TzfFont::decode(b"nope").is_err());
    }
}
