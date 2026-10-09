//! FlatLine row paint: decorations, segments, caret, visual-line draw.

use crate::color_emoji::ColorEmojiAtlas;
use crate::core::types::{detect_level, FlatLine, TextSegment};
use crate::core::visible::{highlight_search_in_segments, SearchPattern};
use crate::viewport_layout::{slice_segments, TextSelection};

use super::fonts::{FontStack, GlyphCache};
use super::primitives::{
    draw_text, drawable_text, fill_rect, highlight_selection_in_segments, severity_cue_color,
    style_to_draw, text_width, CARET_FG, DEFAULT_FG, DISCLOSURE_COLLAPSED, DISCLOSURE_EXPANDED,
};
use super::ViewportCaret;

/// Pixel position (x, row_top) of a block caret within the viewport.
///
/// `visual_lines` must be a **visible-only** slice whose index 0 is the top
/// on-screen row (callers pass the `collect_visible` result). Row geometry uses
/// that slice index — never an absolute scrolled row — so there is no
/// `first_row` parameter (passing one would invite a silent off-by-scroll bug).
pub fn caret_pixel_pos(
    lines: &[FlatLine],
    visual_lines: &[crate::viewport_layout::VisualLine],
    caret: ViewportCaret,
    y_offset: f32,
    x_base: i32,
    row_stride: f32,
    cell_width: u32,
    height: u32,
) -> Option<(i32, f32)> {
    let line = lines.get(caret.flat_index)?;
    // Caret columns are display cells: zero-width marks (VS16, ZWJ, combining
    // marks) occupy no cell, matching the draw path's glyph placement (#238).
    let char_len = crate::color_emoji::display_cell_count(&line.raw);
    let byte_at = crate::viewport_layout::byte_offset_for_char_col(&line.raw, caret.col);

    for (vis_i, visual) in visual_lines.iter().enumerate() {
        if visual.flat_index != caret.flat_index {
            continue;
        }
        // Last wrap of this flat line ends at `raw.len()` — works with a
        // visible-only slice (no need for the full wrap layout).
        let is_last = visual.end == line.raw.len();
        let in_slice = if is_last {
            byte_at >= visual.start
        } else {
            byte_at >= visual.start && byte_at < visual.end
        };
        if !in_slice {
            continue;
        }
        let row_top = -y_offset + vis_i as f32 * row_stride;
        if row_top >= height as f32 {
            return None;
        }
        let cols_before = if caret.col >= char_len && is_last {
            crate::color_emoji::display_cell_count(
                &line.raw[visual.start..visual.end.min(line.raw.len())],
            ) + (caret.col - char_len)
        } else {
            let end = byte_at.min(line.raw.len()).max(visual.start);
            crate::color_emoji::display_cell_count(&line.raw[visual.start..end])
        };
        let x = x_base + (cols_before as i32) * cell_width as i32;
        return Some((x, row_top));
    }
    None
}

pub(super) fn draw_caret_block(
    out: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    row_top: f32,
    cell_width: u32,
    row_height: f32,
) {
    let w = cell_width.max(1) as usize;
    let h = row_height.ceil().max(1.0) as usize;
    let y = row_top.ceil() as i32;
    // Invert the cell so the caret stays visible on any background.
    let clip_top = y.max(0);
    let clip_bottom = (y + h as i32).min(height as i32);
    for py in clip_top..clip_bottom {
        for col in 0..w {
            let px = x + col as i32;
            if px < 0 || px >= width as i32 {
                continue;
            }
            let idx = ((py as u32 * width + px as u32) * 4) as usize;
            out[idx] = 255 - out[idx];
            out[idx + 1] = 255 - out[idx + 1];
            out[idx + 2] = 255 - out[idx + 2];
            out[idx + 3] = 255;
        }
    }
    // Ensure an empty cell still shows a solid bar.
    let mut any_lit = false;
    for py in clip_top..clip_bottom {
        for col in 0..w {
            let px = x + col as i32;
            if px < 0 || px >= width as i32 {
                continue;
            }
            let idx = ((py as u32 * width + px as u32) * 4) as usize;
            if out[idx] > 40 || out[idx + 1] > 40 || out[idx + 2] > 40 {
                any_lit = true;
                break;
            }
        }
        if any_lit {
            break;
        }
    }
    if !any_lit {
        fill_rect(
            out,
            width,
            height,
            x,
            y,
            w,
            h,
            CARET_FG,
            Some((row_top, row_top + row_height)),
        );
    }
}
pub(super) fn draw_visual_line(
    fonts: &FontStack,
    color_emoji: Option<&ColorEmojiAtlas>,
    glyph_cache: &mut GlyphCache,
    out: &mut [u8],
    width: u32,
    height: u32,
    x_base: i32,
    row_top: f32,
    row_height: f32,
    clip: (f32, f32),
    line: &FlatLine,
    flat_index: usize,
    slice_start: usize,
    slice_end: usize,
    search_pattern: Option<&SearchPattern>,
    filter_draft_pattern: Option<&SearchPattern>,
    active_range: Option<(usize, usize)>,
    selection: Option<&TextSelection>,
    cell_width: u32,
    font_size: f32,
) {
    let mut segments = slice_segments(&line.segments, slice_start, slice_end);
    if segments.is_empty() && slice_end > slice_start {
        segments.push(TextSegment {
            text: line.raw[slice_start..slice_end].to_string(),
            style: None,
        });
    }

    if let Some(pattern) = search_pattern {
        segments = highlight_search_in_segments(&segments, pattern, active_range);
    }
    if let Some(pattern) = filter_draft_pattern {
        segments = highlight_search_in_segments(&segments, pattern, None);
    }
    if let Some(sel) = selection {
        segments =
            highlight_selection_in_segments(&segments, sel, flat_index, slice_start, slice_end);
    }

    if slice_start == 0 {
        draw_row_decorations(
            out, width, height, x_base, row_top, row_height, clip, line, cell_width,
        );
    }

    let mut cursor_x = x_base;
    let drew_any = draw_segments(
        fonts,
        color_emoji,
        glyph_cache,
        out,
        width,
        height,
        &mut cursor_x,
        row_top,
        row_height,
        clip,
        &segments,
        cell_width,
        font_size,
    );
    if !drew_any {
        let text = drawable_text(&line.raw[slice_start..slice_end]);
        if !text.is_empty() {
            draw_text(
                fonts,
                color_emoji,
                glyph_cache,
                out,
                width,
                height,
                x_base,
                row_top,
                row_height,
                Some(clip),
                &text,
                DEFAULT_FG,
                false,
                cell_width,
                font_size,
            );
        }
    }
}

/// Severity bar, disclosure cue, and collapsed "+N" preview marks in LEFT_PAD.
///
/// Non-selectable muted gutter on the first visual row of a leveled / collapsible
/// Record — does not insert characters into `raw` / selection / copy text.
fn draw_row_decorations(
    out: &mut [u8],
    width: u32,
    height: u32,
    x_base: i32,
    row_top: f32,
    row_height: f32,
    clip: (f32, f32),
    line: &FlatLine,
    cell_width: u32,
) {
    // Sit in LEFT_PAD with a few pixels of gap so the bar does not glue to glyphs.
    const SEVERITY_TEXT_GAP: i32 = 3;
    let paint_level = line.level.or_else(|| detect_level(&line.raw));
    let gutter_w = (cell_width / 3).clamp(2, 4) as i32;
    let y = row_top.floor() as i32;
    let h = row_height.ceil().max(1.0) as usize;

    if let Some(level) = paint_level {
        let color = severity_cue_color(level);
        let gutter_x = x_base - SEVERITY_TEXT_GAP - gutter_w;
        fill_rect(
            out,
            width,
            height,
            gutter_x,
            y,
            gutter_w as usize,
            h,
            color,
            Some(clip),
        );
    }
    // Disclosure cue for multiline Records (collapsed vs expanded).
    if line.collapsible && line.line_index == 0 {
        let color = if line.collapsed {
            DISCLOSURE_COLLAPSED
        } else {
            DISCLOSURE_EXPANDED
        };
        let cue_w = (cell_width / 2).clamp(3, 5) as usize;
        // Keep disclosure in the pad, left of text (and left of severity when both exist).
        let cue_x = if paint_level.is_some() {
            x_base - SEVERITY_TEXT_GAP - gutter_w - 1 - cue_w as i32
        } else {
            x_base - SEVERITY_TEXT_GAP - cue_w as i32
        };
        fill_rect(out, width, height, cue_x, y, cue_w, h, color, Some(clip));
        // Collapsed preview: muted "+N" suffix via small right-side hash marks.
        if line.collapsed && line.hidden_line_count > 0 {
            let mark_x = x_base + (width as i32).saturating_sub(cell_width as i32 * 4);
            if mark_x > x_base {
                fill_rect(
                    out,
                    width,
                    height,
                    mark_x,
                    y + (h as i32 / 3),
                    (cell_width as usize).saturating_mul(2).min(16),
                    (h / 3).max(2),
                    DISCLOSURE_COLLAPSED,
                    Some(clip),
                );
            }
        }
    }
}

fn draw_segments(
    fonts: &FontStack,
    color_emoji: Option<&ColorEmojiAtlas>,
    glyph_cache: &mut GlyphCache,
    out: &mut [u8],
    width: u32,
    height: u32,
    cursor_x: &mut i32,
    row_top: f32,
    row_height: f32,
    clip: (f32, f32),
    segments: &[crate::core::types::TextSegment],
    cell_width: u32,
    font_size: f32,
) -> bool {
    let mut drew_any = false;
    let width_i = width as i32;
    for segment in segments {
        // FlatLine segments are ANSI-stripped at build time
        // (flat_lines_from_raw_lines / parse_ansi_line); re-parsing per frame
        // was the hot path's biggest avoidable cost (issue #61).
        let text = segment.text.as_str();
        if text.is_empty() {
            continue;
        }
        let (fg, bold, bg, underline) = style_to_draw(segment.style.as_ref());
        let text_w = text_width(text, cell_width) as i32;
        if *cursor_x + text_w <= 0 {
            *cursor_x += text_w;
            continue;
        }
        if let Some(bg_color) = bg {
            fill_rect(
                out,
                width,
                height,
                *cursor_x,
                clip.0.ceil() as i32,
                text_w.max(0) as usize,
                (clip.1 - clip.0).ceil().max(1.0) as usize,
                bg_color,
                Some(clip),
            );
        }
        if underline {
            // Thin bar near the baseline, spanning the whole segment.
            let uy = (row_top + row_height * 0.82).floor() as i32;
            let uh = ((row_height / 14.0).round() as usize).max(1);
            fill_rect(
                out,
                width,
                height,
                *cursor_x,
                uy,
                text_w.max(0) as usize,
                uh,
                fg,
                Some(clip),
            );
        }
        draw_text(
            fonts,
            color_emoji,
            glyph_cache,
            out,
            width,
            height,
            *cursor_x,
            row_top,
            row_height,
            Some(clip),
            text,
            fg,
            bold,
            cell_width,
            font_size,
        );
        *cursor_x += text_w;
        drew_any = true;
        if *cursor_x >= width_i {
            break;
        }
    }
    drew_any
}
