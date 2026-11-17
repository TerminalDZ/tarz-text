//! Runtime font registration: embroidery fonts bought or downloaded from a
//! font store (`.tzf` pre-digitized glyphs + the outline font they were
//! built from) become usable for lettering without rebuilding the engine.
//!
//! * Every input is untrusted: sizes are capped before parsing, the `.tzf`
//!   inflate is bounded ([`MAX_TZF_INFLATED`]), the glyph table must be
//!   sorted and unique with sane units, coordinates are counted, and the
//!   outline font must parse and match the `.tzf` (same units per em, every
//!   glyph id present).
//! * Ids are deterministic: `store:<key>` where `<key>` is the store listing
//!   id (or the `.tzf` `font_id`) lower-cased and reduced to
//!   `[a-z0-9_-]`, so a design saved with a store font names the same font
//!   on every computer.
//! * A design whose store font is not installed falls back to a bundled font
//!   of the right script with a `font_fallback` warning (see
//!   [`FontRegistry::resolve`]).
//!
//! Memory: the parsed outline font borrows its bytes for the life of the
//! process (`rustybuzz::Face<'static>`), so the bytes of each distinct font
//! file are kept once (interned by content); registering the same file again
//! (startup, re-install) reuses them. Re-installing a new version keeps the
//! old bytes until the process ends — bounded by [`MAX_RUNTIME_FONTS`] ×
//! ([`MAX_OUTLINE_BYTES`] + [`MAX_TZF_BYTES`]) in the worst case and a few
//! MB in practice. The `.tzf` is kept compressed and decoded on first use.

use super::tzf::{TZF_VERSION, TzfFont, TzfPiece};
use super::{FontInfo, FontRegistry, has_arabic};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::sync::{Mutex, OnceLock};

/// Largest accepted `.tzf` file (compressed bytes).
pub const MAX_TZF_BYTES: usize = 16 << 20;
/// Largest accepted inflated `.tzf` JSON.
pub const MAX_TZF_INFLATED: u64 = 64 << 20;
/// Largest accepted outline font (TrueType / OpenType).
pub const MAX_OUTLINE_BYTES: usize = 24 << 20;
/// Runtime fonts registered at the same time (store fonts plus the
/// a downloadable font library; their `.tzf` is decoded on first use).
pub const MAX_RUNTIME_FONTS: usize = 400;
/// Coordinates (numbers) a `.tzf` may hold in total.
const MAX_TZF_COORDS: usize = 12_000_000;
/// Id prefix of runtime fonts.
pub const STORE_PREFIX: &str = "store:";
/// Value of [`FontInfo::source`] for runtime fonts.
const SOURCE_STORE: &str = "store";

/// Describes a runtime font (all fields optional).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeFontMeta {
    /// Store listing id (or slug); defaults to the `.tzf` `font_id`.
    pub key: String,
    /// Display name; defaults to the family name of the outline font.
    pub name: String,
    /// Style label (Naskh, Kufi, Script…); defaults to "Store".
    pub style: String,
    /// Licence label (e.g. "personal", "commercial").
    pub license: String,
    /// Recommended minimum letter height (mm); defaults to 7 mm.
    pub min_size_mm: Option<f64>,
}

/// Deterministic id of a store font: `store:` + `key` lower-cased, every
/// character outside `[a-z0-9_-]` replaced by `-`, runs collapsed, at most
/// 64 characters. `None` when nothing usable is left.
pub fn store_font_id(key: &str) -> Option<String> {
    let key = key.trim();
    let key = key.strip_prefix(STORE_PREFIX).unwrap_or(key);
    let mut out = String::new();
    for c in key.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '-'
        };
        if c == '-' && (out.is_empty() || out.ends_with('-')) {
            continue;
        }
        if out.len() >= 64 {
            break;
        }
        out.push(c);
    }
    let out = out.trim_end_matches('-');
    (!out.is_empty()).then(|| format!("{STORE_PREFIX}{out}"))
}

fn bad(msg: impl Into<String>) -> Error {
    Error::Font(format!("invalid store font: {}", msg.into()))
}

/// Outline font bytes kept for the process lifetime, one copy per content.
fn intern(bytes: &[u8]) -> &'static [u8] {
    static POOL: OnceLock<Mutex<Vec<&'static [u8]>>> = OnceLock::new();
    let pool = POOL.get_or_init(|| Mutex::new(Vec::new()));
    let mut pool = match pool.lock() {
        Ok(p) => p,
        Err(p) => p.into_inner(),
    };
    if let Some(d) = pool.iter().find(|d| **d == bytes) {
        return d;
    }
    let leaked: &'static [u8] = Box::leak(bytes.to_vec().into_boxed_slice());
    pool.push(leaked);
    leaked
}

fn piece_coords(p: &TzfPiece) -> usize {
    match p {
        TzfPiece::Satin { a, b } => a.len() + b.len(),
        TzfPiece::Fill { rings } => rings.iter().map(Vec::len).sum(),
        TzfPiece::Run { pts } => pts.len(),
    }
}

/// Structural checks of a decoded `.tzf` (independent of the outline font).
fn check_tzf(f: &TzfFont) -> Result<()> {
    if f.version != TZF_VERSION {
        return Err(bad(format!("unsupported .tzf version {}", f.version)));
    }
    if !(16..=16384).contains(&f.units_per_em) {
        return Err(bad(format!("units per em {} out of range", f.units_per_em)));
    }
    let upm = f.units_per_em as f64;
    if !(f.ref_units.is_finite() && f.ref_units > 0.0 && f.ref_units <= upm * 4.0) {
        return Err(bad("reference height out of range"));
    }
    if !(f.ref_size_mm.is_finite() && f.ref_size_mm > 0.0)
        || !(f.max_satin_mm.is_finite() && f.max_satin_mm > 0.0)
    {
        return Err(bad("reference sizes out of range"));
    }
    if f.glyphs.is_empty() {
        return Err(bad("no glyphs"));
    }
    if f.glyphs.windows(2).any(|w| w[0].gid >= w[1].gid) {
        return Err(bad("glyph ids are not sorted and unique"));
    }
    let mut coords = 0usize;
    for g in &f.glyphs {
        for p in &g.pieces {
            let n = piece_coords(p);
            if !n.is_multiple_of(2) {
                return Err(bad(format!("glyph {} has an odd coordinate list", g.gid)));
            }
            coords = coords.saturating_add(n);
        }
    }
    if coords > MAX_TZF_COORDS {
        return Err(bad("too many coordinates"));
    }
    Ok(())
}

fn family_name(face: &rustybuzz::Face) -> Option<String> {
    use rustybuzz::ttf_parser::name_id;
    let names = face.names();
    [
        name_id::TYPOGRAPHIC_FAMILY,
        name_id::FULL_NAME,
        name_id::FAMILY,
    ]
    .iter()
    .find_map(|id| {
        names
            .into_iter()
            .filter(|n| n.name_id == *id && n.is_unicode())
            .find_map(|n| n.to_string())
    })
    .map(|s| {
        s.chars()
            .filter(|c| !c.is_control())
            .take(80)
            .collect::<String>()
    })
    .filter(|s| !s.trim().is_empty())
}

fn clean_label(s: &str, max: usize) -> String {
    s.chars()
        .filter(|c| !c.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

impl FontRegistry {
    /// Register a store font: its `.tzf` and the outline font (TTF/OTF) it
    /// was built from. Validated and size-capped (see the module docs);
    /// the id is `store:<key>` ([`store_font_id`]). Registering an id that
    /// already exists replaces that font (update). Bundled fonts are never
    /// replaced.
    pub fn register_tzf(
        &mut self,
        tzf: &[u8],
        outline: &[u8],
        meta: &RuntimeFontMeta,
    ) -> Result<FontInfo> {
        if tzf.len() > MAX_TZF_BYTES {
            return Err(bad(format!(
                "the .tzf is larger than {} MB",
                MAX_TZF_BYTES >> 20
            )));
        }
        if outline.is_empty() {
            return Err(bad("the outline font (.ttf / .otf) is missing"));
        }
        if outline.len() > MAX_OUTLINE_BYTES {
            return Err(bad(format!(
                "the outline font is larger than {} MB",
                MAX_OUTLINE_BYTES >> 20
            )));
        }
        let font =
            TzfFont::decode_limited(tzf, MAX_TZF_INFLATED).map_err(|e| bad(e.to_string()))?;
        check_tzf(&font)?;
        let key = if meta.key.trim().is_empty() {
            font.font_id.as_str()
        } else {
            meta.key.as_str()
        };
        let id = store_font_id(key).ok_or_else(|| bad("the font has no usable id"))?;
        let replacing = self.index_of(&id);
        if replacing.is_none() && self.runtime_count() >= MAX_RUNTIME_FONTS {
            return Err(bad(format!(
                "at most {MAX_RUNTIME_FONTS} store fonts can be loaded"
            )));
        }

        // Parse a private copy first; only a valid font is interned.
        let probe = rustybuzz::Face::from_slice(outline, 0)
            .ok_or_else(|| bad("the outline font cannot be parsed"))?;
        if probe.units_per_em() as u16 != font.units_per_em {
            return Err(bad(
                "the .tzf and the outline font have different units per em",
            ));
        }
        let glyphs = probe.number_of_glyphs();
        if let Some(g) = font.glyphs.iter().find(|g| g.gid >= glyphs) {
            return Err(bad(format!("glyph {} is not in the outline font", g.gid)));
        }
        let arabic =
            probe.glyph_index('\u{0628}').is_some() && probe.glyph_index('\u{0627}').is_some();
        let latin = probe.glyph_index('A').is_some() && probe.glyph_index('a').is_some();
        // Tifinagh (Amazigh): YA ⴰ and YAZ ⵣ
        let tifinagh =
            probe.glyph_index('\u{2D30}').is_some() && probe.glyph_index('\u{2D63}').is_some();
        let script = match (arabic, latin, tifinagh) {
            (true, true, _) => "both",
            (true, false, _) => "arabic",
            (false, _, true) => "tifinagh",
            (false, true, false) => "latin",
            (false, false, false) => {
                return Err(bad(
                    "the font has neither Arabic, Latin nor Tifinagh letters",
                ));
            }
        };
        let name = match clean_label(&meta.name, 80) {
            n if !n.is_empty() => n,
            _ => family_name(&probe).unwrap_or_else(|| clean_label(&font.font_id, 80)),
        };
        let name = if name.is_empty() { id.clone() } else { name };
        drop(probe);

        let data = intern(outline);
        let face = rustybuzz::Face::from_slice(data, 0)
            .ok_or_else(|| bad("the outline font cannot be parsed"))?;
        let style = match clean_label(&meta.style, 60) {
            s if !s.is_empty() => s,
            _ => "Store".to_string(),
        };
        let license = match clean_label(&meta.license, 40) {
            s if !s.is_empty() => s,
            _ => SOURCE_STORE.to_string(),
        };
        let info = FontInfo {
            id: id.clone(),
            name,
            script: script.into(),
            style,
            license,
            // runtime fonts are always marked free (see `FontInfo::free`)
            free: true,
            min_size_mm: meta
                .min_size_mm
                .filter(|v| v.is_finite())
                .unwrap_or(7.0)
                .clamp(3.0, 50.0),
            source: SOURCE_STORE.into(),
        };
        if let Some(i) = replacing {
            if self.fonts[i].source != SOURCE_STORE {
                return Err(bad("a bundled font has this id"));
            }
            self.remove_at(i);
        }
        // Keep only the compressed `.tzf` (a few hundred KB) and decode it on
        // first use: a large installed library stays small in memory.
        drop(font);
        let tzf_bytes = intern(tzf);
        self.fonts.push(info.clone());
        self.data.push(data);
        self.faces.push(OnceLock::from(Some(face)));
        self.tzf_data.push(tzf_bytes);
        self.tzf.push(OnceLock::new());
        Ok(info)
    }

    /// Remove a runtime (store) font. Bundled fonts cannot be removed.
    /// Returns whether a font was removed.
    pub fn unregister(&mut self, id: &str) -> bool {
        match self.index_of(id) {
            Some(i) if self.fonts[i].source == SOURCE_STORE => {
                self.remove_at(i);
                true
            }
            _ => false,
        }
    }

    /// Fonts registered at runtime, in registration order.
    pub fn runtime_fonts(&self) -> Vec<FontInfo> {
        self.fonts
            .iter()
            .filter(|f| f.source == SOURCE_STORE)
            .cloned()
            .collect()
    }

    fn runtime_count(&self) -> usize {
        self.fonts
            .iter()
            .filter(|f| f.source == SOURCE_STORE)
            .count()
    }

    fn remove_at(&mut self, i: usize) {
        self.fonts.remove(i);
        self.data.remove(i);
        self.faces.remove(i);
        self.tzf_data.remove(i);
        self.tzf.remove(i);
    }

    /// Whether `text` set in font `id` would fall back to another font
    /// (unknown id, or a Latin-only font asked for Arabic).
    pub fn would_fall_back(&self, id: &str, text: &str) -> bool {
        match self.info(id) {
            Some(f) => has_arabic(text) && !f.supports_arabic(),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::design::TextShape;

    fn amiri() -> (Vec<u8>, &'static [u8]) {
        bundled("amiri")
    }

    fn bundled(id: &str) -> (Vec<u8>, &'static [u8]) {
        let b = FontRegistry::bundled();
        let i = b.index_of(id).unwrap();
        (b.tzf(i).unwrap().encode().unwrap(), b.font_data(i).unwrap())
    }

    fn meta(key: &str) -> RuntimeFontMeta {
        RuntimeFontMeta {
            key: key.into(),
            name: "My Amiri".into(),
            ..Default::default()
        }
    }

    #[test]
    fn ids_are_deterministic_and_clean() {
        assert_eq!(store_font_id("Abc 123").unwrap(), "store:abc-123");
        assert_eq!(store_font_id("store:x__y").unwrap(), "store:x__y");
        assert_eq!(store_font_id("0b9f…/../العربية").unwrap(), "store:0b9f");
        assert!(store_font_id("  ").is_none());
        assert!(store_font_id("///").is_none());
        assert!(store_font_id(&"a".repeat(500)).unwrap().len() <= 70);
    }

    #[test]
    fn register_list_use_and_unregister() {
        let (tzf, ttf) = amiri();
        let mut r = FontRegistry::bundled();
        let n = r.fonts.len();
        let info = r.register_tzf(&tzf, ttf, &meta("L-42")).unwrap();
        assert_eq!(info.id, "store:l-42");
        assert_eq!(info.name, "My Amiri");
        assert_eq!(info.script, "both");
        assert_eq!(info.source, "store");
        assert!(info.free);
        assert_eq!(r.fonts.len(), n + 1);
        assert_eq!(r.runtime_fonts(), vec![info.clone()]);
        // usable for lettering, no fallback
        let shape = TextShape {
            text: "طرز".into(),
            font: info.id.clone(),
            size_mm: 20.0,
            ..Default::default()
        };
        let res = crate::glyph_objects_report(&shape, &r).unwrap();
        assert!(!res.report.fell_back, "{:?}", res.report.warnings);
        assert_eq!(res.report.font, "store:l-42");
        assert!(!res.objects.is_empty());
        // same pieces as the bundled original
        let orig = crate::glyph_objects(
            &TextShape {
                font: "amiri".into(),
                ..shape.clone()
            },
            &r,
        )
        .unwrap();
        assert_eq!(orig, res.objects);
        // re-register = update in place (no duplicate)
        r.register_tzf(&tzf, ttf, &meta("l-42")).unwrap();
        assert_eq!(r.fonts.len(), n + 1);
        assert!(r.unregister("store:l-42"));
        assert!(!r.unregister("store:l-42"));
        assert!(!r.unregister("amiri"), "bundled fonts stay");
        assert_eq!(r.fonts.len(), n);
        // missing store font: graceful fallback with a warning
        let res = crate::glyph_objects_report(&shape, &r).unwrap();
        assert!(res.report.fell_back);
        assert!(
            res.report
                .warnings
                .iter()
                .any(|w| w.starts_with("font_fallback"))
        );
        assert!(r.would_fall_back("store:l-42", "x"));
        assert!(!r.would_fall_back("amiri", "x"));
    }

    #[test]
    fn default_key_and_name_come_from_the_files() {
        let (tzf, ttf) = amiri();
        let mut r = FontRegistry::empty();
        let info = r
            .register_tzf(&tzf, ttf, &RuntimeFontMeta::default())
            .unwrap();
        assert_eq!(info.id, "store:amiri");
        assert!(info.name.to_lowercase().contains("amiri"), "{}", info.name);
        assert_eq!(info.min_size_mm, 7.0);
    }

    #[test]
    fn rejects_bad_input() {
        let (tzf, ttf) = amiri();
        let mut r = FontRegistry::empty();
        let m = meta("x");
        assert!(r.register_tzf(b"nope", ttf, &m).is_err());
        assert!(r.register_tzf(&tzf, &[], &m).is_err());
        assert!(r.register_tzf(&tzf, b"not a font", &m).is_err());
        assert!(
            r.register_tzf(&vec![0u8; MAX_TZF_BYTES + 1], ttf, &m)
                .is_err()
        );
        // another font's outline: units per em or glyph ids do not match
        let b = FontRegistry::bundled();
        let oswald = b.font_data(b.index_of("oswald").unwrap()).unwrap();
        assert!(r.register_tzf(&tzf, oswald, &m).is_err());
        // unsorted glyph table
        let mut f = TzfFont::decode(&tzf).unwrap();
        f.glyphs.swap(0, 1);
        assert!(r.register_tzf(&f.encode().unwrap(), ttf, &m).is_err());
        // decompression bomb (inflates far beyond the cap)
        let mut bomb = crate::tzf::TZF_MAGIC.to_vec();
        {
            use std::io::Write;
            let mut enc =
                flate2::write::DeflateEncoder::new(&mut bomb, flate2::Compression::best());
            let zeros = vec![b' '; 1 << 20];
            for _ in 0..(MAX_TZF_INFLATED >> 20) + 2 {
                enc.write_all(&zeros).unwrap();
            }
            enc.finish().unwrap();
        }
        assert!(bomb.len() < MAX_TZF_BYTES);
        let e = r.register_tzf(&bomb, ttf, &m).unwrap_err().to_string();
        assert!(e.contains("size limit"), "{e}");
        assert!(r.fonts.is_empty());
    }

    #[test]
    fn caps_the_number_of_runtime_fonts() {
        let (tzf, ttf) = bundled("graduate");
        let mut r = FontRegistry::empty();
        for i in 0..MAX_RUNTIME_FONTS {
            r.register_tzf(&tzf, ttf, &meta(&format!("f{i}"))).unwrap();
        }
        assert!(r.register_tzf(&tzf, ttf, &meta("one-more")).is_err());
        // updating an existing one is still fine
        assert!(r.register_tzf(&tzf, ttf, &meta("f3")).is_ok());
        assert_eq!(r.runtime_fonts().len(), MAX_RUNTIME_FONTS);
    }
}
