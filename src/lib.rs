#![forbid(unsafe_code)]
//! Text to outlines: font registry (bundled embroidery fonts), shaping
//! (Arabic via rustybuzz), layout (straight / arc / path), Arabic join
//! overlap and minimum stroke rules.
//!
//! CONTRACT:
//! - `FontRegistry::bundled()` loads all bundled fonts.
//! - `outline(shape, registry)` returns closed outlines in mm, positioned
//!   so the text block is centred on the origin (object transform places it).
//!
//! # Size rule
//! `TextShape::size_mm` is the height of the letter body:
//! - text containing Arabic letters: the height above the baseline of the
//!   isolated alef `ا` of the font (the classic Arabic "nuqta" measure of a
//!   letter body; ascenders such as lam are about the same height, descenders
//!   and harakat extend beyond it);
//! - other text: the font's capital height (OS/2 `sCapHeight`, or the height
//!   of `H` when the table lacks it).
//!
//! A fallback font used for a run of another script gets its own scale by
//! the same rule, so mixed Arabic/Latin lines line up visually.
//!
//! # Embroidery rules
//! Applied in this order on each glyph cluster (all distances in mm):
//! 1. `bold`: outward offset of every glyph outline.
//! 2. minimum column width: when the thinnest strokes of the text are below
//!    [`MIN_COLUMN_MM`], every outline is thickened (max [`MAX_THICKEN_MM`]).
//! 3. small parts (dots, harakat) below [`MIN_PART_AREA_MM2`] are scaled up
//!    around their centroid (never dropped).
//! 4. join overlap: for each pair of Arabic letters that join, the region
//!    within `join_overlap / 2` of both letters is added to the first one, so
//!    joined letters always form one connected region even when the font
//!    only touches at the join.
//!
//! Layout `Path` keeps the coordinates of the given path (the text sits on
//! it); `Straight` and `Arc` are centred on the origin.
//!
//! # Example
//! ```
//! use tarz_text::design::TextShape;
//! use tarz_text::{FontRegistry, TextStitchShape};
//!
//! let fonts = FontRegistry::bundled();
//! let shape = TextShape {
//!     text: "طرز Tarz".into(),
//!     font: "amiri".into(),
//!     size_mm: 20.0,
//!     ..Default::default()
//! };
//! // closed outlines in mm (y down), centred on the origin
//! let outline = tarz_text::outline(&shape, &fonts).unwrap();
//! assert!(!outline.is_empty());
//! // pre-digitized stitch shapes in sew order
//! let result = tarz_text::glyph_objects_report(&shape, &fonts).unwrap();
//! assert!(result
//!     .objects
//!     .iter()
//!     .any(|o| matches!(o.shape, TextStitchShape::Satin { .. })));
//! ```

pub mod design;
pub mod error;
pub mod geom;

pub use error::{Error, Result};

mod bidi;
mod fonts;
mod layout;
pub mod quality;
mod region;
mod runtime;
mod skeleton;
mod stitch;
pub mod tzf;

pub use bidi::{Run, has_arabic, insert_kashida, paragraph_rtl, visual_runs};
pub use runtime::{
    MAX_OUTLINE_BYTES, MAX_RUNTIME_FONTS, MAX_TZF_BYTES, MAX_TZF_INFLATED, RuntimeFontMeta,
    STORE_PREFIX, store_font_id,
};

use crate::design::TextShape;
use crate::geom::{MultiPolygon, PathData, Point};

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

/// Thinnest sewable column of an outline (mm).
pub const MIN_COLUMN_MM: f64 = 1.0;
/// Maximum automatic thickening (mm, each side).
pub const MAX_THICKEN_MM: f64 = 0.3;
/// Minimum diameter of dots and diacritics (mm).
pub const MIN_DOT_MM: f64 = 1.2;
/// Dots and harakat smaller than this (mm², a 1.2 mm disc) are enlarged.
pub const MIN_PART_AREA_MM2: f64 = std::f64::consts::PI * 0.36;
/// Narrowest satin column of pre-digitized letters (mm).
pub const MIN_SATIN_MM: f64 = 1.0;
/// Widest satin column; wider strokes are fills (mm).
pub const MAX_SATIN_MM: f64 = 8.0;
/// Text smaller than this triggers a warning (mm).
pub const SMALL_TEXT_MM: f64 = 6.0;
/// Default Arabic join overlap (mm). `TextShape::join_overlap`
/// carries the per-object value.
pub use crate::design::DEFAULT_JOIN_OVERLAP_MM;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FontInfo {
    pub id: String,
    pub name: String,
    /// "arabic", "latin", "both" or "tifinagh" (Tifinagh with or without
    /// Latin letters).
    pub script: String,
    /// Calligraphic style label (Naskh, Kufi, Ruqaa, Diwani, Serif, Sans, Script…).
    pub style: String,
    /// Licence of the font asset.
    pub license: String,
    /// Application-defined flag (Tarz Studio: usable in the free edition).
    pub free: bool,
    /// Recommended minimum letter height in mm.
    pub min_size_mm: f64,
    /// Where the font comes from: empty for bundled fonts, `"store"` for
    /// fonts registered at runtime ([`FontRegistry::register_tzf`]).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
}

impl FontInfo {
    pub fn supports_arabic(&self) -> bool {
        self.script == "arabic" || self.script == "both"
    }
    pub fn supports_latin(&self) -> bool {
        self.script == "latin" || self.script == "both"
    }
}

/// Font collection. Font data is parsed lazily on first use.
#[derive(Default)]
pub struct FontRegistry {
    pub fonts: Vec<FontInfo>,
    data: Vec<&'static [u8]>,
    faces: Vec<OnceLock<Option<rustybuzz::Face<'static>>>>,
    tzf_data: Vec<&'static [u8]>,
    tzf: Vec<OnceLock<Option<tzf::TzfFont>>>,
}

impl FontRegistry {
    pub fn bundled() -> Self {
        let mut r = FontRegistry::default();
        for b in fonts::BUNDLED {
            r.register(
                FontInfo {
                    id: b.id.into(),
                    name: b.name.into(),
                    script: b.script.into(),
                    style: b.style.into(),
                    license: "OFL-1.1".into(),
                    free: b.free,
                    min_size_mm: b.min_size_mm,
                    source: String::new(),
                },
                b.data,
            );
            r.tzf_data[r.fonts.len() - 1] = b.tzf;
        }
        r
    }
    pub fn empty() -> Self {
        FontRegistry::default()
    }
    pub fn list(&self) -> Vec<FontInfo> {
        self.fonts.clone()
    }
    /// Add a font (TrueType/OpenType bytes). The data must outlive the
    /// registry; leak a `Vec<u8>` (`Box::leak`) for runtime-loaded fonts.
    pub fn register(&mut self, info: FontInfo, data: &'static [u8]) {
        self.register_with_tzf(info, data, &[]);
    }
    /// Add a font together with its pre-digitized `.tzf` glyphs.
    pub fn register_with_tzf(&mut self, info: FontInfo, data: &'static [u8], tzf: &'static [u8]) {
        self.fonts.push(info);
        self.data.push(data);
        self.faces.push(OnceLock::new());
        self.tzf_data.push(tzf);
        self.tzf.push(OnceLock::new());
    }
    /// Raw font bytes of font `idx` (TrueType/OpenType).
    pub fn font_data(&self, idx: usize) -> Option<&'static [u8]> {
        self.data.get(idx).copied()
    }
    /// Pre-digitized glyphs of font `idx` (None when no valid `.tzf`).
    pub fn tzf(&self, idx: usize) -> Option<&tzf::TzfFont> {
        let data = *self.tzf_data.get(idx)?;
        self.tzf[idx]
            .get_or_init(|| {
                if data.is_empty() {
                    None
                } else {
                    tzf::TzfFont::decode(data).ok()
                }
            })
            .as_ref()
    }
    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.fonts.iter().position(|f| f.id == id)
    }
    pub fn info(&self, id: &str) -> Option<&FontInfo> {
        self.fonts.iter().find(|f| f.id == id)
    }
    /// Parsed face of font `idx` (None when the data is not a valid font).
    pub(crate) fn face(&self, idx: usize) -> Option<&rustybuzz::Face<'static>> {
        let data = *self.data.get(idx)?;
        self.faces[idx]
            .get_or_init(|| rustybuzz::Face::from_slice(data, 0))
            .as_ref()
    }

    /// Font to use for `id` and `text`. Unknown ids, and Latin-only fonts
    /// asked to set Arabic text, fall back to the first font of the right
    /// script; `fell_back` is then true so the UI can warn.
    pub fn resolve(&self, id: &str, text: &str) -> (String, bool) {
        let arabic = has_arabic(text);
        if let Some(f) = self.info(id)
            && (!arabic || f.supports_arabic())
        {
            return (f.id.clone(), false);
        }
        // Latin text prefers a dedicated Latin font over a bilingual one.
        let pick = if arabic {
            self.fonts.iter().find(|f| f.supports_arabic())
        } else {
            self.fonts
                .iter()
                .find(|f| f.script == "latin")
                .or_else(|| self.fonts.iter().find(|f| f.supports_latin()))
        }
        .or(self.fonts.first());
        match pick {
            Some(f) => (f.id.clone(), true),
            None => (id.to_string(), true),
        }
    }
}

/// One sewable piece of lettering: a base letter cluster (with the join
/// overlap towards the next letter) or the marks of a cluster.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextRegion {
    pub region: MultiPolygon,
    /// Characters of the cluster (after kashida insertion).
    pub text: String,
    pub line: usize,
    /// Word index over the whole text (logical order).
    pub word: usize,
    /// Harakat / combining marks (sewn after the base letters of the word).
    pub is_mark: bool,
    /// Cluster belongs to a right-to-left run.
    pub rtl: bool,
}

/// Full result of laying out a text object.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TextOutlines {
    /// Regions in sew order: lines top to bottom, words in reading order,
    /// letters in reading order (right to left for Arabic), marks after the
    /// base letters of their word.
    pub regions: Vec<TextRegion>,
    /// Merged outline (closed sub-paths, non-overlapping). Glyphs that were
    /// not modified keep their quadratic/cubic curves.
    pub path: PathData,
    /// Font actually used.
    pub font: String,
    /// The requested font was unknown or unsuitable.
    pub fell_back: bool,
    /// Automatic thickening applied (mm per side) for the minimum column rule.
    pub thickened_mm: f64,
    /// Number of small parts that were enlarged.
    pub enlarged_parts: usize,
    /// Human-readable warnings (codes before the colon).
    pub warnings: Vec<String>,
    /// Where the layout origin ended up after centring: for arc text this is
    /// the centre of the circle (object coordinates).
    pub origin: Point,
}

/// One pre-digitized lettering piece.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TextStitchShape {
    /// Satin column between two rails (same number of points; `rungs` pair
    /// them and pin the stitch direction).
    Satin {
        rail_a: PathData,
        rail_b: PathData,
        rungs: Vec<(crate::geom::Point, crate::geom::Point)>,
    },
    /// Fill (tatami) region.
    Fill { region: MultiPolygon },
    /// Running stitch.
    Run { path: PathData },
}

/// A pre-digitized piece of a letter, in final position (mm, y down, same
/// placement as [`outline`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TextStitchObject {
    pub shape: TextStitchShape,
    /// Where sewing of this piece starts / ends.
    pub entry: crate::geom::Point,
    pub exit: crate::geom::Point,
    /// The next object belongs to the same connected letter body (another
    /// piece of this letter that touches it, possibly through other pieces,
    /// or the next letter across an Arabic join): connect `exit` → next
    /// `entry` without a trim, travelling inside the body (under the
    /// stitches).
    pub joined_to_next: bool,
    /// Characters of the cluster this piece belongs to.
    pub text: String,
    pub line: usize,
    pub word: usize,
    pub is_mark: bool,
    pub rtl: bool,
}

/// Warnings and statistics of a lettering object.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TextReport {
    /// `code: message` strings, e.g. `text_too_small: …`.
    pub warnings: Vec<String>,
    pub font: String,
    pub fell_back: bool,
    /// Glyphs digitized on the fly (not in the font's `.tzf`).
    pub converted_glyphs: usize,
    /// Satin rungs widened to the minimum column width.
    pub widened_rungs: usize,
    /// Dots / harakat enlarged to the minimum diameter.
    pub enlarged_dots: usize,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct TextStitchResult {
    pub objects: Vec<TextStitchObject>,
    pub report: TextReport,
}

/// Lay out and outline a text object (all rules applied).
pub fn render(shape: &TextShape, fonts: &FontRegistry) -> Result<TextOutlines> {
    let (_, built) = prepare(shape, fonts)?;
    Ok(built.out)
}

fn warnings_for(
    shape: &TextShape,
    fonts: &FontRegistry,
    font: &str,
    fell_back: bool,
) -> Vec<String> {
    let mut w = Vec::new();
    if fell_back {
        w.push(format!(
            "font_fallback: font '{}' unavailable for this text, using '{}'",
            shape.font, font
        ));
    }
    if shape.size_mm < SMALL_TEXT_MM && !shape.text.trim().is_empty() {
        w.push(format!(
            "text_too_small: {:.1} mm text is below {:.0} mm; small letters and harakat may not sew cleanly",
            shape.size_mm, SMALL_TEXT_MM
        ));
    }
    if let Some(info) = fonts.info(font)
        && shape.size_mm < info.min_size_mm
        && shape.size_mm >= SMALL_TEXT_MM
    {
        w.push(format!(
            "below_font_min_size: {} is recommended at {:.0} mm or more",
            info.name, info.min_size_mm
        ));
    }
    w
}

fn prepare(shape: &TextShape, fonts: &FontRegistry) -> Result<(layout::Layout, region::Built)> {
    if !(shape.size_mm.is_finite() && shape.size_mm > 0.0) {
        return Err(Error::Invalid(format!(
            "text size must be positive, got {}",
            shape.size_mm
        )));
    }
    if fonts.fonts.is_empty() {
        return Err(Error::Font("no fonts available".into()));
    }
    let (font_id, fell_back) = fonts.resolve(&shape.font, &shape.text);
    let primary = fonts
        .index_of(&font_id)
        .ok_or_else(|| Error::Font(format!("unknown font '{}'", shape.font)))?;
    if fonts.face(primary).is_none() {
        return Err(Error::Font(format!("font '{}' cannot be parsed", font_id)));
    }
    let lay = layout::layout(shape, fonts, primary)?;
    let mut built = region::build_full(shape, fonts, &lay)?;
    built.out.warnings = warnings_for(shape, fonts, &font_id, fell_back);
    built.out.font = font_id;
    built.out.fell_back = fell_back;
    Ok((lay, built))
}

/// Pre-digitized lettering pieces (satin columns, fills, runs) in sew order:
/// lines top to bottom, words in reading order, letters in reading order
/// (right to left for Arabic) with consecutive joined letters chained
/// exit → entry, marks after the base letters of their word. Uses the
/// font's `.tzf` glyphs (converted on the fly when missing).
pub fn glyph_objects(shape: &TextShape, fonts: &FontRegistry) -> Result<Vec<TextStitchObject>> {
    Ok(glyph_objects_report(shape, fonts)?.objects)
}

/// [`glyph_objects`] plus warnings (text below 6 mm, font fallback…).
pub fn glyph_objects_report(shape: &TextShape, fonts: &FontRegistry) -> Result<TextStitchResult> {
    let (lay, built) = prepare(shape, fonts)?;
    let mut report = TextReport {
        warnings: built.out.warnings.clone(),
        font: built.out.font.clone(),
        fell_back: built.out.fell_back,
        ..Default::default()
    };
    let objects = stitch::build_objects(shape, fonts, &lay, &built, &mut report);
    Ok(TextStitchResult { objects, report })
}

/// Merged closed outlines in mm (y down), centred on the origin.
pub fn outline(shape: &TextShape, fonts: &FontRegistry) -> Result<PathData> {
    Ok(render(shape, fonts)?.path)
}

/// Per-cluster regions in sew order (see [`TextOutlines::regions`]).
pub fn outline_regions(shape: &TextShape, fonts: &FontRegistry) -> Result<Vec<TextRegion>> {
    Ok(render(shape, fonts)?.regions)
}

/// Centre of the circle of an arc layout in object coordinates (the text
/// itself is centred on the origin). `None` for other layouts.
pub fn arc_center(shape: &TextShape, fonts: &FontRegistry) -> Result<Option<Point>> {
    if !matches!(shape.layout, crate::design::TextLayout::Arc { .. }) {
        return Ok(None);
    }
    Ok(Some(render(shape, fonts)?.origin))
}
