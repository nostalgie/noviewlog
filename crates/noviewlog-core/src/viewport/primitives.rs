use super::fonts::{glyph_baseline_y, FontStack, GlyphCache};
use crate::color_emoji::{
    blit_color_emoji, blit_color_emoji_span, char_kind, display_cell_count, display_items,
    is_color_emoji_candidate, CharKind, ColorEmojiAtlas,
};
use crate::core::ansi::strip_ansi;
use crate::core::types::{LogLevel, TextSegment, TextStyle as LineStyle};
use crate::viewport_layout::{selection_slice_range, TextSelection};

pub(super) const BG: [u8; 4] = [0, 0, 0, 255];
pub(super) const DEFAULT_FG: [u8; 4] = [230, 237, 243, 255];
const DIM_FG: [u8; 4] = [139, 148, 158, 255];
pub(super) const HINT_FG: [u8; 4] = [139, 148, 158, 255];
const SEARCH_BG: [u8; 4] = [58, 100, 150, 255];
const SEARCH_CURRENT_BG: [u8; 4] = [184, 134, 11, 255];
const SELECTION_BG: [u8; 4] = [45, 70, 110, 255];
pub(super) const CARET_FG: [u8; 4] = [230, 237, 243, 255];
/// Muted severity gutter cues (not Theme.accent / bright fluent blue).
const SEVERITY_ERROR: [u8; 4] = [180, 80, 80, 255];
const SEVERITY_WARN: [u8; 4] = [180, 145, 70, 255];
const SEVERITY_INFO: [u8; 4] = [100, 140, 165, 255];
const SEVERITY_DEBUG: [u8; 4] = [130, 120, 150, 255];
/// Disclosure cues (muted; not Theme.accent).
pub(super) const DISCLOSURE_COLLAPSED: [u8; 4] = [120, 130, 145, 255];
pub(super) const DISCLOSURE_EXPANDED: [u8; 4] = [90, 100, 115, 255];
pub(super) fn drawable_text(text: &str) -> String {
    strip_ansi(text)
}

pub(super) fn severity_cue_color(level: LogLevel) -> [u8; 4] {
    match level {
        LogLevel::Error => SEVERITY_ERROR,
        LogLevel::Warn => SEVERITY_WARN,
        LogLevel::Info => SEVERITY_INFO,
        LogLevel::Debug => SEVERITY_DEBUG,
    }
}
pub(super) fn highlight_selection_in_segments(
    segments: &[TextSegment],
    sel: &TextSelection,
    flat_index: usize,
    slice_start: usize,
    slice_end: usize,
) -> Vec<TextSegment> {
    let Some((rel_start, rel_end)) = selection_slice_range(sel, flat_index, slice_start, slice_end)
    else {
        return segments.to_vec();
    };

    let abs_start = slice_start + rel_start;
    let abs_end = slice_start + rel_end;
    let mut out = Vec::new();
    let mut cursor = slice_start;
    for seg in segments {
        let seg_start = cursor;
        let seg_end = cursor + seg.text.len();
        cursor = seg_end;

        if seg_end <= abs_start || seg_start >= abs_end {
            out.push(seg.clone());
            continue;
        }

        // Clamp offsets to char boundaries: a selection that survived a buffer
        // swap can hold stale offsets landing mid-character, and plain slicing
        // would panic (issue #235). Mirrors the guard in selection_plain_text.
        let local_start = floor_char_boundary(&seg.text, abs_start.saturating_sub(seg_start));
        let local_end = floor_char_boundary(&seg.text, (abs_end - seg_start).min(seg.text.len()))
            .max(local_start);

        if seg_start < abs_start {
            out.push(TextSegment {
                text: seg.text[..local_start].to_string(),
                style: seg.style.clone(),
            });
        }

        let mut style = seg.style.clone().unwrap_or_default();
        style.selected = true;
        out.push(TextSegment {
            text: seg.text[local_start..local_end].to_string(),
            style: Some(style),
        });

        if seg_end > abs_end {
            out.push(TextSegment {
                text: seg.text[local_end..].to_string(),
                style: seg.style.clone(),
            });
        }
    }
    out
}

/// Largest char boundary `<= at` (and `<= s.len()`), matching the guard in
/// `selection_plain_text`.
fn floor_char_boundary(s: &str, at: usize) -> usize {
    let at = at.min(s.len());
    if s.is_char_boundary(at) {
        at
    } else {
        (0..at).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0)
    }
}
/// (fg, bold, bg, underline) for a segment style.
pub(super) fn style_to_draw(style: Option<&LineStyle>) -> ([u8; 4], bool, Option<[u8; 4]>, bool) {
    let Some(style) = style else {
        return (DEFAULT_FG, false, None, false);
    };
    if style.search_current {
        return (DEFAULT_FG, false, Some(SEARCH_CURRENT_BG), style.underline);
    }
    if style.search {
        return (DEFAULT_FG, false, Some(SEARCH_BG), style.underline);
    }
    if style.selected {
        return (DEFAULT_FG, false, Some(SELECTION_BG), style.underline);
    }
    let mut fg = if style.dim { DIM_FG } else { DEFAULT_FG };
    if let Some((r, g, b)) = style.fg {
        fg = [r, g, b, 255];
    }
    let mut bg = style.bg.map(|(r, g, b)| [r, g, b, 255]);
    if style.search {
        bg = Some(SEARCH_BG);
    }
    // OSC 8 links are always underlined (click affordance).
    (fg, style.bold, bg, style.underline || style.link.is_some())
}

pub(super) fn text_width(text: &str, cell_width: u32) -> u32 {
    if text.is_empty() {
        return 0;
    }
    display_cell_count(text) as u32 * cell_width
}

pub(super) fn draw_text(
    fonts: &FontStack,
    color_emoji: Option<&ColorEmojiAtlas>,
    glyph_cache: &mut GlyphCache,
    out: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    row_top: f32,
    row_height: f32,
    clip: Option<(f32, f32)>,
    text: &str,
    color: [u8; 4],
    bold: bool,
    cell_width: u32,
    font_size: f32,
) {
    if text.is_empty() {
        return;
    }
    let width_i = width as i32;
    let cell_w = cell_width as i32;
    let mut col: i32 = 0;
    for item in display_items(text) {
        let item_x = x + col * cell_w;
        let span = item.cells as i32;
        col += span.max(0);
        if span == 0 {
            // Combining mark: overlay ink onto the previous cell, no advance.
            let prev_x = item_x - cell_w;
            if prev_x + cell_w <= 0 || prev_x >= width_i {
                continue;
            }
            draw_char_at(
                fonts,
                glyph_cache,
                out,
                width,
                height,
                prev_x,
                row_top,
                clip,
                item.text,
                color,
                bold,
                font_size,
            );
            continue;
        }
        if item_x + span * cell_w <= 0 {
            continue;
        }
        if item_x >= width_i {
            break;
        }
        if item.text.chars().count() > 1 {
            // Multi-scalar cluster (ZWJ sequence, flag pair, keycap): prefer
            // the composite CBDT glyph spanning the item's cells.
            if let Some(atlas) = color_emoji {
                if let Some(glyph) = atlas.glyph_cluster(item.text) {
                    blit_color_emoji_span(
                        out,
                        width,
                        height,
                        item_x,
                        row_top,
                        row_height,
                        cell_width,
                        item.cells.max(1) as u32,
                        &glyph,
                        clip,
                    );
                    continue;
                }
            }
        }
        // Per-scalar paint within the item (single chars or cluster fallback).
        let mut sub_col = 0i32;
        for mch in item.text.chars() {
            match char_kind(mch) {
                CharKind::SilentSkip => continue,
                CharKind::Overlay => {
                    if sub_col > 0 {
                        draw_char_at(
                            fonts,
                            glyph_cache,
                            out,
                            width,
                            height,
                            item_x + (sub_col - 1) * cell_w,
                            row_top,
                            clip,
                            &mch.to_string(),
                            color,
                            bold,
                            font_size,
                        );
                    }
                }
                CharKind::Advance => {
                    let cell_x = item_x + sub_col * cell_w;
                    sub_col += 1;
                    if cell_x + cell_w <= 0 || cell_x >= width_i {
                        continue;
                    }
                    // Prefer CBDT color emoji for pictograph ranges when present.
                    if is_color_emoji_candidate(mch) {
                        if let Some(atlas) = color_emoji {
                            if let Some(glyph) = atlas.glyph(mch) {
                                blit_color_emoji(
                                    out, width, height, cell_x, row_top, row_height, cell_width,
                                    &glyph, clip,
                                );
                                continue;
                            }
                        }
                    }
                    draw_char_at(
                        fonts,
                        glyph_cache,
                        out,
                        width,
                        height,
                        cell_x,
                        row_top,
                        clip,
                        &mch.to_string(),
                        color,
                        bold,
                        font_size,
                    );
                }
            }
        }
    }
}

/// Rasterize one scalar (or combining-mark cluster) at a fixed cell position
/// via fontdue. Glyph placement is computed directly from font metrics —
/// building a fontdue `Layout` per glyph per frame was pure overhead
/// (issue #61): the old loop ignored glyph advances anyway (each glyph drew at
/// `cell_x + metrics.xmin`, baseline from the single-line layout).
#[allow(clippy::too_many_arguments)]
fn draw_char_at(
    fonts: &FontStack,
    glyph_cache: &mut GlyphCache,
    out: &mut [u8],
    width: u32,
    height: u32,
    cell_x: i32,
    row_top: f32,
    clip: Option<(f32, f32)>,
    ch_str: &str,
    color: [u8; 4],
    bold: bool,
    font_size: f32,
) {
    for ch in ch_str.chars() {
        if ch.is_control() {
            continue;
        }
        let font = fonts.pick(ch);
        let glyph_index = font.lookup_glyph_index(ch);
        let metrics = font.metrics_indexed(glyph_index, font_size);
        let glyph_x = cell_x + metrics.xmin;
        // fontdue single-line placement: baseline = pen_y + ceil(ascent),
        // bitmap top = baseline + floor(-bounds.height - bounds.ymin).
        let baseline = glyph_baseline_y(font, font_size, row_top);
        let glyph_y = (baseline + (-metrics.bounds.height - metrics.bounds.ymin).floor()) as i32;
        let cached = glyph_cache.rasterize_indexed(font, glyph_index, font_size);
        blit_glyph(
            out,
            width,
            height,
            glyph_x,
            glyph_y,
            &cached.bitmap,
            cached.width,
            cached.height,
            color,
            bold,
            clip,
        );
    }
}

pub(super) fn fill_rect(
    out: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    color: [u8; 4],
    clip: Option<(f32, f32)>,
) {
    let clip_top = clip.map(|c| c.0.floor() as i32).unwrap_or(0);
    let clip_bottom = clip.map(|c| c.1.ceil() as i32).unwrap_or(height as i32);
    for row in 0..h {
        for col in 0..w {
            let px = x + col as i32;
            let py = y + row as i32;
            if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                continue;
            }
            if py < clip_top || py >= clip_bottom {
                continue;
            }
            let idx = ((py as u32 * width + px as u32) * 4) as usize;
            blend_pixel(&mut out[idx..idx + 4], color, 255);
        }
    }
}

fn blit_glyph(
    out: &mut [u8],
    width: u32,
    height: u32,
    x: i32,
    y: i32,
    bitmap: &[u8],
    gw: usize,
    gh: usize,
    color: [u8; 4],
    bold: bool,
    clip: Option<(f32, f32)>,
) {
    let clip_top = clip.map(|c| c.0.floor() as i32).unwrap_or(0);
    let clip_bottom = clip.map(|c| c.1.ceil() as i32).unwrap_or(height as i32);
    for row in 0..gh {
        for col in 0..gw {
            let mut alpha = bitmap[row * gw + col];
            if bold {
                alpha = alpha.saturating_add(alpha / 2);
            }
            if alpha == 0 {
                continue;
            }
            let px = x + col as i32;
            let py = y + row as i32;
            if px < 0 || py < 0 || px >= width as i32 || py >= height as i32 {
                continue;
            }
            if py < clip_top || py >= clip_bottom {
                continue;
            }
            let idx = ((py as u32 * width + px as u32) * 4) as usize;
            blend_pixel(&mut out[idx..idx + 4], color, alpha);
            if bold && px + 1 < width as i32 {
                let idx2 = ((py as u32 * width + (px + 1) as u32) * 4) as usize;
                blend_pixel(&mut out[idx2..idx2 + 4], color, alpha / 2);
            }
        }
    }
}

fn blend_pixel(dst: &mut [u8], src: [u8; 4], alpha: u8) {
    let a = alpha as f32 / 255.0;
    for i in 0..3 {
        dst[i] = ((src[i] as f32 * a) + (dst[i] as f32 * (1.0 - a))) as u8;
    }
    dst[3] = 255;
}
