//! Viewport rendering: dirty tracking, paint, PTY geometry sync, font size.

use super::*;

impl Engine {
    pub fn needs_render(&self) -> bool {
        // Paint only when something actually dirtied the viewport.
        // Live follow must not force perpetual redraw: PTY ingest, rebuild,
        // caret blink, scroll, resize, and tab/terminal switches call
        // mark_viewport_dirty() when content or chrome changes.
        self.viewport_dirty
    }

    pub(crate) fn mark_viewport_dirty(&mut self) {
        self.viewport_dirty = true;
    }

    /// Under continuous PTY flood, dirty at most once per [`VIEWPORT_PAINT_MIN_INTERVAL`]
    /// since the last paint. When the flood queue is empty after ingest (echo / catch-up),
    /// always dirty so the tail becomes visible promptly.
    ///
    /// Follow scroll MUST already have been snapped on this ingest — skipping paint must
    /// not leave a stale `scroll_offset_y`.
    pub(crate) fn mark_viewport_dirty_after_pty_ingest(&mut self, more_pending: bool) {
        if !more_pending {
            self.mark_viewport_dirty();
            return;
        }
        if self.viewport_dirty {
            return;
        }
        let due = match self.last_viewport_paint_at {
            None => true,
            Some(t) => t.elapsed() >= VIEWPORT_PAINT_MIN_INTERVAL,
        };
        if due {
            self.mark_viewport_dirty();
        }
    }

    /// Host calls after a successful Viewport Image upload (or after `render` in tests).
    pub fn note_viewport_painted(&mut self) {
        self.last_viewport_paint_at = Some(Instant::now());
    }

    /// Character grid size for the PTY / VT emulator.
    ///
    /// Rows track the viewport. Cols are `max(viewport_cols, MIN_PTY_COLS)` so a
    /// wide window is not capped at the old fixed 120, while a narrow window
    /// still gets a wide logical line buffer for soft-wrap / horizontal scroll.
    pub(crate) fn set_viewport_focus(&mut self, focused: bool) {
        if self.viewport_focused == focused {
            return;
        }
        self.viewport_focused = focused;
        // Overlay caret is host-drawn; content paint is unchanged by focus alone.
        // Unfocus: no need to dirty the bitmap (caret overlay hides independently).
        let _ = focused;
    }

    pub(crate) fn viewport_pty_size(&self) -> PtySize {
        let metrics = self.renderer.metrics();
        let viewport_cols = max_cols(content_width(self.viewport_width), metrics.cell_width)
            .clamp(1, u16::MAX as usize) as u16;
        let cols = viewport_cols.max(MIN_PTY_COLS);
        let rows = ((self.viewport_height as f32) / metrics.row_stride.max(1.0))
            .floor()
            .clamp(1.0, u16::MAX as f32) as u16;
        let cell_w = metrics.cell_width.max(1);
        let cell_h = metrics.row_stride.max(1.0).ceil() as u32;
        PtySize {
            cols,
            rows: rows.max(1),
            pixel_width: (cell_w as u32)
                .saturating_mul(cols as u32)
                .min(u16::MAX as u32) as u16,
            pixel_height: cell_h.saturating_mul(rows as u32).min(u16::MAX as u32) as u16,
        }
    }

    /// Keep PTY winsize + terminal emulator cols/rows in sync with the viewport.
    /// Soft-wrap remains display-only (viewport pixels); PTY cols use a wide
    /// floor so child hard-wrap does not steal the Wrap toggle's job.
    pub(crate) fn sync_terminal_geometry(&mut self) {
        let size = self.viewport_pty_size();
        let cols = size.cols as usize;
        let rows = size.rows as usize;
        for pty in self.ptys.values_mut() {
            let _ = pty.set_size(size);
        }
        if self.terminals.is_empty() {
            return;
        }
        let mut any = false;
        for term in &mut self.terminals {
            if term.ingest.size() != (cols, rows) {
                term.ingest
                    .resize(cols, rows, &mut term.buffer, &mut term.parser);
                any = true;
            }
        }
        if any {
            self.mark_all_views_dirty();
            self.mark_viewport_dirty();
        }
    }

    pub fn render(&mut self, width: u32, height: u32, out: &mut [u8]) -> Result<(), String> {
        let size_changed = width != self.viewport_width || height != self.viewport_height;
        self.viewport_width = width;
        self.viewport_height = height;
        if size_changed {
            self.sync_terminal_geometry();
        }
        self.ensure_valid_state();
        if !self.has_active_terminal() {
            self.renderer
                .render_center_message(out, width, height, "No terminal")?;
            self.viewport_dirty = false;
            self.note_viewport_painted();
            return Ok(());
        }

        let scroll_row = {
            let terminal = self.active_terminal_mut();
            terminal.scroll_to_row.take()
        };
        if let Some(row) = scroll_row {
            self.scroll_to_row_index(row);
        }

        // FILES: never paint with local scroll past the loaded window (black frames).
        if self.active_terminal().is_file_session() && self.active_terminal().file_backed.is_some()
        {
            let local_max = self.local_window_max_scroll();
            let terminal = self.active_terminal_mut();
            if terminal.scroll_offset_y > local_max {
                terminal.scroll_offset_y = local_max;
            }
        }

        if self.paints_live_vt_grid() {
            // Native Follow: paint the live screen only. Scroll keeps the caret
            // in view (WRAP may grow visual height); never the capped scrollback ring.
            let wrap = self.active_view().wrap_lines;
            let lines = self.active_terminal().ingest.grid_flat_lines();
            let caret_row = self
                .active_terminal()
                .ingest
                .grid_caret()
                .map(|(r, _)| r)
                .unwrap_or(0);
            let (scroll_y, visual_rows) =
                self.live_vt_grid_follow_scroll(&lines, wrap, width, height, caret_row);
            self.renderer.render_with_total(
                out,
                width,
                height,
                &lines,
                scroll_y,
                0.0,
                wrap,
                None,
                None,
                None,
                None,
                None,
                Some(visual_rows),
                None,
            )?;
            let max = self.max_scroll_offset();
            self.active_terminal_mut().scroll_offset_y = max;
            self.viewport_dirty = false;
            self.note_viewport_painted();
            return Ok(());
        }

        let (
            auto_follow,
            wrap_lines,
            flat_lines,
            search_pattern,
            active_match,
            running,
            scroll_offset_y,
            scroll_x,
            selection,
        ) = {
            let terminal = self.active_terminal();
            let view = terminal.active_view();
            (
                // Find chrome owns search: a live query pins the viewport on matches
                // (no Follow). Closing Find must SearchSet empty or this stays frozen.
                // File sessions never Follow.
                !terminal.is_file_session() && view.auto_follow && view.search_query.is_empty(),
                view.wrap_lines,
                Arc::clone(&view.flat_lines),
                view.search_pattern.clone(),
                view.search_matches.get(view.search_match_index).copied(),
                terminal.running,
                terminal.scroll_offset_y,
                terminal.scroll_x,
                terminal.selection,
            )
        };
        let filter_draft_pattern = self.filter_draft_pattern.clone();

        let mut scroll_offset_y = scroll_offset_y;
        if auto_follow {
            let metrics = self.renderer.metrics();
            let rows =
                self.active_view()
                    .cached_visual_rows(width, metrics.cell_width, count_visual_rows);
            let content_h = rows as f32 * metrics.row_stride;
            let new_scroll = (content_h - height as f32).max(0.0);
            if (new_scroll - scroll_offset_y).abs() > 0.01 {
                self.mark_viewport_dirty();
            }
            scroll_offset_y = new_scroll;
            self.active_terminal_mut().scroll_offset_y = scroll_offset_y;
        } else if !self.active_terminal().is_file_session() {
            // Live-grid Follow uses a taller (ring + screen) range. After
            // leaving Follow, clamp so overlay paint cannot skip the top.
            let local_max = self.local_window_max_scroll();
            if scroll_offset_y > local_max {
                scroll_offset_y = local_max;
                self.active_terminal_mut().scroll_offset_y = scroll_offset_y;
            }
        }

        if !running && flat_lines.is_empty() {
            let terminal = self.active_terminal();
            let view = terminal.active_view();
            let msg = if terminal.is_file_session()
                && view.uses_match_index()
                && view.match_scan_pos.is_some()
            {
                // Same text as the status bar while the whole-file match index builds.
                self.status_message.as_str()
            } else if terminal.is_file_session()
                && view.uses_match_index()
                && view.match_scan_pos.is_none()
            {
                "No matching lines"
            } else if terminal.active_view == 0 {
                EMPTY_TERMINAL_TAB_STOPPED
            } else {
                EMPTY_FILTER_TAB_STOPPED
            };
            self.renderer
                .render_center_message(out, width, height, msg)?;
            self.viewport_dirty = false;
            self.note_viewport_painted();
            return Ok(());
        }
        let effective_scroll_x = if wrap_lines { 0.0 } else { scroll_x };

        let metrics = self.renderer.metrics();
        let index = self
            .active_view()
            .ensure_visual_row_index(width, metrics.cell_width);
        let total_rows = index.total_rows();

        // Caret is drawn by the Slint host overlay — keep the bitmap content-only.
        self.renderer.render_with_total(
            out,
            width,
            height,
            &flat_lines,
            scroll_offset_y,
            effective_scroll_x,
            wrap_lines,
            selection.as_ref(),
            search_pattern.as_ref(),
            filter_draft_pattern.as_ref(),
            active_match,
            None,
            Some(total_rows),
            Some(index.as_ref()),
        )?;
        self.viewport_dirty = false;
        self.note_viewport_painted();
        Ok(())
    }

    /// Visible slice of the active view's flat lines for non-bitmap hosts
    /// (TUI). The host owns painting; this only projects engine state:
    /// Follow (auto_follow, no live search) and the live VT grid pin the
    /// slice to the tail, otherwise `scroll_offset_y` is the first visible
    /// row. Rows are whole flat lines — the TUI host pins wrap off.
    pub fn visible_flat_lines(&self, rows: usize) -> Vec<crate::core::types::FlatLine> {
        if rows == 0 || !self.has_active_terminal() {
            return Vec::new();
        }
        if self.paints_live_vt_grid() {
            let grid = self.active_terminal().ingest.grid_flat_lines();
            let start = grid.len().saturating_sub(rows);
            return grid[start..].to_vec();
        }
        let terminal = self.active_terminal();
        let view = terminal.active_view();
        let follow =
            !terminal.is_file_session() && view.auto_follow && view.search_query.is_empty();
        let total = view.flat_lines.len();
        let first_row = if follow {
            total.saturating_sub(rows)
        } else {
            let stride = self.renderer.metrics().row_stride;
            (terminal.scroll_offset_y / stride).floor().max(0.0) as usize
        };
        let start = first_row.min(total.saturating_sub(1));
        let end = (start + rows).min(total);
        view.flat_lines[start..end].to_vec()
    }

    pub(crate) fn set_viewport_font_size(&mut self, size: f32) {
        let capped = clamp_viewport_font_size(size);
        self.config.viewport_font_size = capped;
        self.renderer.set_font_size(capped);
        self.sync_terminal_geometry();
        // Wrap/scroll layout depends on cell metrics.
        if self.has_active_terminal() {
            for view in &mut self.active_terminal_mut().views {
                view.mark_flat_lines_dirty();
            }
        }
        self.mark_config_dirty();
        self.status_message = format!("Viewport font size: {capped:.0} pt");
        self.push_event(json!({"type":"status","message": self.status_message}));
        self.mark_viewport_dirty();
        self.last_stats_at = None;
    }
}
