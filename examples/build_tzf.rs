//! Convert a TrueType/OpenType font to a `.tzf` embroidery font
//! (pre-digitized satin/fill pieces + connection anchors).
//!
//! ```text
//! cargo run --release --example build_tzf -- assets/fonts/Oswald-Medium.ttf oswald.tzf
//! cargo run --release --example build_tzf -- MyArabicFont.ttf my-font.tzf --arabic
//! ```
//!
//! `--arabic` sizes the font by its isolated alef (the Arabic size rule);
//! without it the capital height is used.

use tarz_text::tzf::convert_font;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let arabic = args.iter().any(|a| a == "--arabic");
    let pos: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let (Some(input), Some(output)) = (pos.first(), pos.get(1)) else {
        eprintln!("usage: build_tzf <font.ttf> <out.tzf> [--arabic]");
        std::process::exit(2);
    };
    let data = std::fs::read(input)?;
    let id = std::path::Path::new(output.as_str())
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("font")
        .to_string();
    let t = std::time::Instant::now();
    let font = convert_font(&id, &data, arabic)?;
    let bytes = font.encode()?;
    std::fs::write(output.as_str(), &bytes)?;
    println!(
        "{input} -> {output}: {} glyphs, {} bytes, {:.1} s",
        font.glyphs.len(),
        bytes.len(),
        t.elapsed().as_secs_f64()
    );
    Ok(())
}
