//! Unicode support for embroidery lettering: Arabic joining types, bidi
//! runs (Unicode Bidirectional Algorithm via `unicode-bidi`, one paragraph
//! per line) and kashida (tatweel) insertion.

/// Arabic joining type (Unicode `ArabicShaping.txt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Joining {
    /// Non-joining.
    U,
    /// Right-joining: connects only to the previous (right-hand) letter.
    R,
    /// Dual-joining.
    D,
    /// Join-causing (tatweel, ZWJ).
    C,
    /// Transparent (combining marks).
    T,
}

pub const TATWEEL: char = '\u{0640}';

pub fn is_mark(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x0300..=0x036F
        | 0x0483..=0x0489
        | 0x0591..=0x05BD
        | 0x05BF | 0x05C1 | 0x05C2 | 0x05C4 | 0x05C5 | 0x05C7
        | 0x0610..=0x061A
        | 0x064B..=0x065F
        | 0x0670
        | 0x06D6..=0x06DC
        | 0x06DF..=0x06E4
        | 0x06E7 | 0x06E8
        | 0x06EA..=0x06ED
        | 0x08D3..=0x08E1
        | 0x08E3..=0x08FF
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF
        | 0xFE20..=0xFE2F)
}

pub fn joining(c: char) -> Joining {
    let u = c as u32;
    if is_mark(c) {
        return Joining::T;
    }
    match u {
        0x0640 | 0x200D => Joining::C,
        0x0622..=0x0625
        | 0x0627
        | 0x0629
        | 0x062F..=0x0632
        | 0x0648
        | 0x0671..=0x0673
        | 0x0675..=0x0677
        | 0x0688..=0x0699
        | 0x06C0
        | 0x06C3..=0x06CB
        | 0x06CD
        | 0x06CF
        | 0x06D2
        | 0x06D3
        | 0x06D5
        | 0x06EE
        | 0x06EF
        | 0x0759..=0x075B
        | 0x076B
        | 0x076C
        | 0x0771
        | 0x0773
        | 0x0774
        | 0x0778
        | 0x0779
        | 0x08AA..=0x08AC
        | 0x08AE
        | 0x08B1
        | 0x08B2
        | 0x08B9 => Joining::R,
        0x0620
        | 0x0626
        | 0x0628
        | 0x062A..=0x062E
        | 0x0633..=0x063F
        | 0x0641..=0x0647
        | 0x0649
        | 0x064A
        | 0x066E
        | 0x066F
        | 0x0678..=0x0687
        | 0x069A..=0x06BF
        | 0x06C1
        | 0x06C2
        | 0x06CC
        | 0x06CE
        | 0x06D0
        | 0x06D1
        | 0x06FA..=0x06FC
        | 0x06FF
        | 0x0750..=0x0758
        | 0x075C..=0x076A
        | 0x076D..=0x0770
        | 0x0772
        | 0x0775..=0x0777
        | 0x077A..=0x077F
        | 0x08A0..=0x08A9
        | 0x08AF
        | 0x08B0
        | 0x08B3..=0x08B8
        | 0x08BA..=0x08C7 => Joining::D,
        _ => Joining::U,
    }
}

/// Arabic-script letter (strong right-to-left "AL" class).
pub fn is_arabic_letter(c: char) -> bool {
    let u = c as u32;
    let in_block = matches!(u, 0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF
        | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF);
    in_block && !is_mark(c) && !is_arabic_digit(c) && !matches!(u, 0x060C | 0x066B | 0x066C)
}

pub fn is_arabic_digit(c: char) -> bool {
    matches!(c as u32, 0x0660..=0x0669 | 0x06F0..=0x06F9)
}

pub fn has_arabic(s: &str) -> bool {
    s.chars().any(is_arabic_letter)
}

/// `out[i]` is true when character `i` connects to the next non-transparent
/// character (logical order).
pub fn joins_next(chars: &[char]) -> Vec<bool> {
    let mut out = vec![false; chars.len()];
    for i in 0..chars.len() {
        let a = joining(chars[i]);
        if !matches!(a, Joining::D | Joining::C) {
            continue;
        }
        if let Some(j) = (i + 1..chars.len()).find(|&j| joining(chars[j]) != Joining::T) {
            out[i] = matches!(joining(chars[j]), Joining::D | Joining::R | Joining::C);
        }
    }
    out
}

fn is_lam(c: char) -> bool {
    matches!(c as u32, 0x0644 | 0x06B5..=0x06B8 | 0x076A | 0x08A6)
}
fn is_alef(c: char) -> bool {
    matches!(
        c as u32,
        0x0622 | 0x0623 | 0x0625 | 0x0627 | 0x0671..=0x0673 | 0x0675
    )
}

/// Insert `count` tatweels into every Arabic word of `line`. Candidates are
/// the joins between two connected letters (never inside lam-alef, never
/// next to an existing tatweel); later joins are preferred, extra tatweels
/// are distributed round-robin starting from the last join.
pub fn insert_kashida(line: &str, count: u32) -> String {
    if count == 0 || !has_arabic(line) {
        return line.to_string();
    }
    let chars: Vec<char> = line.chars().collect();
    let joins = joins_next(&chars);
    // insertion count before char index
    let mut extra = vec![0u32; chars.len() + 1];
    let mut i = 0;
    while i < chars.len() {
        if chars[i].is_whitespace() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && !chars[i].is_whitespace() {
            i += 1;
        }
        let end = i;
        let mut cands = Vec::new();
        for a in start..end {
            if !joins[a] || !is_arabic_letter(chars[a]) {
                continue;
            }
            let Some(b) = (a + 1..end).find(|&j| joining(chars[j]) != Joining::T) else {
                continue;
            };
            if chars[b] == TATWEEL || !is_arabic_letter(chars[b]) {
                continue;
            }
            if is_lam(chars[a]) && is_alef(chars[b]) {
                continue;
            }
            cands.push(b);
        }
        if cands.is_empty() {
            continue;
        }
        cands.reverse();
        for k in 0..count as usize {
            extra[cands[k % cands.len()]] += 1;
        }
    }
    let mut out = String::with_capacity(line.len() + count as usize * 4);
    for (i, &c) in chars.iter().enumerate() {
        for _ in 0..extra[i] {
            out.push(TATWEEL);
        }
        out.push(c);
    }
    out
}

/// Paragraph direction from the first strong character (rules P2/P3).
/// Text without strong characters is RTL when `default_rtl`.
pub fn paragraph_rtl(text: &str, default_rtl: bool) -> bool {
    match unicode_bidi::get_base_direction(text) {
        unicode_bidi::Direction::Rtl => true,
        unicode_bidi::Direction::Ltr => false,
        unicode_bidi::Direction::Mixed => default_rtl,
    }
}

/// A directional run: char range `[start, end)` of the line in logical
/// order, and its embedding level (odd = right-to-left).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    pub start: usize,
    pub end: usize,
    pub level: u8,
}

impl Run {
    pub fn rtl(&self) -> bool {
        self.level % 2 == 1
    }
}

/// Split one line (a single paragraph) into directional runs in VISUAL
/// order using the Unicode Bidirectional Algorithm (`unicode-bidi`).
pub fn visual_runs(chars: &[char], para_rtl: bool) -> Vec<Run> {
    if chars.is_empty() {
        return Vec::new();
    }
    let text: String = chars.iter().collect();
    let level = if para_rtl {
        unicode_bidi::Level::rtl()
    } else {
        unicode_bidi::Level::ltr()
    };
    let info = unicode_bidi::ParagraphBidiInfo::new(&text, Some(level));
    let (levels, runs) = info.visual_runs(0..text.len());
    // byte offset → char index
    let mut b2c = vec![0usize; text.len() + 1];
    for (ci, (b, _)) in text.char_indices().enumerate() {
        b2c[b] = ci;
    }
    b2c[text.len()] = chars.len();
    runs.into_iter()
        .filter(|r| !r.is_empty())
        .map(|r| Run {
            start: b2c[r.start],
            end: b2c[r.end],
            level: levels[r.start].number(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn visual(s: &str) -> String {
        let chars: Vec<char> = s.chars().collect();
        let rtl = paragraph_rtl(s, true);
        let mut out = String::new();
        for r in visual_runs(&chars, rtl) {
            let seg: Vec<char> = chars[r.start..r.end].to_vec();
            if r.rtl() {
                out.extend(seg.iter().rev());
            } else {
                out.extend(seg.iter());
            }
        }
        out
    }

    #[test]
    fn joining_types() {
        let c: Vec<char> = "محمد".chars().collect();
        assert_eq!(joins_next(&c), vec![true, true, true, false]);
        let c: Vec<char> = "دار".chars().collect();
        assert_eq!(joins_next(&c), vec![false, false, false]);
        let c: Vec<char> = "مُحَمَّد".chars().collect();
        assert!(joins_next(&c)[0]);
    }

    #[test]
    fn bidi_orders() {
        assert_eq!(visual("abc"), "abc");
        assert_eq!(visual("ابت"), "تبا");
        // Latin paragraph: Arabic word followed by number → number shows
        // to the left of the Arabic word (AN after AL).
        assert_eq!(visual("Tarz طرز 2026"), "Tarz 2026 زرط");
        // Arabic paragraph with Latin and digits embedded.
        assert_eq!(visual("طرز Tarz 2026"), "Tarz 2026 زرط");
        assert_eq!(visual("سنة 2026"), "2026 ةنس");
    }

    #[test]
    fn kashida_prefers_late_joins() {
        assert_eq!(insert_kashida("محمد", 1), "محمـد");
        assert_eq!(insert_kashida("محمد", 2), "محـمـد");
        // never inside lam-alef
        assert_eq!(insert_kashida("سلام", 1), "سـلام");
        assert_eq!(insert_kashida("abc", 3), "abc");
    }
}
