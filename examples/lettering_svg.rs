//! Mixed Arabic + Latin lettering → stitch shapes → SVG.
//!
//! ```text
//! cargo run --example lettering_svg -- "طرز Tarz" amiri lettering.svg
//! ```
//!
//! Writes an SVG with the merged outline (light grey), satin columns
//! (rails + rungs), fill regions and running stitches, and prints the
//! lettering report (warnings, converted glyphs, enlarged dots…).

use tarz_text::design::{TextLayout, TextShape};
use tarz_text::geom::{PathData, Point};
use tarz_text::{FontRegistry, TextStitchShape, glyph_objects_report, render};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let text = args.next().unwrap_or_else(|| "طرز Tarz 2026".to_string());
    let font = args.next().unwrap_or_else(|| "amiri".to_string());
    let out = args.next().unwrap_or_else(|| "lettering.svg".to_string());

    let fonts = FontRegistry::bundled();
    let shape = TextShape {
        text,
        font,
        size_mm: 20.0,
        layout: TextLayout::Straight,
        ..Default::default()
    };

    // 1. merged closed outlines (mm, y down, centred on the origin)
    let outlines = render(&shape, &fonts)?;
    // 2. pre-digitized stitch shapes in sew order
    let result = glyph_objects_report(&shape, &fonts)?;

    let mut satin = 0;
    let mut fill = 0;
    let mut run = 0;
    let mut body = String::new();
    body.push_str(&format!(
        r##"<path d="{}" fill="#e6e6e6" fill-rule="evenodd"/>"##,
        outlines.path.to_svg()
    ));
    for o in &result.objects {
        match &o.shape {
            TextStitchShape::Satin {
                rail_a,
                rail_b,
                rungs,
            } => {
                satin += 1;
                for rail in [rail_a, rail_b] {
                    body.push_str(&format!(
                        r##"<path d="{}" fill="none" stroke="#1565c0" stroke-width="0.12"/>"##,
                        rail.to_svg()
                    ));
                }
                for (a, b) in rungs {
                    body.push_str(&line(*a, *b, "#90caf9", 0.06));
                }
            }
            TextStitchShape::Fill { region } => {
                fill += 1;
                body.push_str(&format!(
                    r##"<path d="{}" fill="#a5d6a7" fill-opacity="0.7" stroke="#2e7d32" stroke-width="0.1"/>"##,
                    region.to_path().to_svg()
                ));
            }
            TextStitchShape::Run { path } => {
                run += 1;
                body.push_str(&format!(
                    r##"<path d="{}" fill="none" stroke="#c62828" stroke-width="0.15"/>"##,
                    path.to_svg()
                ));
            }
        }
    }

    let b = bounds(&outlines.path, &result.objects);
    let pad = 2.0;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="{:.2} {:.2} {:.2} {:.2}" width="{:.0}mm" height="{:.0}mm">{body}</svg>"#,
        b.0 - pad,
        b.1 - pad,
        b.2 - b.0 + 2.0 * pad,
        b.3 - b.1 + 2.0 * pad,
        b.2 - b.0 + 2.0 * pad,
        b.3 - b.1 + 2.0 * pad,
    );
    std::fs::write(&out, svg)?;

    let r = &result.report;
    println!("font: {} (fell back: {})", r.font, r.fell_back);
    println!(
        "objects: {} (satin {satin}, fill {fill}, run {run}); regions: {}",
        result.objects.len(),
        outlines.regions.len()
    );
    println!(
        "joined_to_next: {} of {}",
        result.objects.iter().filter(|o| o.joined_to_next).count(),
        result.objects.len()
    );
    println!(
        "converted glyphs: {}, widened rungs: {}, enlarged dots: {}",
        r.converted_glyphs, r.widened_rungs, r.enlarged_dots
    );
    for w in &r.warnings {
        println!("warning: {w}");
    }
    println!("wrote {out}");
    Ok(())
}

fn line(a: Point, b: Point, color: &str, w: f64) -> String {
    format!(
        r#"<line x1="{:.3}" y1="{:.3}" x2="{:.3}" y2="{:.3}" stroke="{color}" stroke-width="{w}"/>"#,
        a.x, a.y, b.x, b.y
    )
}

fn bounds(path: &PathData, objects: &[tarz_text::TextStitchObject]) -> (f64, f64, f64, f64) {
    let mut r = path.bounds();
    for o in objects {
        let b = match &o.shape {
            TextStitchShape::Satin { rail_a, rail_b, .. } => {
                rail_a.bounds().union(&rail_b.bounds())
            }
            TextStitchShape::Fill { region } => region.bounds(),
            TextStitchShape::Run { path } => path.bounds(),
        };
        r = r.union(&b);
    }
    if r.is_empty() {
        (0.0, 0.0, 1.0, 1.0)
    } else {
        (r.min_x, r.min_y, r.max_x, r.max_y)
    }
}
