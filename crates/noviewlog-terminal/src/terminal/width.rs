//! Minimal `wcwidth`-style character width for the VT cell grid.
//!
//! `noviewlog-terminal` is the publishable crate with an agreed dependency
//! allowlist, so there is no `unicode-width` dependency here. Instead this
//! module carries a small range table that mirrors the Unicode East Asian
//! Width `W` / `F` classes for the common ranges (issue #78).
//!
//! Limitations (documented, intentional):
//! - Characters outside the wide table default to width 1, so emoji beyond
//!   the listed pictograph ranges stay width 1. The renderer already
//!   tolerates one-cell overflow by design.
//! - Cells store a single `char`, so combining marks (width 0) cannot attach
//!   to their base cell; [`char_width`] reports 0 for them and the grid
//!   drops them without moving the cursor.

/// East Asian Wide (`W`) / Fullwidth (`F`) ranges, inclusive. Mirrors the
/// common subsets of those classes (CJK, Hangul, Kana, fullwidth forms,
/// common pictograph blocks).
const WIDE_RANGES: &[(u32, u32)] = &[
    (0x1100, 0x115F),   // Hangul Jamo initial consonants
    (0x2E80, 0x303E),   // CJK Radicals Supplement .. CJK Symbols (excl. U+303F)
    (0x3041, 0x33FF),   // Hiragana .. CJK Compatibility
    (0x3400, 0x4DBF),   // CJK Unified Ideographs Extension A
    (0x4E00, 0x9FFF),   // CJK Unified Ideographs
    (0xA000, 0xA4CF),   // Yi Syllables / Radicals
    (0xA960, 0xA97F),   // Hangul Jamo Extended-A
    (0xAC00, 0xD7A3),   // Hangul Syllables
    (0xF900, 0xFAFF),   // CJK Compatibility Ideographs
    (0xFE10, 0xFE19),   // Vertical Forms
    (0xFE30, 0xFE6F),   // CJK Compatibility Forms
    (0xFF00, 0xFF60),   // Fullwidth Forms (halfwidth range excluded)
    (0xFFE0, 0xFFE6),   // Fullwidth signs
    (0x1F000, 0x1F02F), // Mahjong / Domino Tiles
    (0x1F300, 0x1F64F), // Misc Symbols and Pictographs, Emoticons
    (0x1F900, 0x1F9FF), // Supplemental Symbols and Pictographs
    (0x20000, 0x2FFFD), // CJK Extension B+
    (0x30000, 0x3FFFD), // CJK Extension G+
];

/// Zero-width ranges: combining marks, zero-width format controls,
/// variation selectors and the BOM.
const ZERO_RANGES: &[(u32, u32)] = &[
    (0x0300, 0x036F), // Combining Diacritical Marks
    (0x200B, 0x200F), // Zero-width space .. RTL marks
    (0xFE00, 0xFE0F), // Variation Selectors
    (0xFEFF, 0xFEFF), // Zero-width no-break space (BOM)
];

/// Grid cell width of `c`: 0 (zero-width), 2 (double-width) or 1 (normal).
/// Public so the TUI host paints and hit-tests in the same cell space as
/// the emulator.
pub fn char_width(c: char) -> usize {
    let v = c as u32;
    if in_ranges(v, ZERO_RANGES) {
        0
    } else if in_ranges(v, WIDE_RANGES) {
        2
    } else {
        1
    }
}

fn in_ranges(v: u32, ranges: &[(u32, u32)]) -> bool {
    ranges.iter().any(|&(lo, hi)| v >= lo && v <= hi)
}

#[cfg(test)]
mod tests {
    use super::char_width;

    #[test]
    fn widths_match_wcwidth_expectations() {
        assert_eq!(char_width('a'), 1);
        assert_eq!(char_width('中'), 2);
        assert_eq!(char_width('文'), 2);
        assert_eq!(char_width('한'), 2);
        assert_eq!(char_width('ｱ'), 1); // halfwidth katakana
        assert_eq!(char_width('！'), 2); // fullwidth !
        assert_eq!(char_width('─'), 1); // box drawing stays narrow
        assert_eq!(char_width('\u{0301}'), 0); // combining acute
        assert_eq!(char_width('\u{200B}'), 0); // zero-width space
        assert_eq!(char_width('\u{FE0F}'), 0); // variation selector
        assert_eq!(char_width('😀'), 2); // pictograph range
        assert_eq!(char_width('\u{1F600}'), 2);
    }
}
