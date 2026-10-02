//! Terminal block-caret geometry: visibility, overlay rect, live-grid follow scroll, blink tick.

use super::*;

impl Engine {
    pub fn terminal_caret_active(&self) -> bool {
        self.viewport_focused
            && self.has_active_terminal()
            && self.active_terminal().running
            && self.active_terminal().active_view == 0
            && self.active_terminal().ingest.viewport_caret().is_some()
    }

    /// Device-pixel block caret rect `(x, y, w, h)` for the Slint overlay, or `None`
    /// when the Terminal tab cannot accept input or the caret is off-screen.
    pub fn terminal_caret_rect(&self, width: u32, height: u32) -> Option<(f32, f32, f32, f32)> {
        if !self.terminal_caret_active() || width == 0 || height == 0 {
            return None;
        }
        let metrics = self.renderer.metrics();
        if self.paints_live_vt_grid() {
            // Same wrap + caret-pinned scroll as live-grid paint. Bare `row * stride`
            // ignored scroll_y after WRAP, so the overlay crawled above the prompt.
            let wrap = self.active_view().wrap_lines;
            let (row, col) = self.active_terminal().ingest.grid_caret()?;
            let lines = self.active_terminal().ingest.grid_flat_lines();
            let (scroll_y, visual_rows) =
                self.live_vt_grid_follow_scroll(&lines, wrap, width, height, row);
            let first_row = (scroll_y / metrics.row_stride).floor() as usize;
            let y_offset = scroll_y - first_row as f32 * metrics.row_stride;
            let max_rows = (height as f32 / metrics.row_stride).ceil() as usize + 1;
            let visual = crate::viewport_layout::collect_visible_visual_lines_with_total(
                &lines,
                wrap,
                width,
                metrics.cell_width,
                first_row,
                max_rows,
                Some(visual_rows),
                None,
            );
            let caret = crate::viewport::ViewportCaret {
                flat_index: row,
                col,
            };
            let (cx, cy) = crate::viewport::caret_pixel_pos(
                &lines,
                &visual,
                caret,
                0,
                y_offset,
                LEFT_PAD as i32,
                metrics.row_stride,
                metrics.cell_width,
                height,
            )?;
            let w = metrics.cell_width.max(1) as f32;
            let h = metrics.row_height.max(1.0);
            return Some((cx as f32, cy, w, h));
        }
        let terminal = self.active_terminal();
        let view = terminal.active_view();
        let flat_lines = view.flat_lines.as_ref();
        let wrap_lines = view.wrap_lines;
        let scroll_x = if wrap_lines { 0.0 } else { terminal.scroll_x };
        let mut scroll_y = terminal.scroll_offset_y;
        let metrics = self.renderer.metrics();
        let rows = view.cached_visual_rows(width, metrics.cell_width, count_visual_rows);
        if view.auto_follow && view.search_query.is_empty() && !terminal.is_file_session() {
            let content_h = rows as f32 * metrics.row_stride;
            scroll_y = (content_h - height as f32).max(0.0);
        }
        let screen = terminal.ingest.viewport_caret()?;
        let base = flat_lines
            .len()
            .saturating_sub(terminal.ingest.volatile_count());
        let caret = crate::viewport::ViewportCaret {
            flat_index: base.saturating_add(screen.line),
            col: screen.col,
        };
        let first_row = (scroll_y / metrics.row_stride).floor() as usize;
        let y_offset = scroll_y - first_row as f32 * metrics.row_stride;
        let x_base = if wrap_lines {
            LEFT_PAD as i32
        } else {
            LEFT_PAD as i32 - scroll_x as i32
        };
        let max_rows = (height as f32 / metrics.row_stride).ceil() as usize + 1;
        let index = view.ensure_visual_row_index(width, metrics.cell_width);
        let visual = crate::viewport_layout::collect_visible_visual_lines_with_total(
            flat_lines,
            wrap_lines,
            width,
            metrics.cell_width,
            first_row,
            max_rows,
            Some(index.total_rows()),
            Some(index.as_ref()),
        );
        let (cx, cy) = crate::viewport::caret_pixel_pos(
            flat_lines,
            &visual,
            caret,
            0,
            y_offset,
            x_base,
            metrics.row_stride,
            metrics.cell_width,
            height,
        )?;
        let w = metrics.cell_width.max(1) as f32;
        let h = metrics.row_height.max(1.0);
        Some((cx as f32, cy, w, h))
    }

    /// Scroll Y for Follow live-grid paint/caret: pin so the caret's flat line
    /// stays in view (bottom-aligned). Avoids scrolling blank PTY rows below a
    /// home cursor off the top of the viewport.
    pub(crate) fn live_vt_grid_follow_scroll(
        &self,
        lines: &[crate::core::types::FlatLine],
        wrap: bool,
        width: u32,
        height: u32,
        caret_row: usize,
    ) -> (f32, usize) {
        let metrics = self.renderer.metrics();
        let stride = metrics.row_stride;
        let index = VisualRowIndex::rebuild(lines, wrap, width, metrics.cell_width);
        let visual_rows = index.total_rows();
        let max_scroll = (visual_rows as f32 * stride - height as f32).max(0.0);
        let caret_line = caret_row.min(lines.len().saturating_sub(1));
        let line_end = index.visual_end_of_flat(caret_line).max(1);
        let scroll_y = (line_end as f32 * stride - height as f32)
            .max(0.0)
            .min(max_scroll);
        (scroll_y, visual_rows)
    }

    /// Host owns blink phase; engine no longer dirties the viewport for caret blink.
    pub(crate) fn tick_caret_blink(&mut self) {
        // retained for tick() call site stability — blink is Slint-side now
    }

    /// TUI hosts: caret cell of the live VT grid, relative to the visible
    /// slice of `rows` flat lines (same tail-pinning as
    /// [`Engine::visible_flat_lines`]). `None` when there is no live caret
    /// (stopped session, file session, non-live filter views).
    pub fn caret_visible_pos(&self, rows: usize) -> Option<(usize, usize)> {
        if rows == 0 || !self.has_active_terminal() || !self.paints_live_vt_grid() {
            return None;
        }
        let terminal = self.active_terminal();
        let total = terminal.ingest.grid_flat_lines().len();
        let start = total.saturating_sub(rows);
        let (r, c) = terminal.ingest.grid_caret()?;
        Some((r.saturating_sub(start), c))
    }
}
