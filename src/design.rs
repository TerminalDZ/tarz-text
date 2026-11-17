//! Text object description: what to write, with which font, at which size
//! and along which layout.

use crate::geom::PathData;
use serde::{Deserialize, Serialize};

/// Default overlap added at Arabic letter joins (mm).
pub const DEFAULT_JOIN_OVERLAP_MM: f64 = 0.3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[derive(Default)]
pub enum TextLayout {
    #[default]
    Straight,
    /// Text along a circular arc. `radius_mm` > 0: text sits on top of the
    /// arc (reads clockwise); `inside`: baseline inside the circle (bottom arc).
    Arc { radius_mm: f64, inside: bool },
    /// Text along an arbitrary path (first sub-path is used).
    Path { path: PathData },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextShape {
    pub text: String,
    /// Font id from the font registry (e.g. "amiri", "reem-kufi").
    pub font: String,
    /// Height of capitals / Arabic letter body in mm.
    pub size_mm: f64,
    /// Extra spacing between letters (mm). Ignored inside joined Arabic words.
    pub letter_spacing: f64,
    /// Line spacing as a multiple of the font line height.
    pub line_spacing: f64,
    pub align: TextAlign,
    pub layout: TextLayout,
    pub bold: f64,
    /// Overlap added at Arabic joins so letters stay connected (mm).
    pub join_overlap: f64,
    /// Kashida (tatweel) elongation count for Arabic justification.
    pub kashida: u32,
}

impl Default for TextShape {
    fn default() -> Self {
        TextShape {
            text: String::new(),
            font: "amiri".into(),
            size_mm: 20.0,
            letter_spacing: 0.0,
            line_spacing: 1.2,
            align: TextAlign::Center,
            layout: TextLayout::Straight,
            bold: 0.0,
            join_overlap: DEFAULT_JOIN_OVERLAP_MM,
            kashida: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_shape_serde_defaults() {
        let s: TextShape = serde_json::from_str(r#"{"text":"abc"}"#).unwrap();
        assert_eq!(s.text, "abc");
        assert_eq!(s.font, "amiri");
        assert_eq!(s.join_overlap, DEFAULT_JOIN_OVERLAP_MM);
        let arc: TextLayout =
            serde_json::from_str(r#"{"type":"arc","radius_mm":40.0,"inside":false}"#).unwrap();
        assert_eq!(
            arc,
            TextLayout::Arc {
                radius_mm: 40.0,
                inside: false
            }
        );
    }
}
