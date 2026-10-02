use crate::color_emoji::ColorEmojiAtlas;
use crate::core::types::{
    clamp_viewport_font_size, detect_level, FlatLine, SearchMatch, TextSegment,
    DEFAULT_VIEWPORT_FONT_SIZE,
};
use crate::core::visible::{highlight_search_in_segments, SearchPattern};
use crate::viewport_layout::{
    collect_visible_visual_lines_with_total, slice_segments, TextSelection, LEFT_PAD,
};

mod fonts;
mod primitives;

use fonts::{compute_metrics, load_emoji_fallback_font, load_mono_font, FontStack, GlyphCache};
use primitives::{
    draw_text, drawable_text, fill_rect, highlight_selection_in_segments, severity_cue_color,
    style_to_draw, text_width, BG, CARET_FG, DEFAULT_FG, DISCLOSURE_COLLAPSED, DISCLOSURE_EXPANDED,
    HINT_FG,
};

/// Block caret in the rendered viewport (flat-line index + cell column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportCaret {
    pub flat_index: usize,
    pub col: usize,
}

pub struct ViewportMetrics {
    pub row_height: f32,
    pub row_stride: f32,
    pub ascent: f32,
    /// Fixed terminal cell width in whole pixels (every column advances by this amount).
    pub cell_width: u32,
}
pub struct ViewportRenderer {
    fonts: FontStack,
    /// System Noto Color Emoji (CBDT); optional — mono Symbols2 remains the fallback.
    color_emoji: Option<ColorEmojiAtlas>,
    metrics: ViewportMetrics,
    font_size: f32,
    glyph_cache: GlyphCache,
}

impl ViewportRenderer {
    pub fn new() -> Self {
        Self::with_font_size(DEFAULT_VIEWPORT_FONT_SIZE)
    }

    pub fn with_font_size(font_size: f32) -> Self {
        let font_size = clamp_viewport_font_size(font_size);
        let primary = load_mono_font();
        let fallback = load_emoji_fallback_font();
        let color_emoji = ColorEmojiAtlas::load();
        let metrics = compute_metrics(&primary, font_size);
        Self {
            fonts: FontStack::new(primary, fallback),
            color_emoji,
            metrics,
            font_size,
            glyph_cache: GlyphCache::new(),
        }
    }

    pub fn font_size(&self) -> f32 {
        self.font_size
    }

    /// Rebuild cell metrics for a new fontdue size (clamped to 8–32).
    pub fn set_font_size(&mut self, font_size: f32) {
        let font_size = clamp_viewport_font_size(font_size);
        if (self.font_size - font_size).abs() < f32::EPSILON {
            return;
        }
        self.font_size = font_size;
        self.metrics = compute_metrics(&self.fonts.primary, font_size);
        self.glyph_cache.clear();
    }

    pub fn metrics(&self) -> &ViewportMetrics {
        &self.metrics
    }

    pub fn render_center_message(
        &mut self,
        out: &mut [u8],
        width: u32,
        height: u32,
        message: &str,
    ) -> Result<(), String> {
        let expected = (width as usize) * (height as usize) * 4;
        if out.len() < expected {
            return Err(format!(
                "buffer too small: need {expected}, got {}",
                out.len()
            ));
        }
        for px in out[..expected].chunks_exact_mut(4) {
            px.copy_from_slice(&BG);
        }
        let row_top = (height as f32 * 0.45 - self.metrics.ascent).max(0.0);
        draw_text(
            &self.fonts,
            self.color_emoji.as_ref(),
            &mut self.glyph_cache,
            out,
            width,
            height,
            16,
            row_top,
            self.metrics.row_height,
            None,
            message,
            HINT_FG,
            false,
            self.metrics.cell_width,
            self.font_size,
        );
        Ok(())
    }

    pub fn render(
        &mut self,
        out: &mut [u8],
        width: u32,
        height: u32,
        lines: &[FlatLine],
        scroll_y: f32,
        scroll_x: f32,
        wrap_lines: bool,
        selection: Option<&TextSelection>,
        search_pattern: Option<&SearchPattern>,
        filter_draft_pattern: Option<&SearchPattern>,
        active_match: Option<SearchMatch>,
        caret: Option<ViewportCaret>,
    ) -> Result<(), String> {
        self.render_with_total(
            out,
            width,
            height,
            lines,
            scroll_y,
            scroll_x,
            wrap_lines,
            selection,
            search_pattern,
            filter_draft_pattern,
            active_match,
            caret,
            None,
            None,
        )
    }

    pub fn render_with_total(
        &mut self,
        out: &mut [u8],
        width: u32,
        height: u32,
        lines: &[FlatLine],
        scroll_y: f32,
        scroll_x: f32,
        wrap_lines: bool,
        selection: Option<&TextSelection>,
        search_pattern: Option<&SearchPattern>,
        filter_draft_pattern: Option<&SearchPattern>,
        active_match: Option<SearchMatch>,
        caret: Option<ViewportCaret>,
        total_visual_rows: Option<usize>,
        visual_row_index: Option<&crate::viewport_layout::VisualRowIndex>,
    ) -> Result<(), String> {
        let expected = (width as usize) * (height as usize) * 4;
        if out.len() < expected {
            return Err(format!(
                "buffer too small: need {expected}, got {}",
                out.len()
            ));
        }
        for px in out[..expected].chunks_exact_mut(4) {
            px.copy_from_slice(&BG);
        }

        let first_row = (scroll_y / self.metrics.row_stride).floor() as usize;
        let y_offset = scroll_y - first_row as f32 * self.metrics.row_stride;
        let mut row_top = -y_offset;

        let max_rows = (height as f32 / self.metrics.row_stride).ceil() as usize + 1;
        let visual_lines = collect_visible_visual_lines_with_total(
            lines,
            wrap_lines,
            width,
            self.metrics.cell_width,
            first_row,
            max_rows,
            total_visual_rows,
            visual_row_index,
        );
        let x_base = if wrap_lines {
            LEFT_PAD as i32
        } else {
            LEFT_PAD as i32 - scroll_x as i32
        };

        for visual in &visual_lines {
            if row_top >= height as f32 {
                break;
            }
            let line = &lines[visual.flat_index];
            let active_range = active_match
                .filter(|m| m.line_index == visual.flat_index)
                .map(|m| (m.start, m.end));
            let clip = (row_top, row_top + self.metrics.row_height);
            draw_visual_line(
                &self.fonts,
                self.color_emoji.as_ref(),
                &mut self.glyph_cache,
                out,
                width,
                height,
                x_base,
                row_top,
                self.metrics.row_height,
                clip,
                line,
                visual.flat_index,
                visual.start,
                visual.end,
                search_pattern,
                filter_draft_pattern,
                active_range,
                selection,
                self.metrics.cell_width,
                self.font_size,
            );
            row_top += self.metrics.row_stride;
        }

        if let Some(c) = caret {
            if let Some((cx, cy)) = caret_pixel_pos(
                lines,
                &visual_lines,
                c,
                0,
                y_offset,
                x_base,
                self.metrics.row_stride,
                self.metrics.cell_width,
                height,
            ) {
                draw_caret_block(
                    out,
                    width,
                    height,
                    cx,
                    cy,
                    self.metrics.cell_width,
                    self.metrics.row_height,
                );
            }
        }
        Ok(())
    }
}

/// Pixel position (x, row_top) of a block caret within the viewport.
pub fn caret_pixel_pos(
    lines: &[FlatLine],
    visual_lines: &[crate::viewport_layout::VisualLine],
    caret: ViewportCaret,
    first_row: usize,
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
        if vis_i < first_row {
            return None;
        }
        let row_top = -y_offset + (vis_i - first_row) as f32 * row_stride;
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

fn draw_caret_block(
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
fn draw_visual_line(
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

    // Non-selectable muted gutter on the first visual row of a leveled Record.
    // Does not insert characters into `raw` / selection / copy text.
    // Sit in LEFT_PAD with a few pixels of gap so the bar does not glue to glyphs.
    const SEVERITY_TEXT_GAP: i32 = 3;
    let paint_level = line.level.or_else(|| detect_level(&line.raw));
    if slice_start == 0 {
        if let Some(level) = paint_level {
            let color = severity_cue_color(level);
            let gutter_w = ((cell_width / 3).clamp(2, 4)) as usize;
            let y = row_top.floor() as i32;
            let h = row_height.ceil().max(1.0) as usize;
            let gutter_x = x_base - SEVERITY_TEXT_GAP - gutter_w as i32;
            fill_rect(
                out,
                width,
                height,
                gutter_x,
                y,
                gutter_w,
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
            let cue_w = ((cell_width / 2).clamp(3, 5)) as usize;
            let y = row_top.floor() as i32;
            let h = row_height.ceil().max(1.0) as usize;
            // Keep disclosure in the pad, left of text (and left of severity when both exist).
            let gutter_w = ((cell_width / 3).clamp(2, 4)) as i32;
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
#[cfg(test)]
mod tests {
    use super::fonts::{glyph_baseline_y, glyph_has_ink, GLYPH_CACHE_CAP};
    use super::*;
    use crate::color_emoji::EMOJI_CACHE_CAP;
    use crate::core::types::TextSegment;
    use crate::core::visible::{compile_search_pattern, highlight_search_in_segments};
    use crate::viewport_layout::TextPos;
    use fontdue::Font;

    #[test]
    fn drawable_text_strips_embedded_ansi() {
        assert_eq!(drawable_text("\u{1b}[32mhello\u{1b}[0m"), "hello");
    }

    /// Issue #238: the caret column is a display cell — `⏱️` (U+23F1 + U+FE0F)
    /// occupies one cell, so the caret block under cell 3 must sit at the same
    /// x as the glyph the draw path paints there (3 cells from the base).
    #[test]
    fn caret_pixel_pos_uses_display_cells() {
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: "ab\u{23F1}\u{FE0F}cd".to_string(),
                style: None,
            }],
            raw: "ab\u{23F1}\u{FE0F}cd".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let lines = vec![line];
        let visual = vec![crate::viewport_layout::VisualLine {
            flat_index: 0,
            start: 0,
            end: lines[0].raw.len(),
        }];
        let cell = 8u32;
        // Caret on 'c' (cell 3; VS16 shifts nothing).
        let (x, _) = caret_pixel_pos(
            &lines,
            &visual,
            ViewportCaret {
                flat_index: 0,
                col: 3,
            },
            0,
            0.0,
            LEFT_PAD as i32,
            16.0,
            cell,
            100,
        )
        .expect("caret visible");
        assert_eq!(x, LEFT_PAD as i32 + 3 * cell as i32);
        // Past the line end the caret parks `caret.col` cells from the base
        // (same overflow semantics as before; the base is now display cells).
        let (x, _) = caret_pixel_pos(
            &lines,
            &visual,
            ViewportCaret {
                flat_index: 0,
                col: 9,
            },
            0,
            0.0,
            LEFT_PAD as i32,
            16.0,
            cell,
            100,
        )
        .expect("caret visible");
        assert_eq!(x, LEFT_PAD as i32 + 9 * cell as i32);
    }

    /// The Layout-free draw path must place glyphs exactly where a one-line
    /// fontdue `Layout` did (issue #61): same bitmap top and same coverage.
    #[test]
    fn glyph_placement_matches_fontdue_layout() {
        use fontdue::layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle};
        let font = load_mono_font();
        for size in [12.0_f32, 14.0, 20.0] {
            for ch in ['A', 'g', 'ж', '日', '⚡', '⠋'] {
                let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
                layout.reset(&LayoutSettings {
                    x: 0.0,
                    y: 37.5,
                    ..Default::default()
                });
                layout.append(&[&*font], &TextStyle::new(&ch.to_string(), size, 0));
                let glyph = layout.glyphs().first().expect("layout glyph");
                let laid_y = glyph.y as i32;
                let metrics = font.metrics_indexed(font.lookup_glyph_index(ch), size);
                let baseline = glyph_baseline_y(&font, size, 37.5);
                let computed_y =
                    (baseline + (-metrics.bounds.height - metrics.bounds.ymin).floor()) as i32;
                assert_eq!(laid_y, computed_y, "y mismatch for {ch} @{size}");

                // Coverage parity: same rasterizer config, same bitmap.
                let (laid_metrics, laid_bitmap) =
                    font.rasterize_indexed(font.lookup_glyph_index(ch), size);
                let mine = font.rasterize_indexed(font.lookup_glyph_index(ch), size);
                assert_eq!(&laid_bitmap, &mine.1, "bitmap mismatch for {ch} @{size}");
                assert_eq!(laid_metrics.width, mine.0.width);
                assert_eq!(laid_metrics.height, mine.0.height);
            }
        }
    }

    /// Ink-probe answers must be memoized (issue #61): repeated picks reuse
    /// the cache instead of re-rasterizing per frame.
    #[test]
    fn font_stack_pick_memoizes_ink_probe() {
        let fonts = FontStack::new(load_mono_font(), load_emoji_fallback_font());
        let first = fonts.pick('⠋') as *const Font;
        let second = fonts.pick('⠋') as *const Font;
        assert_eq!(first, second, "pick must be stable");
        {
            let probe = fonts.ink_probe.borrow();
            assert!(probe.contains_key(&'⠋'), "probe result must be cached");
        }
    }

    /// GlyphCache must stay bounded (issue #61): inserting past the cap
    /// evicts instead of growing without limit.
    #[test]
    fn glyph_cache_is_bounded() {
        let font = load_mono_font();
        let glyph = font.lookup_glyph_index('A');
        let mut cache = GlyphCache::new();
        for i in 0..(GLYPH_CACHE_CAP + 512) {
            // Distinct px values give distinct cache keys without needing
            // 16k distinct glyphs in the font.
            cache.rasterize_indexed(&font, glyph, 1.0 + i as f32 * 0.001);
            assert!(cache.entries.len() <= GLYPH_CACHE_CAP);
        }
    }

    /// ColorEmojiAtlas memo must stay bounded (issue #61).
    #[test]
    fn color_emoji_cache_is_bounded() {
        let Some(atlas) = ColorEmojiAtlas::load() else {
            return; // no system emoji font in CI — nothing to bound
        };
        // ZWJ clusters get distinct keys; push well past the cap.
        for i in 0..(EMOJI_CACHE_CAP + 32) {
            let seq = format!("\u{1F600}\u{200D}{i}");
            let _ = atlas.glyph_cluster(&seq);
            let cache = atlas.cache.lock().unwrap();
            assert!(cache.len() <= EMOJI_CACHE_CAP, "cache len {}", cache.len());
        }
    }

    #[test]
    fn text_width_ignores_ansi_bytes() {
        let renderer = ViewportRenderer::new();
        let cell = renderer.metrics.cell_width;
        let plain = text_width("http://localhost:1337", cell);
        let with_ansi = text_width(
            &drawable_text("\u{1b}[32mhttp\u{1b}[0m://localhost:1337"),
            cell,
        );
        assert_eq!(plain, with_ansi);
        assert!(plain > 0);
    }

    #[test]
    fn search_highlight_splits_url_match() {
        let segments = vec![TextSegment {
            text: "http://localhost:1337".to_string(),
            style: None,
        }];
        let pattern = compile_search_pattern("http", false, false, false).unwrap();
        let highlighted = highlight_search_in_segments(&segments, &pattern, Some((0, 4)));
        let joined: String = highlighted.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, "http://localhost:1337");
        assert_eq!(highlighted.len(), 2);
        assert!(highlighted[0]
            .style
            .as_ref()
            .is_some_and(|s| s.search_current));
        assert!(!highlighted[1].style.as_ref().is_some_and(|s| s.search));
    }

    #[test]
    fn render_search_match_on_url() {
        let mut renderer = ViewportRenderer::new();
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: "http://localhost:1337".to_string(),
                style: None,
            }],
            raw: "http://localhost:1337".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let re = compile_search_pattern("http", false, false, false).unwrap();
        let active = SearchMatch {
            line_index: 0,
            start: 0,
            end: 4,
        };
        let mut buf = vec![0u8; 400 * 40 * 4];
        renderer
            .render(
                &mut buf,
                400,
                40,
                &[line],
                0.0,
                0.0,
                false,
                None,
                Some(&re),
                None,
                Some(active),
                None,
            )
            .unwrap();
        // Active match uses orange highlight (R > G, B low).
        let orange_pixels = buf
            .chunks_exact(4)
            .filter(|px| px[0] > 150 && px[1] > 100 && px[1] < 160 && px[2] < 40)
            .count();
        assert!(
            orange_pixels > 20,
            "expected orange search highlight pixels"
        );
    }

    #[test]
    fn render_nowrap_scroll_x_reveals_line_tail() {
        use crate::viewport_layout::max_scroll_x;

        let mut renderer = ViewportRenderer::new();
        let cell = renderer.metrics.cell_width;
        let text = "START__http://localhost:1337/admin/dashboard__END";
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: text.to_string(),
                style: None,
            }],
            raw: text.to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let width = 160u32;
        let height = 40u32;
        let scroll_x = max_scroll_x(&[line.clone()], width, cell);
        assert!(scroll_x > cell as f32, "line should overflow viewport");

        let mut head_buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut head_buf,
                width,
                height,
                &[line.clone()],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let mut tail_buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut tail_buf,
                width,
                height,
                &[line],
                0.0,
                scroll_x,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let head_start = char_column_lit(&head_buf, width, 0, cell, 0.0);
        let head_end_col = text.chars().count().saturating_sub(3);
        let head_end = char_column_lit(&head_buf, width, head_end_col, cell, 0.0);
        let tail_start = char_column_lit(&tail_buf, width, 0, cell, scroll_x);
        let tail_end = char_column_lit(&tail_buf, width, head_end_col, cell, scroll_x);

        assert!(head_start, "expected line start visible at scroll_x=0");
        assert!(!head_end, "expected line end hidden at scroll_x=0");
        assert!(tail_end, "expected line end visible at max scroll_x");
        assert!(!tail_start, "expected line start hidden at max scroll_x");
    }

    fn char_column_lit(buf: &[u8], width: u32, col: usize, cell_width: u32, scroll_x: f32) -> bool {
        let x = crate::viewport_layout::LEFT_PAD as i32 + col as i32 * cell_width as i32
            - scroll_x as i32;
        if x + cell_width as i32 <= 0 || x >= width as i32 {
            return false;
        }
        let start_x = x.max(0) as u32;
        let end_x = (x + cell_width as i32).min(width as i32) as u32;
        let height = buf.len() / ((width * 4) as usize);
        for y in 0..height {
            for px in start_x..end_x {
                let idx = ((y as u32 * width + px) * 4) as usize;
                let px4 = &buf[idx..idx + 4];
                if px4[0] > 10 || px4[1] > 10 || px4[2] > 10 {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn render_emoji_and_symbol_chars() {
        let mut renderer = ViewportRenderer::new();
        let cases = [
            ("To access the server ⚡, go to:", '⚡'),
            ("✔ Cleaning dist dir (6ms)", '✔'),
            ("⠋ Building...", '⠋'),
        ];
        for (text, marker) in cases {
            let line = FlatLine {
                record_id: 1,
                line_index: 0,
                segments: vec![TextSegment {
                    text: text.to_string(),
                    style: None,
                }],
                raw: text.to_string(),
                level: None,
                collapsible: false,
                collapsed: false,
                hidden_line_count: 0,
            };
            let mut buf = vec![0u8; 600 * 40 * 4];
            renderer
                .render(
                    &mut buf,
                    600,
                    40,
                    &[line],
                    0.0,
                    0.0,
                    false,
                    None,
                    None,
                    None,
                    None,
                    None,
                )
                .unwrap();
            let lit = buf
                .chunks_exact(4)
                .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
                .count();
            assert!(
                lit > 30,
                "expected visible pixels for {text:?} (marker {marker}), got {lit}"
            );
            assert_no_horizontal_stripe_artifacts(&buf, 600, 40);
        }
    }

    #[test]
    fn render_color_rocket_emoji_when_noto_available() {
        let mut renderer = ViewportRenderer::new();
        if renderer.color_emoji.is_none() {
            eprintln!("skip: Noto Color Emoji not installed");
            return;
        }
        let text = "🚀 launch";
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: text.to_string(),
                style: None,
            }],
            raw: text.to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let width = 200u32;
        let height = 40u32;
        let mut buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf,
                width,
                height,
                &[line],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        // Color emoji should contribute chromatic (non-gray) pixels, not tofu/empty.
        let colorful = buf
            .chunks_exact(4)
            .filter(|px| {
                let [r, g, b, _] = [px[0], px[1], px[2], px[3]];
                let max = r.max(g).max(b);
                let min = r.min(g).min(b);
                max > 40 && (max - min) > 20
            })
            .count();
        assert!(
            colorful > 20,
            "expected colored rocket pixels when Noto Color Emoji is present, got {colorful}"
        );
    }

    #[test]
    fn render_color_stopwatch_when_noto_available() {
        let mut renderer = ViewportRenderer::new();
        if renderer.color_emoji.is_none() {
            eprintln!("skip: Noto Color Emoji not installed");
            return;
        }
        // U+23F1 lives in Miscellaneous Technical — must not fall through to Symbols2.
        // Sample app.js emits ⏱️ as U+23F1 + U+FE0F.
        let text = "\u{23F1}\u{FE0F} 2.1s";
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: text.to_string(),
                style: None,
            }],
            raw: text.to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let width = 200u32;
        let height = 40u32;
        let mut buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf,
                width,
                height,
                &[line],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let colorful = buf
            .chunks_exact(4)
            .filter(|px| {
                let [r, g, b, _] = [px[0], px[1], px[2], px[3]];
                let max = r.max(g).max(b);
                let min = r.min(g).min(b);
                max > 40 && (max - min) > 20
            })
            .count();
        assert!(
            colorful > 20,
            "expected colored stopwatch pixels (not Symbols2 wireframe), got {colorful}"
        );
    }

    #[test]
    fn variation_selector_16_does_not_draw_tofu_cell() {
        let mut renderer = ViewportRenderer::new();
        let cell = renderer.metrics.cell_width;
        // Sample: console.time(`⏱️ …`) → U+23F1 + U+FE0F
        assert_eq!(
            text_width("\u{23F1}\u{FE0F}", cell),
            text_width("\u{23F1}", cell)
        );
        assert_eq!(text_width("\u{FE0F}", cell), 0);

        // FE0F alone must not produce a tofu box.
        let vs_only = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: "\u{FE0F}".to_string(),
                style: None,
            }],
            raw: "\u{FE0F}".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let width = 120u32;
        let height = 40u32;
        let mut buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf,
                width,
                height,
                &[vs_only],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let lit = buf
            .chunks_exact(4)
            .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
            .count();
        assert_eq!(
            lit, 0,
            "FE0F alone must not rasterize tofu, got {lit} lit pixels"
        );

        // Marker after emoji+VS16 lands in the same cell as after bare emoji.
        let with_vs = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: "\u{23F1}\u{FE0F}#".to_string(),
                style: None,
            }],
            raw: "\u{23F1}\u{FE0F}#".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let bare = FlatLine {
            record_id: 2,
            line_index: 0,
            segments: vec![TextSegment {
                text: "\u{23F1}#".to_string(),
                style: None,
            }],
            raw: "\u{23F1}#".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let mut buf_vs = vec![0u8; (width * height * 4) as usize];
        let mut buf_bare = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf_vs,
                width,
                height,
                &[with_vs],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        renderer
            .render(
                &mut buf_bare,
                width,
                height,
                &[bare],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let marker_col = 1usize; // display cell after the emoji
        assert!(
            char_column_lit(&buf_vs, width, marker_col, cell, 0.0),
            "expected '#' after emoji+VS16 in display cell 1"
        );
        assert!(
            char_column_lit(&buf_bare, width, marker_col, cell, 0.0),
            "expected '#' after bare emoji in display cell 1"
        );
    }

    #[test]
    fn combining_mark_overlays_base_cell_and_does_not_advance() {
        let mut renderer = ViewportRenderer::new();
        let cell = renderer.metrics.cell_width;
        // "a" + U+0301 + "#": the '#' must land in display cell 1 (mark overlays cell 0).
        let combined = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: "a\u{0301}#".to_string(),
                style: None,
            }],
            raw: "a\u{0301}#".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let bare = FlatLine {
            record_id: 2,
            line_index: 0,
            segments: vec![TextSegment {
                text: "a#".to_string(),
                style: None,
            }],
            raw: "a#".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let width = 120u32;
        let height = 40u32;
        let mut buf_combined = vec![0u8; (width * height * 4) as usize];
        let mut buf_bare = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf_combined,
                width,
                height,
                &[combined],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        renderer
            .render(
                &mut buf_bare,
                width,
                height,
                &[bare],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(
            char_column_lit(&buf_combined, width, 0, cell, 0.0),
            "base 'a' cell must have ink"
        );
        assert!(
            char_column_lit(&buf_combined, width, 1, cell, 0.0),
            "expected '#' in display cell 1 after combining mark"
        );
        assert!(
            !char_column_lit(&buf_combined, width, 2, cell, 0.0),
            "combining mark must not spill into cell 2 (tofu advance)"
        );
        // Arabic: base + fatha + shadda renders, no extra cell.
        let arabic = FlatLine {
            record_id: 3,
            line_index: 0,
            segments: vec![TextSegment {
                text: "\u{0627}\u{064E}\u{0651}".to_string(),
                style: None,
            }],
            raw: "\u{0627}\u{064E}\u{0651}".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let mut buf_ar = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf_ar,
                width,
                height,
                &[arabic],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(
            char_column_lit(&buf_ar, width, 0, cell, 0.0),
            "Arabic base cell must have ink"
        );
        assert!(
            !char_column_lit(&buf_ar, width, 1, cell, 0.0),
            "harakat must not occupy their own cells"
        );
    }

    #[test]
    fn flag_pair_spans_two_cells_and_marker_lands_after() {
        let mut renderer = ViewportRenderer::new();
        let cell = renderer.metrics.cell_width;
        // US flag + '#': '#' must land in display cell 2 (flag spans 2 cells).
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: "\u{1F1FA}\u{1F1F8}#".to_string(),
                style: None,
            }],
            raw: "\u{1F1FA}\u{1F1F8}#".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let width = 120u32;
        let height = 40u32;
        let mut buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf,
                width,
                height,
                &[line],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert!(
            char_column_lit(&buf, width, 0, cell, 0.0),
            "flag must paint its first cell"
        );
        assert!(
            char_column_lit(&buf, width, 2, cell, 0.0),
            "expected '#' in display cell 2 after a 2-cell flag"
        );
        assert!(
            !char_column_lit(&buf, width, 3, cell, 0.0),
            "flag must not occupy a third cell"
        );
    }

    #[test]
    fn emoji_fallback_font_loads() {
        let fonts = FontStack::new(load_mono_font(), load_emoji_fallback_font());
        assert!(
            fonts.fallback.is_some(),
            "expected emoji/symbol fallback font"
        );
        let fb = fonts.fallback.as_ref().unwrap();
        for ch in ['⚡', '✔', '⠋'] {
            assert!(
                fb.has_glyph(ch) && glyph_has_ink(fb, ch),
                "fallback should rasterize {ch}"
            );
        }
        // Box drawing stays on the primary monospace font.
        assert!(fonts.primary.has_glyph('│'));
    }

    #[test]
    fn render_multiple_lines_have_vertical_glyph_spread() {
        let mut renderer = ViewportRenderer::new();
        let lines: Vec<FlatLine> = (0..5)
            .map(|i| {
                let i = i as usize;
                FlatLine {
                    record_id: i as u64,
                    line_index: i,
                    segments: vec![TextSegment {
                        text: format!("log line {i}: hello world"),
                        style: None,
                    }],
                    raw: format!("log line {i}: hello world"),
                    level: None,
                    collapsible: false,
                    collapsed: false,
                    hidden_line_count: 0,
                }
            })
            .collect();
        let width = 400u32;
        let height = 120u32;
        let mut buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf, width, height, &lines, 0.0, 0.0, false, None, None, None, None, None,
            )
            .unwrap();

        let text_rows = rows_with_text_pixels(&buf, width, height);
        assert!(
            text_rows.len() >= 3,
            "expected glyphs on multiple rows, got {} lit rows: {:?}",
            text_rows.len(),
            text_rows
        );
        let max_row_pixels = text_rows
            .iter()
            .map(|row| count_lit_pixels_on_row(&buf, width, *row))
            .max()
            .unwrap_or(0);
        assert!(
            max_row_pixels > 20,
            "expected solid glyph rows, not 1px stripes (max row pixels={max_row_pixels})"
        );
        assert_no_horizontal_stripe_artifacts(&buf, width, height);
    }

    #[test]
    fn render_search_match_shows_text_not_only_background() {
        let mut renderer = ViewportRenderer::new();
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: "http://localhost:1337".to_string(),
                style: None,
            }],
            raw: "http://localhost:1337".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let re = compile_search_pattern("http", false, false, false).unwrap();
        let active = SearchMatch {
            line_index: 0,
            start: 0,
            end: 4,
        };
        let width = 400u32;
        let height = 40u32;
        let mut buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf,
                width,
                height,
                &[line],
                0.0,
                0.0,
                false,
                None,
                Some(&re),
                None,
                Some(active),
                None,
            )
            .unwrap();

        let orange_pixels = buf
            .chunks_exact(4)
            .filter(|px| px[0] > 150 && px[1] > 100 && px[1] < 160 && px[2] < 40)
            .count();
        assert!(
            orange_pixels > 20,
            "expected orange search highlight pixels"
        );

        let text_pixels = buf
            .chunks_exact(4)
            .filter(|px| {
                // Default foreground (not black background, not orange highlight)
                px[0] > 180 && px[1] > 200 && px[2] > 200
            })
            .count();
        assert!(
            text_pixels > 30,
            "expected visible foreground text pixels, got {text_pixels}"
        );
        assert_no_horizontal_stripe_artifacts(&buf, width, height);
    }

    #[test]
    fn render_box_drawing_vertical_bars_align_across_rows() {
        let mut renderer = ViewportRenderer::new();
        let width = 800u32;
        let height = 80u32;
        let row_stride = renderer.metrics.row_stride;

        let rows = [
            "┌──────────┬──────────────────────────────────────────┐",
            "│ Time     │ Fri Jul 10 2026 12:07:46 GMT+0300        │",
            "│ Launched │ 2333 ms                                  │",
            "└──────────┴──────────────────────────────────────────┘",
        ];

        let flat_lines: Vec<FlatLine> = rows
            .iter()
            .enumerate()
            .map(|(i, text)| FlatLine {
                record_id: i as u64,
                line_index: i,
                segments: vec![TextSegment {
                    text: (*text).to_string(),
                    style: None,
                }],
                raw: text.to_string(),
                level: None,
                collapsible: false,
                collapsed: false,
                hidden_line_count: 0,
            })
            .collect();

        let mut buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf,
                width,
                height,
                &flat_lines,
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        // Middle divider is at column 11 on every row (after 10-cell-wide left column).
        let bar_cols = [0usize, 11];
        let cell_width = renderer.metrics.cell_width;
        let base_x = 8u32;

        for row in 0..3 {
            let row_top = (row as f32 * row_stride).round() as u32;
            let row_bottom = ((row as f32 + 1.0) * row_stride).round() as u32;
            for &col in &bar_cols {
                let cell_x = base_x + col as u32 * cell_width;
                let lit_x = find_lit_x_in_column(&buf, width, height, cell_x, row_top, row_bottom);
                assert!(
                    lit_x.is_some(),
                    "row {row} missing bar ink near column {col} (cell_x={cell_x})"
                );
                let x = lit_x.unwrap();
                assert!(
                    x >= cell_x as i32 && x < (cell_x + cell_width) as i32,
                    "row {row} col {col}: bar ink at x={x} outside cell [{cell_x}, {})",
                    cell_x + cell_width
                );
            }
        }

        // Data rows (1-2) use │ at the same columns — ink x must match exactly.
        for &col in &bar_cols {
            let cell_x = base_x + col as u32 * cell_width;
            let mut xs = Vec::new();
            for row in 1..3 {
                let row_top = (row as f32 * row_stride).round() as u32;
                let row_bottom = ((row as f32 + 1.0) * row_stride).round() as u32;
                if let Some(x) =
                    find_lit_x_in_column(&buf, width, height, cell_x, row_top, row_bottom)
                {
                    xs.push(x);
                }
            }
            assert_eq!(
                xs.len(),
                2,
                "expected │ ink on both data rows at column {col}"
            );
            assert_eq!(
                xs[0], xs[1],
                "│ column {col} jumped between data rows: {:?}",
                xs
            );
        }
    }

    /// Find the leftmost lit pixel in a cell column on a row (for box-drawing bar alignment).
    fn find_lit_x_in_column(
        buf: &[u8],
        width: u32,
        height: u32,
        cell_x: u32,
        row_top: u32,
        row_bottom: u32,
    ) -> Option<i32> {
        let cell_w = 8u32;
        let x_end = (cell_x + cell_w).min(width);
        let y_start = row_top.min(height);
        let y_end = row_bottom.min(height);
        for y in y_start..y_end {
            for x in cell_x..x_end {
                let idx = ((y * width + x) * 4) as usize;
                let px = &buf[idx..idx + 4];
                if px[0] > 10 || px[1] > 10 || px[2] > 10 {
                    return Some(x as i32);
                }
            }
        }
        None
    }

    #[test]
    fn render_strapi_table_lines_visible() {
        use crate::core::ansi::{parse_ansi_line, strip_ansi};
        use crate::core::types::FlatLine;

        let mut renderer = ViewportRenderer::new();
        let colored = "\u{1b}[90m│\u{1b}[39m \u{1b}[34mTime\u{1b}[39m               \u{1b}[90m│\u{1b}[39m Fri Jul 10 2026 12:07:46 GMT+0300 \u{1b}[90m│\u{1b}[39m";
        let plain = strip_ansi(colored);
        let segments = parse_ansi_line(colored);
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments,
            raw: plain.clone(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };

        let width = 800u32;
        let height = 40u32;
        let mut buf = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf,
                width,
                height,
                &[line],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();

        let lit = buf
            .chunks_exact(4)
            .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
            .count();
        assert!(
            lit > 50,
            "table row should render visible pixels, got {lit} for {plain:?}"
        );
        let border_only = FlatLine {
            record_id: 2,
            line_index: 0,
            segments: vec![TextSegment {
                text: "╭────────────────────┬──────────────────────────────────────────────────╮"
                    .to_string(),
                style: None,
            }],
            raw: "╭────────────────────┬──────────────────────────────────────────────────╮"
                .to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let mut buf2 = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut buf2,
                width,
                height,
                &[border_only],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        let lit_border = buf2
            .chunks_exact(4)
            .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
            .count();
        assert!(
            lit_border > 30,
            "box-drawing border should render, got {lit_border} pixels"
        );
    }

    fn rows_with_text_pixels(buf: &[u8], width: u32, height: u32) -> Vec<u32> {
        let mut rows = Vec::new();
        for y in 0..height {
            if count_lit_pixels_on_row(buf, width, y) > 5 {
                rows.push(y);
            }
        }
        rows
    }

    fn count_lit_pixels_on_row(buf: &[u8], width: u32, y: u32) -> usize {
        let start = (y * width * 4) as usize;
        let end = start + (width as usize) * 4;
        buf[start..end]
            .chunks_exact(4)
            .filter(|px| px[0] > 10 || px[1] > 10 || px[2] > 10)
            .count()
    }

    /// Detect the regression where only search/row backgrounds render as thin horizontal bars.
    fn assert_no_horizontal_stripe_artifacts(buf: &[u8], width: u32, height: u32) {
        let text_rows = rows_with_text_pixels(buf, width, height);
        assert!(
            !text_rows.is_empty(),
            "viewport rendered no visible pixels at all"
        );

        let mut thin_rows = 0usize;
        for &y in &text_rows {
            let lit = count_lit_pixels_on_row(buf, width, y);
            if lit <= 3 {
                thin_rows += 1;
            }
        }
        assert!(
            thin_rows < text_rows.len(),
            "viewport looks like horizontal stripes: {thin_rows}/{} lit rows are 1-3px tall",
            text_rows.len()
        );
    }

    #[test]
    fn render_draws_block_caret_past_line_end() {
        let mut renderer = ViewportRenderer::new();
        let line = FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![crate::core::types::TextSegment {
                text: "$ ".to_string(),
                style: None,
            }],
            raw: "$ ".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        };
        let width = 200u32;
        let height = 40u32;
        let cell = renderer.metrics.cell_width;
        let mut with_caret = vec![0u8; (width * height * 4) as usize];
        let mut without = vec![0u8; (width * height * 4) as usize];
        renderer
            .render(
                &mut without,
                width,
                height,
                &[line.clone()],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                None,
            )
            .unwrap();
        renderer
            .render(
                &mut with_caret,
                width,
                height,
                &[line],
                0.0,
                0.0,
                false,
                None,
                None,
                None,
                None,
                Some(ViewportCaret {
                    flat_index: 0,
                    col: 5,
                }),
            )
            .unwrap();

        let caret_x = 8 + 5 * cell;
        let mut caret_lit = 0usize;
        let mut diff = 0usize;
        for y in 0..height {
            for x in caret_x..caret_x + cell {
                let i = ((y * width + x) * 4) as usize;
                if with_caret[i] != without[i]
                    || with_caret[i + 1] != without[i + 1]
                    || with_caret[i + 2] != without[i + 2]
                {
                    diff += 1;
                }
                if with_caret[i] > 40 || with_caret[i + 1] > 40 || with_caret[i + 2] > 40 {
                    caret_lit += 1;
                }
            }
        }
        assert!(
            diff > 10,
            "caret should change pixels at col 5, diff={diff}"
        );
        assert!(
            caret_lit > 10,
            "caret cell should be visible, lit={caret_lit}"
        );
    }

    #[test]
    fn set_font_size_rebuilds_metrics() {
        let mut renderer = ViewportRenderer::new();
        assert!((renderer.font_size() - 13.0).abs() < 0.01);
        let baseline = renderer.metrics().row_stride;
        let baseline_cell = renderer.metrics().cell_width;

        renderer.set_font_size(24.0);
        assert!((renderer.font_size() - 24.0).abs() < 0.01);
        assert!(renderer.metrics().row_stride > baseline);
        assert!(renderer.metrics().cell_width >= baseline_cell);

        renderer.set_font_size(8.0);
        assert!((renderer.font_size() - 8.0).abs() < 0.01);
        assert!(renderer.metrics().row_stride < baseline);

        renderer.set_font_size(100.0);
        assert!((renderer.font_size() - 32.0).abs() < 0.01);
    }

    // Issue #235: a selection that survived a buffer swap can hold stale byte
    // offsets landing mid-character; the highlight path used to slice without
    // the char-boundary guard selection_plain_text has and panicked.
    #[test]
    fn highlight_selection_tolerates_stale_mid_char_offsets() {
        let text = "日本語です"; // 2 bytes per char, 10 bytes total
        let segments = vec![TextSegment {
            text: text.to_string(),
            style: None,
        }];
        let sel = TextSelection::new(
            TextPos {
                line_index: 0,
                byte_offset: 1,
            }, // mid-char (stale)
            TextPos {
                line_index: 0,
                byte_offset: 5,
            }, // mid-char (stale)
        );
        let out = highlight_selection_in_segments(&segments, &sel, 0, 0, text.len());
        let joined: String = out.iter().map(|s| s.text.as_str()).collect();
        assert_eq!(joined, text, "highlight must not drop or corrupt chars");
        assert!(out
            .iter()
            .any(|s| s.style.as_ref().is_some_and(|st| st.selected)));
    }
}
