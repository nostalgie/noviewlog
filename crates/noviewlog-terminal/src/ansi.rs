//! Line-oriented SGR parse / strip / overlay (**non-VT layer**).
//!
//! For live PTY screen emulation (cursor, erase, scrollback), see
//! [`crate::terminal`]. That module re-emits ANSI rows; this module
//! turns those (or file) lines into styled [`TextSegment`]s for filters and
//! the viewport. Do not add VT cursor semantics here.

use crate::types::{TextSegment, TextStyle};

use std::sync::Arc;

// Range checks compare the char itself, not `c as u8`: the cast truncates to
// the low 8 bits, so a non-ASCII char after a malformed `ESC[` could be
// misclassified (e.g. U+0170 looks like 'p') and silently swallowed.
fn is_csi_param(c: char) -> bool {
    ('\u{30}'..='\u{3F}').contains(&c)
}

fn is_csi_intermediate(c: char) -> bool {
    ('\u{20}'..='\u{2F}').contains(&c)
}

fn is_csi_final(c: char) -> bool {
    ('\u{40}'..='\u{7E}').contains(&c)
}

/// Consume an escape sequence that is neither CSI nor OSC, starting at the
/// char after ESC: zero or more intermediate bytes (0x20–0x2F) followed by
/// one final byte, e.g. `ESC ( B` (charset designation, emitted by
/// ncurses/less) or `ESC M` (reverse index). Skipping only one char leaked
/// the final byte into the text (issue #188).
fn skip_esc_sequence(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.peek().copied() {
        Some(c) if is_csi_intermediate(c) => {
            // Consume intermediates, then the final byte.
            for c in chars.by_ref() {
                if !is_csi_intermediate(c) {
                    break;
                }
            }
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

/// Strip all ANSI escape sequences (for filtering / parsing).
///
/// Byte-skip pass (issue #54): plain-text semantics identical to joining
/// [`parse_ansi_line`] segments — ESC sequences dropped, `\r` keeps only text
/// after the last CR, `\t` kept, other control chars dropped — but no
/// `TextSegment`/`TextStyle` allocation, which callers immediately discard.
pub fn strip_ansi(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    // Consume params/intermediates, stop at the final byte or
                    // at any other char (consumed and dropped, as before).
                    for c in chars.by_ref() {
                        if is_csi_final(c) {
                            break;
                        }
                        if !(is_csi_param(c) || is_csi_intermediate(c)) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC body: until BEL or ST (ESC \).
                    while let Some(c) = chars.next() {
                        if c == '\u{07}' {
                            break;
                        }
                        if c == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                // Other ESC forms — intermediates + final byte.
                Some(_) => {
                    skip_esc_sequence(&mut chars);
                }
                None => {}
            }
            continue;
        }
        if ch == '\r' {
            // CR overwrite within a finished line: keep only text after last CR.
            out.clear();
            continue;
        }
        if ch == '\t' || !ch.is_control() {
            out.push(ch);
        }
    }
    out
}

/// Parse a line into styled segments, keeping SGR colors and dropping other CSI/OSC.
pub fn parse_ansi_line(input: &str) -> Vec<TextSegment> {
    let mut segments = Vec::new();
    let mut current = TextStyle::default();
    // Currently active OSC 8 URI (`None` = plain text).
    let mut current_link: Option<Arc<str>> = None;
    let mut text = String::new();
    let mut chars = input.chars().peekable();

    let flush = |text: &mut String, current: &TextStyle, segments: &mut Vec<TextSegment>| {
        if text.is_empty() {
            return;
        }
        let style = if current == &TextStyle::default() {
            None
        } else {
            Some(current.clone())
        };
        segments.push(TextSegment {
            text: std::mem::take(text),
            style,
        });
    };

    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    let mut params = String::new();
                    let mut intermediate = String::new();
                    let mut final_byte = None;
                    for c in chars.by_ref() {
                        if is_csi_param(c) {
                            params.push(c);
                        } else if is_csi_intermediate(c) {
                            intermediate.push(c);
                        } else if is_csi_final(c) {
                            final_byte = Some(c);
                            break;
                        } else {
                            break;
                        }
                    }
                    if final_byte == Some('m') && intermediate.is_empty() && !params.contains('?') {
                        flush(&mut text, &current, &mut segments);
                        apply_sgr(&mut current, &params);
                        current.link = current_link.clone();
                    }
                    // Non-SGR CSI (cursor, erase, etc.) is dropped.
                }
                Some(']') => {
                    chars.next();
                    // Capture the OSC body (until BEL or ST) before dropping it.
                    let mut body = String::new();
                    let mut terminated = false;
                    while let Some(c) = chars.next() {
                        if c == '\u{07}' {
                            terminated = true;
                            break;
                        }
                        if c == '\u{1b}' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            terminated = true;
                            break;
                        }
                        body.push(c);
                    }
                    if terminated {
                        if let Some(uri) = parse_osc8_uri(&body) {
                            flush(&mut text, &current, &mut segments);
                            current_link = if uri.is_empty() {
                                None // OSC 8 with empty URI closes the link
                            } else {
                                Some(Arc::from(uri))
                            };
                            current.link = current_link.clone();
                        }
                    }
                }
                Some(_) => {
                    // Other ESC forms — intermediates + final byte.
                    skip_esc_sequence(&mut chars);
                }
                None => {}
            }
            continue;
        }

        if ch == '\r' {
            // CR overwrite within a finished line: keep only text after last CR.
            text.clear();
            segments.clear();
            current = TextStyle::default();
            current_link = None;
            continue;
        }

        if ch == '\t' || !ch.is_control() {
            text.push(ch);
        }
    }

    flush(&mut text, &current, &mut segments);

    if segments.is_empty() {
        vec![TextSegment {
            text: String::new(),
            style: None,
        }]
    } else {
        segments
    }
}

/// Expand SGR params into the flat code list of the equivalent semicolon-only
/// form. Colon subparameters (ITU T.416) are mapped onto their `;` twins
/// (`38:5:idx` → `38;5;idx`, `4:1` → `4`); unsupported colon forms are dropped
/// entirely — letting them degrade to stray codes would e.g. turn `38:2;…`
/// leftovers into a style-resetting `0`.
fn sgr_codes(params: &str) -> Vec<u32> {
    fn parse_piece(p: &str) -> Option<u32> {
        if p.is_empty() {
            Some(0)
        } else {
            p.parse().ok()
        }
    }

    let pieces: Vec<&str> = params.split(';').collect();
    let mut codes = Vec::new();
    let mut i = 0;
    while i < pieces.len() {
        let piece = pieces[i];
        i += 1;
        if !piece.contains(':') {
            if let Some(n) = parse_piece(piece) {
                codes.push(n);
            }
            continue;
        }
        let mut parts = piece.split(':');
        let lead = parts.next().and_then(|p| p.parse().ok());
        let subs: Vec<u32> = parts.map(|p| p.parse().ok().unwrap_or(0)).collect();
        let Some(lead) = lead else { continue };
        match (lead, subs.as_slice()) {
            (4, [0]) => codes.push(24),
            (4, [n]) if *n > 0 => codes.push(4),
            (c @ (38 | 48), [5, idx]) => codes.extend([c, 5, *idx]),
            (c @ (38 | 48), [2, .., r, g, b]) => codes.extend([c, 2, *r, *g, *b]),
            // `38:2;r;g;b` / `38:5;idx` — color kind in the colon group, the
            // values in the following `;`-separated pieces.
            (c @ (38 | 48), [2]) => {
                if i + 3 <= pieces.len() {
                    if let (Some(r), Some(g), Some(b)) = (
                        parse_piece(pieces[i]),
                        parse_piece(pieces[i + 1]),
                        parse_piece(pieces[i + 2]),
                    ) {
                        codes.extend([c, 2, r, g, b]);
                        i += 3;
                    } else {
                        // Malformed values: drop the rest of the group.
                        i = pieces.len();
                    }
                } else {
                    // Truncated group (`38:2;0;0`): the remaining `;`-pieces
                    // belong to it — consume-and-drop so e.g. a standalone
                    // `0` cannot leak in as a style reset.
                    i = pieces.len();
                }
            }
            (c @ (38 | 48), [5]) => {
                if let Some(&piece) = pieces.get(i) {
                    if let Some(idx) = parse_piece(piece) {
                        codes.extend([c, 5, idx]);
                        i += 1;
                    } else {
                        i = pieces.len();
                    }
                }
            }
            // Unsupported colon form (e.g. `58:...`). Color-lead groups
            // follow the 38/48 grammar: an `X:2`-style group may carry its
            // r;g;b across this piece AND the following `;`-pieces — consume
            // exactly what it still needs so no stray codes leak. A complete
            // group (`58:5:9`) owns only its own piece; trailing `;`-pieces
            // are independent parameters and must survive.
            _ => {
                let needed = match subs.as_slice() {
                    [2] => 3,
                    [2, ..] => 3 - (subs.len() - 2).min(3),
                    [5] => 1,
                    _ => 0,
                };
                i = (i + needed).min(pieces.len());
            }
        }
    }
    codes
}

fn apply_sgr(style: &mut TextStyle, params: &str) {
    if params.is_empty() {
        *style = TextStyle::default();
        return;
    }

    let codes = sgr_codes(params);

    let mut i = 0;
    while i < codes.len() {
        match codes[i] {
            0 => *style = TextStyle::default(),
            1 => style.bold = true,
            2 => style.dim = true,
            4 => style.underline = true,
            22 => {
                style.bold = false;
                style.dim = false;
            }
            24 => style.underline = false,
            39 => style.fg = None,
            49 => style.bg = None,
            n @ 30..=37 => style.fg = Some(ansi_basic_color(n - 30, false)),
            n @ 90..=97 => style.fg = Some(ansi_basic_color(n - 90, true)),
            n @ 40..=47 => style.bg = Some(ansi_basic_color(n - 40, false)),
            n @ 100..=107 => style.bg = Some(ansi_basic_color(n - 100, true)),
            38 | 48 => {
                let is_fg = codes[i] == 38;
                if i + 1 < codes.len() {
                    match codes[i + 1] {
                        5 if i + 2 < codes.len() => {
                            let color = ansi_256_color(codes[i + 2]);
                            if is_fg {
                                style.fg = Some(color);
                            } else {
                                style.bg = Some(color);
                            }
                            i += 2;
                        }
                        2 if i + 4 < codes.len() => {
                            let color = (
                                codes[i + 2].min(255) as u8,
                                codes[i + 3].min(255) as u8,
                                codes[i + 4].min(255) as u8,
                            );
                            if is_fg {
                                style.fg = Some(color);
                            } else {
                                style.bg = Some(color);
                            }
                            i += 4;
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
}

/// OSC 8 body `8;params;uri` → the URI (`Some("")` when the link closes).
/// Returns `None` for non-OSC-8 bodies.
pub fn parse_osc8_uri(body: &str) -> Option<&str> {
    let rest = body.strip_prefix("8;")?;
    // Params run to the first ';'; the URI is everything after it.
    let (_, uri) = rest.split_once(';')?;
    Some(uri)
}

pub(crate) fn ansi_basic_color(index: u32, bright: bool) -> (u8, u8, u8) {
    // GitHub-dark-ish palette close to common terminal themes.
    let colors = if bright {
        [
            (110, 118, 129), // black/bright gray
            (255, 123, 114), // red
            (86, 211, 100),  // green
            (227, 179, 65),  // yellow
            (121, 192, 255), // blue
            (219, 114, 235), // magenta
            (86, 210, 217),  // cyan
            (255, 255, 255), // white
        ]
    } else {
        [
            (72, 79, 88),    // black
            (248, 81, 73),   // red
            (63, 185, 80),   // green
            (210, 153, 34),  // yellow
            (88, 166, 255),  // blue
            (210, 96, 230),  // magenta
            (57, 197, 207),  // cyan
            (230, 237, 243), // white
        ]
    };
    colors[index as usize % 8]
}

pub(crate) fn ansi_256_color(index: u32) -> (u8, u8, u8) {
    match index {
        0..=7 => ansi_basic_color(index, false),
        8..=15 => ansi_basic_color(index - 8, true),
        16..=231 => {
            let n = index - 16;
            let r = n / 36;
            let g = (n % 36) / 6;
            let b = n % 6;
            let level = |v: u32| if v == 0 { 0 } else { 55 + 40 * v };
            (level(r) as u8, level(g) as u8, level(b) as u8)
        }
        232..=255 => {
            let v = (8 + (index - 232) * 10).min(255) as u8;
            (v, v, v)
        }
        _ => (230, 237, 243),
    }
}

/// Overlay user/search styles onto ANSI base segments by character ranges.
pub fn overlay_styles(base: &[TextSegment], overlays: &[TextSegment]) -> Vec<TextSegment> {
    if overlays.is_empty() {
        return base.to_vec();
    }
    if base.is_empty() {
        return overlays.to_vec();
    }

    let plain: String = base.iter().map(|s| s.text.as_str()).collect();
    let overlay_plain: String = overlays.iter().map(|s| s.text.as_str()).collect();
    if plain != overlay_plain {
        // Fallback: prefer overlays (user rules) if texts diverge.
        return overlays.to_vec();
    }

    // Merge by character-space runs instead of per-byte style lanes:
    // lanes cost O(line bytes) clones each and ~24 bytes per byte of line,
    // which spiked several hundred MB on a 16 MiB highlighted line
    // (issue #190). Segment runs are contiguous, so a handful of cut points
    // fully describes the merged output.
    let mut char_pos = 0usize;
    let mut base_ranges: Vec<(usize, usize, Option<&TextStyle>)> = Vec::with_capacity(base.len());
    for seg in base {
        let n = seg.text.chars().count();
        base_ranges.push((char_pos, char_pos + n, seg.style.as_ref()));
        char_pos += n;
    }
    let total_chars = char_pos;
    char_pos = 0usize;
    let mut over_ranges: Vec<(usize, usize, Option<&TextStyle>)> =
        Vec::with_capacity(overlays.len());
    for seg in overlays {
        let n = seg.text.chars().count();
        over_ranges.push((char_pos, char_pos + n, seg.style.as_ref()));
        char_pos += n;
    }

    let mut cuts: Vec<usize> = Vec::with_capacity(base.len() + overlays.len() + 2);
    cuts.extend(base_ranges.iter().flat_map(|&(a, b, _)| [a, b]));
    cuts.extend(over_ranges.iter().flat_map(|&(a, b, _)| [a, b]));
    cuts.push(0);
    cuts.push(total_chars);
    cuts.sort_unstable();
    cuts.dedup();

    // Map char-space cut points to byte offsets in one pass.
    let mut cut_bytes: Vec<usize> = Vec::with_capacity(cuts.len());
    let (mut next_cut, mut byte_off, mut ordinal) = (0usize, 0usize, 0usize);
    for ch in plain.chars() {
        while next_cut < cuts.len() && cuts[next_cut] == ordinal {
            cut_bytes.push(byte_off);
            next_cut += 1;
        }
        byte_off += ch.len_utf8();
        ordinal += 1;
    }
    while next_cut < cuts.len() {
        cut_bytes.push(byte_off);
        next_cut += 1;
    }

    let base_style_at = |p: usize| -> Option<&TextStyle> {
        let idx = base_ranges.partition_point(|r| r.1 <= p);
        base_ranges[idx].2
    };
    let over_style_at = |p: usize| -> Option<&TextStyle> {
        let idx = over_ranges.partition_point(|r| r.1 <= p);
        over_ranges[idx].2
    };

    let mut out: Vec<TextSegment> = Vec::new();
    for w in 0..cuts.len().saturating_sub(1) {
        let a = cuts[w];
        let style = match over_style_at(a) {
            Some(over) => Some(merge_style(
                base_style_at(a).cloned().unwrap_or_default(),
                over.clone(),
            )),
            None => base_style_at(a).cloned(),
        };
        let text = &plain[cut_bytes[w]..cut_bytes[w + 1]];
        if let Some(last) = out.last_mut() {
            if last.style == style {
                last.text.push_str(text);
                continue;
            }
        }
        out.push(TextSegment {
            text: text.to_string(),
            style,
        });
    }

    if out.is_empty() {
        vec![TextSegment {
            text: plain,
            style: None,
        }]
    } else {
        out
    }
}

fn merge_style(base: TextStyle, over: TextStyle) -> TextStyle {
    TextStyle {
        fg: over.fg.or(base.fg),
        bg: over.bg.or(base.bg),
        bold: over.bold || base.bold,
        dim: over.dim || base.dim,
        underline: over.underline || base.underline,
        search: over.search || base.search,
        search_current: over.search_current || base.search_current,
        selected: over.selected || base.selected,
        link: over.link.or(base.link),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_basic_green() {
        let segs = parse_ansi_line("\u{1b}[32m✔ Building...\u{1b}[0m");
        let joined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "✔ Building...");
        assert!(segs.iter().any(|s| s
            .style
            .as_ref()
            .is_some_and(|st| st.fg == Some((63, 185, 80)))));
    }

    #[test]
    fn strip_preserves_leading_spaces() {
        assert_eq!(strip_ansi("    at foo.js:1:1"), "    at foo.js:1:1");
    }

    #[test]
    fn strips_cursor_but_keeps_color() {
        let segs = parse_ansi_line("\u{1b}[?25l\u{1b}[32mOK\u{1b}[0m\u{1b}[?25h");
        assert_eq!(
            strip_ansi("\u{1b}[?25l\u{1b}[32mOK\u{1b}[0m\u{1b}[?25h"),
            "OK"
        );
        assert_eq!(segs[0].text, "OK");
        assert!(segs[0].style.clone().unwrap().fg.is_some());
    }

    #[test]
    fn strips_device_attribute_csi() {
        let segs = parse_ansi_line("To access the server \u{1b}[>0;10;1cgo to:");
        let joined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "To access the server go to:");
    }

    #[test]
    fn strip_ansi_plain() {
        assert_eq!(strip_ansi("hello"), "hello");
    }

    #[test]
    fn osc8_hyperlink_becomes_link_style() {
        let line = "\u{1b}]8;;https://example.com/docs\u{7}docs\u{1b}]8;;\u{7} after";
        let segs = parse_ansi_line(line);
        let joined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "docs after");

        let linked = segs
            .iter()
            .find(|s| s.style.as_ref().is_some_and(|st| st.link.is_some()))
            .expect("linked segment");
        assert_eq!(linked.text, "docs");
        assert_eq!(
            linked.style.as_ref().unwrap().link.as_deref(),
            Some("https://example.com/docs")
        );
        assert!(
            segs.iter()
                .all(|s| s.style.as_ref().and_then(|st| st.link.as_deref())
                    != Some("https://example.com/docs")
                    || s.text == "docs"),
            "link must close at the empty OSC 8"
        );
    }

    #[test]
    fn osc8_st_terminated_and_stripped() {
        let line = "\u{1b}]8;;file:///tmp/x\u{1b}\\click\u{1b}]8;;\u{1b}\\";
        assert_eq!(strip_ansi(line), "click");
        let segs = parse_ansi_line(line);
        assert!(segs[0].style.as_ref().unwrap().link.is_some());
    }

    #[test]
    fn osc7_still_ignored_by_line_parser() {
        let segs = parse_ansi_line("\u{1b}]7;file:///home/user\u{7}text");
        assert_eq!(strip_ansi("\u{1b}]7;file:///home/user\u{7}text"), "text");
        assert!(segs.iter().all(|s| s.style.as_ref().is_none()));
    }

    #[test]
    fn sgr_after_osc8_keeps_link() {
        let line = "\u{1b}]8;;https://a.b\u{7}\u{1b}[4mlinked\u{1b}[0m\u{1b}]8;;\u{7}";
        let segs = parse_ansi_line(line);
        let st = segs[0].style.as_ref().unwrap();
        assert!(st.underline && st.link.is_some());
    }

    #[test]
    fn colon_indexed_fg_sets_color_without_reset() {
        // Issue #236: `38:5:idx` must behave like `38;5;idx`, not reset style.
        let segs = parse_ansi_line("\u{1b}[1m\u{1b}[38:5:196mred");
        let st = segs[0].style.as_ref().unwrap();
        assert_eq!(st.fg, Some(ansi_256_color(196)));
        assert!(st.bold, "earlier SGR attributes must survive");
    }

    #[test]
    fn colon_truecolor_with_semicolon_values_sets_fg() {
        // Issue #236: `38:2;r;g;b` (colon color, `;`-separated RGB, seen in
        // the wild) must behave like `38;2;r;g;b`, not reset the style.
        let segs = parse_ansi_line("\u{1b}[38:2;255;0;0mred");
        let st = segs[0].style.as_ref().unwrap();
        assert_eq!(st.fg, Some((255, 0, 0)));
    }

    #[test]
    fn colon_bg_and_underline_subparameters_map_like_semicolon() {
        let segs = parse_ansi_line("\u{1b}[48:5:21m\u{1b}[4:1mu\u{1b}[4:0m-");
        let st = segs[0].style.as_ref().unwrap();
        assert_eq!(st.bg, Some(ansi_256_color(21)));
        assert!(st.underline);
        assert!(!segs[1].style.as_ref().unwrap().underline);
    }

    #[test]
    fn unknown_colon_form_is_ignored_not_reset() {
        let segs = parse_ansi_line("\u{1b}[1m\u{1b}[58:2:1;2;3m\u{1b}[38:7mbold");
        let st = segs[0].style.as_ref().unwrap();
        assert!(st.bold, "unsupported colon forms must not reset style");
        assert!(st.fg.is_none());
    }

    // Pass-3 finding: an unsupported but SELF-CONTAINED colon group owns only
    // its own piece — the following `;`-pieces are independent parameters.
    #[test]
    fn unsupported_complete_colon_group_keeps_following_params() {
        let segs = parse_ansi_line("\u{1b}[4:3;58:5:9;1mtext");
        let st = segs[0].style.as_ref().unwrap();
        assert!(
            st.bold,
            "the trailing ;1 after 58:5:9 must still apply bold"
        );
        assert!(st.underline, "4:3 (curly underline) must apply");
    }

    #[test]
    fn truncated_colon_truecolor_does_not_reset_style() {
        // Issue #255: `38:2;0;0` (truncated RGB) must not leak its leftover
        // `;`-pieces — a standalone `0` would reset the style.
        let segs = parse_ansi_line("\u{1b}[1m\u{1b}[38:2;0;0mred");
        let st = segs[0].style.as_ref().unwrap();
        assert!(st.bold, "truncated truecolor must not reset style");
        assert!(st.fg.is_none());
        assert!(!st.dim);
        assert!(!st.underline);
    }

    #[test]
    fn unsupported_colon_lead_leaves_style_untouched() {
        // Issue #255: `58:2:1;2;3` must not leak `2`/`3` as raw SGR codes
        // (dim / italic-ish attribute flips).
        let segs = parse_ansi_line("\u{1b}[1m\u{1b}[58:2:1;2;3mtext");
        let st = segs[0].style.as_ref().unwrap();
        assert!(st.bold, "earlier SGR attributes must survive");
        assert!(!st.dim);
        assert!(!st.underline);
        assert!(st.fg.is_none());
        assert!(st.bg.is_none());
    }

    #[test]
    fn cr_resets_active_osc8_link() {
        // Issue #236: text reprinted after CR (typically with a fresh SGR)
        // must not inherit the hyperlink from before the CR.
        let line = "\u{1b}]8;;https://example.com/x\u{7}docs\r\u{1b}[31mreprinted";
        let segs = parse_ansi_line(line);
        assert!(
            segs.iter()
                .all(|s| s.style.as_ref().and_then(|st| st.link.as_deref()).is_none()),
            "link must not survive CR: {segs:?}"
        );
    }
    #[test]
    fn strips_esc_intermediate_sequence_final_byte() {
        // Issue #188: `ESC ( B` (charset designation) must not leak `B`.
        assert_eq!(strip_ansi("\u{1b}(Bhello"), "hello");
        assert_eq!(strip_ansi("a\u{1b}(0b\u{1b}(Bc"), "abc");
        let segs = parse_ansi_line("\u{1b}(Bhello");
        let joined: String = segs.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "hello");
    }

    #[test]
    fn overlay_styles_merges_at_char_boundaries() {
        // Issue #190: same result as the old per-byte lanes, on UTF-8 text.
        let base = parse_ansi_line("\u{1b}[31mпривет мир\u{1b}[0m");
        // Highlight "вет" (chars 3..6) in bold.
        let overlays = vec![
            TextSegment {
                text: "при".into(),
                style: None,
            },
            TextSegment {
                text: "вет".into(),
                style: Some(TextStyle {
                    bold: true,
                    ..Default::default()
                }),
            },
            TextSegment {
                text: " мир".into(),
                style: None,
            },
        ];
        let out = overlay_styles(&base, &overlays);
        let joined: String = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "привет мир");
        assert_eq!(out.len(), 3, "red plain + red bold + red plain");
        assert!(out[1].style.as_ref().is_some_and(|st| st.bold));
        // Bold overlay run keeps the base (red) foreground.
        assert_eq!(
            out[1].style.as_ref().and_then(|st| st.fg),
            base[0].style.as_ref().and_then(|st| st.fg)
        );
        // No overlay style on plain runs: inherits base.
        assert_eq!(out[0].style, base[0].style);
    }
}
