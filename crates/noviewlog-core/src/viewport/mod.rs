//! Viewport bitmap paint: fontdue renderer + FlatLine row helpers.

use crate::color_emoji::ColorEmojiAtlas;
use crate::core::types::{
    clamp_viewport_font_size, FlatLine, SearchMatch, DEFAULT_VIEWPORT_FONT_SIZE,
};
use crate::core::visible::SearchPattern;
use crate::viewport_layout::{collect_visible_visual_lines_with_total, TextSelection, LEFT_PAD};

mod fonts;
mod paint;
mod primitives;

#[cfg(test)]
mod tests;

use fonts::{compute_metrics, load_emoji_fallback_font, load_mono_font, FontStack, GlyphCache};
use paint::{draw_caret_block, draw_visual_line};
use primitives::{draw_text, BG, HINT_FG};

pub use paint::caret_pixel_pos;

/// Block caret in the rendered viewport (flat-line index + cell column).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewportCaret {
    pub flat_index: usize,
    pub col: usize,
}

pub struct ViewportMetrics {
    /// Paint / caret box height in pixels (currently identical to [`Self::row_stride`]).
    pub row_height: f32,
    /// Vertical scroll advance per visual row in pixels (same value as [`Self::row_height`] today).
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
        for px in out[..expected].as_chunks_mut::<4>().0 {
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
        for px in out[..expected].as_chunks_mut::<4>().0 {
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
