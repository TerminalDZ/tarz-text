//! The Quick start snippet of README.md, kept compiling and running.

use tarz_text::design::TextShape;
use tarz_text::{FontRegistry, TextStitchShape};

fn quick_start() -> tarz_text::Result<()> {
    let fonts = FontRegistry::bundled();
    let shape = TextShape {
        text: "طرز Tarz".into(),
        font: "amiri".into(),
        size_mm: 20.0, // Arabic: height of the isolated alef
        ..Default::default()
    };

    // Closed outlines in mm (y down), centred on the origin.
    let outline = tarz_text::outline(&shape, &fonts)?;
    println!("outline: {} sub-paths", outline.subpaths.len());
    let _svg_path_data = outline.to_svg();

    // Pre-digitized stitch shapes in sew order.
    let result = tarz_text::glyph_objects_report(&shape, &fonts)?;
    for obj in &result.objects {
        match &obj.shape {
            TextStitchShape::Satin { rungs, .. } => println!("satin, {} rungs", rungs.len()),
            TextStitchShape::Fill { region } => println!("fill, {:.1} mm²", region.area()),
            TextStitchShape::Run { .. } => println!("run"),
        }
    }
    for w in &result.report.warnings {
        println!("warning: {w}");
    }
    Ok(())
}

#[test]
fn readme_quick_start_runs() {
    quick_start().unwrap();
}

#[test]
fn bundled_fonts_have_licences() {
    let fonts = FontRegistry::bundled();
    assert_eq!(fonts.fonts.len(), 25);
    for f in &fonts.fonts {
        assert_eq!(f.license, "OFL-1.1");
    }
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fonts/licenses");
    assert_eq!(std::fs::read_dir(dir).unwrap().count(), 25);
}
