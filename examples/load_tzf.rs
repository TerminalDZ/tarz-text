//! Load a `.tzf` embroidery font with its outline font, inspect it, run the
//! quality checks and set text with it.
//!
//! ```text
//! cargo run --example load_tzf -- assets/fonts/amiri.tzf assets/fonts/Amiri-Regular.ttf "بسم الله"
//! ```

use std::io::Read;

use tarz_text::design::TextShape;
use tarz_text::tzf::{TZF_MAGIC, TzfFont, TzfPiece};
use tarz_text::{FontRegistry, RuntimeFontMeta, glyph_objects_report, quality};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let tzf_path = args
        .next()
        .unwrap_or_else(|| "assets/fonts/amiri.tzf".to_string());
    let ttf_path = args
        .next()
        .unwrap_or_else(|| "assets/fonts/Amiri-Regular.ttf".to_string());
    let text = args.next().unwrap_or_else(|| "بسم الله".to_string());

    let tzf = std::fs::read(&tzf_path)?;
    let ttf = std::fs::read(&ttf_path)?;

    // --- the file itself: b"TZF1" + deflate(JSON)
    let font = TzfFont::decode(&tzf)?;
    let mut json = Vec::new();
    flate2::read::DeflateDecoder::new(&tzf[TZF_MAGIC.len()..]).read_to_end(&mut json)?;
    let (mut satin, mut fill, mut run, mut anchored) = (0, 0, 0, 0);
    for g in &font.glyphs {
        if g.left.is_some() || g.right.is_some() {
            anchored += 1;
        }
        for p in &g.pieces {
            match p {
                TzfPiece::Satin { .. } => satin += 1,
                TzfPiece::Fill { .. } => fill += 1,
                TzfPiece::Run { .. } => run += 1,
            }
        }
    }
    println!("{tzf_path}");
    println!(
        "  font_id {}, version {}, units/em {}, ref {} units = {} mm, max satin {} mm",
        font.font_id,
        font.version,
        font.units_per_em,
        font.ref_units,
        font.ref_size_mm,
        font.max_satin_mm
    );
    println!(
        "  {} glyphs ({anchored} with connection anchors), pieces: satin {satin}, fill {fill}, run {run}",
        font.glyphs.len()
    );
    println!(
        "  {} bytes on disk, {} bytes of JSON inflated ({:.1}% smaller)",
        tzf.len(),
        json.len(),
        100.0 * (1.0 - tzf.len() as f64 / json.len() as f64)
    );

    // --- quality checks (validity, coverage, satin widths, joins, renders)
    let q = quality::check_font(&font.font_id, &tzf, &ttf);
    println!(
        "quality: ok={} script={} stitch={} recommended {}–{} mm",
        q.ok, q.script, q.stitch, q.min_size_mm, q.max_size_mm
    );
    if let Some(j) = &q.joins {
        println!("  arabic joins sewn without trim: {}/{}", j.ok, j.checked);
    }
    for e in &q.errors {
        println!("  error: {e}");
    }
    for w in &q.warnings {
        println!("  warning: {w}");
    }

    // --- register at runtime and set text with it
    let mut fonts = FontRegistry::empty();
    let info = fonts.register_tzf(
        &tzf,
        &ttf,
        &RuntimeFontMeta {
            key: font.font_id.clone(),
            ..Default::default()
        },
    )?;
    println!(
        "registered as '{}' ({}, {})",
        info.id, info.name, info.script
    );
    let res = glyph_objects_report(
        &TextShape {
            text: text.clone(),
            font: info.id.clone(),
            size_mm: 15.0,
            ..Default::default()
        },
        &fonts,
    )?;
    println!(
        "'{text}' at 15 mm: {} stitch objects, {} glyphs converted on the fly",
        res.objects.len(),
        res.report.converted_glyphs
    );
    Ok(())
}
