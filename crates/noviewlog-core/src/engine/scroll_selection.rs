use super::*;

impl Engine {
    /// Leave Follow live-grid paint and rebuild the WRAP-aware overlay so
    /// scrollbar and wheel share one coordinate space.
    ///
    /// Returns `(was_live, old_max)` so an absolute scrollbar offset can be
    /// remapped from the inflated Follow range onto overlay visual height.
    fn materialize_overlay_for_user_scroll(&mut self) -> (bool, f32) {
        if !self.paints_live_vt_grid() {
            return (false, self.max_scroll_offset());
        }
        let old_max = self.max_scroll_offset();
        self.active_view_mut().auto_follow = false;
        self.materialize_live_terminal_tab();
        self.last_stats_at = None;
        (true, old_max)
    }

    pub(crate) fn scroll_by_lines(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        if !self.active_terminal().is_file_session() {
            let (was_live, _) = self.materialize_overlay_for_user_scroll();
            if was_live {
                // Wheel from Follow starts at the overlay tail (history above
                // the live screen), not in the inflated live-grid range.
                let max = self.max_scroll_offset();
                self.active_terminal_mut().scroll_offset_y = max;
            }
        }
        let row_stride = self.renderer.metrics().row_stride;
        let max_scroll = if self.active_terminal().is_file_session()
            && self.active_terminal().file_backed.is_some()
        {
            self.local_window_max_scroll()
        } else {
            self.max_scroll_offset()
        };
        let terminal = self.active_terminal_mut();
        terminal.scroll_offset_y =
            (terminal.scroll_offset_y + delta as f32 * row_stride).clamp(0.0, max_scroll);
        self.sync_follow_from_scroll();
        self.maybe_prefetch_file_window();
        if self.active_terminal().is_file_session() && self.active_view().uses_match_index() {
            self.apply_match_window();
        }
        self.mark_viewport_dirty();
    }

    pub(crate) fn scroll_page(&mut self, direction: i32) {
        if direction == 0 {
            return;
        }
        if !self.active_terminal().is_file_session() {
            let (was_live, _) = self.materialize_overlay_for_user_scroll();
            if was_live {
                let max = self.max_scroll_offset();
                self.active_terminal_mut().scroll_offset_y = max;
            }
        }
        let page = self.viewport_height as f32 * 0.9;
        let max_scroll = if self.active_terminal().is_file_session()
            && self.active_terminal().file_backed.is_some()
        {
            self.local_window_max_scroll()
        } else {
            self.max_scroll_offset()
        };
        let terminal = self.active_terminal_mut();
        terminal.scroll_offset_y =
            (terminal.scroll_offset_y + direction.signum() as f32 * page).clamp(0.0, max_scroll);
        self.sync_follow_from_scroll();
        self.maybe_prefetch_file_window();
        if self.active_terminal().is_file_session() && self.active_view().uses_match_index() {
            self.apply_match_window();
        }
        self.mark_viewport_dirty();
    }

    /// Absolute scrollbar position (live Terminal overlay, not FILES).
    pub(crate) fn scroll_to_offset(&mut self, offset: f32) {
        let (was_live, old_max) = self.materialize_overlay_for_user_scroll();
        let new_max = self.max_scroll_offset();
        let mapped = if was_live && old_max > 0.5 {
            (offset / old_max) * new_max
        } else {
            offset
        };
        self.active_terminal_mut().scroll_offset_y = mapped.clamp(0.0, new_max);
        self.sync_follow_from_scroll();
        self.maybe_prefetch_file_window();
        self.mark_viewport_dirty();
    }

    pub(crate) fn scroll_to_start(&mut self) {
        self.active_view_mut().auto_follow = false;
        self.last_stats_at = None;
        if self.active_terminal().file_backed.is_some() {
            if self.active_view().uses_match_index() {
                self.scroll_match_to_global_offset(0.0);
            } else {
                self.request_file_window_at(0, 0.0);
            }
        } else {
            self.active_terminal_mut().scroll_offset_y = 0.0;
            self.materialize_live_terminal_tab();
        }
        self.mark_viewport_dirty();
    }

    pub(crate) fn scroll_to_end(&mut self) {
        if self.active_terminal().file_backed.is_some() {
            let max = self.max_scroll_offset();
            if self.active_view().uses_match_index() {
                self.scroll_match_to_global_offset(max);
            } else {
                self.scroll_file_to_global_offset(max);
            }
            self.last_stats_at = None;
            self.mark_viewport_dirty();
            return;
        }
        if !self.active_terminal().is_file_session() {
            self.active_view_mut().auto_follow = true;
        }
        self.active_terminal_mut().scroll_offset_y = self.max_scroll_offset();
        self.last_stats_at = None;
        self.mark_viewport_dirty();
    }

    pub(crate) fn scroll_to_row_index(&mut self, row: usize) {
        self.active_view_mut().auto_follow = false;
        let metrics = self.renderer.metrics();
        // `row` is a flat line index; scroll math is in visual rows. With wrap
        // on, one flat line may span several visual rows (issue #159).
        let visual_row = {
            let view = self.active_view();
            let index = view.ensure_visual_row_index(self.viewport_width, metrics.cell_width);
            index.visual_end_of_flat(row.saturating_sub(1))
        };
        let row_top = visual_row as f32 * metrics.row_stride;
        let row_bottom = row_top + metrics.row_height;
        let viewport_height = self.viewport_height as f32;
        let terminal = self.active_terminal_mut();
        let visible_top = terminal.scroll_offset_y;
        let visible_bottom = terminal.scroll_offset_y + viewport_height;
        if row_top < visible_top {
            terminal.scroll_offset_y = row_top.max(0.0);
        } else if row_bottom > visible_bottom {
            terminal.scroll_offset_y = (row_bottom - viewport_height).max(0.0);
        }
    }

    /// Stick Follow when the viewport is at (or past) the bottom; clear it when scrolled away.
    pub(crate) fn sync_follow_from_scroll(&mut self) {
        if self.active_terminal().is_file_session() {
            if self.active_view().auto_follow {
                self.active_view_mut().auto_follow = false;
                self.last_stats_at = None;
            }
            return;
        }
        let max_scroll = self.max_scroll_offset();
        if max_scroll <= 0.5 {
            // Overlay fits in the viewport. Do not toggle Follow: turning it
            // back on would paint the live VT grid and hide committed
            // scrollback that still fits on screen (scrollbar vs wheel).
            return;
        }
        let scroll_y = self.active_terminal().scroll_offset_y;
        let at_bottom = scroll_y >= max_scroll - 1.0;
        let was_follow = self.active_view().auto_follow;
        let view = self.active_view_mut();
        if view.auto_follow == at_bottom {
            return;
        }
        view.auto_follow = at_bottom;
        self.last_stats_at = None;
        if was_follow && !at_bottom {
            self.materialize_live_terminal_tab();
        }
    }

    pub(crate) fn max_scroll_offset(&self) -> f32 {
        let metrics = self.renderer.metrics();
        let stride = metrics.row_stride;

        if self.has_active_terminal() {
            let terminal = self.active_terminal();
            let view = terminal.active_view();
            // Match-index tabs: never use full-file height.
            if terminal.is_file_session() && view.uses_match_index() {
                if view.match_scan_pos.is_some() {
                    // Mid-scan: empty viewport; keep scrollbar range at partial match count.
                    let total = view.match_offsets.len();
                    let content_h = total as f32 * stride;
                    return (content_h - self.viewport_height as f32).max(0.0);
                }
                let total = view.match_offsets.len();
                if total == 0 {
                    return 0.0;
                }
                // Entire result set fits in one window: scrollbar must use the same
                // WRAP-aware visual range as wheel (ScrollLines → local_window_max_scroll).
                // Using match_count * stride here made the thumb nearly inert while wheel worked.
                if total <= crate::file_match::MATCH_WINDOW_LINES {
                    return self.local_window_max_scroll();
                }
                // Huge match sets: ordinal scrollbar, never below the resident window's visual max.
                // Same f32 ~2^24 px precision boundary as the whole-file branch (#237).
                let ordinal = (total as f32 * stride - self.viewport_height as f32).max(0.0);
                let local = self.local_window_max_scroll();
                let global_floor = view.match_window_start as f32 * stride + local;
                return ordinal.max(global_floor);
            }
            if let Some(backed) = &terminal.file_backed {
                // Whole-file scrollbar range (1 file line ≈ 1 visual row for unread spans).
                // When the last window is resident, raise the range to the real visual
                // height so Wrap ON can still scroll to the true bottom.
                // f32 precision boundary (#237): `total as f32 * stride` is only
                // exact while the product stays under 2^24 (~16.7M px, e.g.
                // ~800k lines at 20 px/row). Beyond it consecutive f32 values
                // are more than 1 px apart (≈128 px near 100M lines), so
                // sub-row scroll precision degrades. Inherent to the f32
                // `Scroll { offset }` wire contract with Slint — accepted,
                // widening the wire type is a cross-crate API break.
                let total = backed.index.total_lines();
                let window = self.file_view_window_lines() as u64;
                let max_start = total.saturating_sub(window);
                let mut content_h = total as f32 * stride;
                let (start, local_rows_h) = if let Some(pending) = &terminal.pending_file_window {
                    if pending.new_start >= max_start {
                        let rows = view.cached_visual_rows(
                            self.viewport_width,
                            metrics.cell_width,
                            count_visual_rows,
                        );
                        // Pending EOF: estimate at least raw window height; after load
                        // the resident branch below will refine.
                        (
                            pending.new_start,
                            (window as f32 * stride).max(rows as f32 * stride),
                        )
                    } else {
                        (pending.new_start, 0.0)
                    }
                } else if terminal.buffer_line_start >= max_start {
                    let rows = view.cached_visual_rows(
                        self.viewport_width,
                        metrics.cell_width,
                        count_visual_rows,
                    );
                    (terminal.buffer_line_start, rows as f32 * stride)
                } else {
                    (0, 0.0)
                };
                if local_rows_h > 0.0 {
                    content_h = content_h.max(start as f32 * stride + local_rows_h);
                }
                return (content_h - self.viewport_height as f32).max(0.0);
            }
        }

        if self.paints_live_vt_grid() {
            // Paint is live-screen only; scrollbar range is retained ring + screen
            // so the thumb stays small (~viewport / (cap+rows)). Screen-only max
            // made the thumb ~half the track under WRAP while the line counter
            // correctly showed ever-seen hundreds of thousands.
            let wrap = self.active_view().wrap_lines;
            let committed = self.active_terminal().buffer.records_len();
            let grid_visual = if wrap {
                let lines = self.active_terminal().ingest.grid_flat_lines();
                count_visual_rows(&lines, true, self.viewport_width, metrics.cell_width)
            } else {
                self.active_terminal().ingest.size().1
            };
            let content_h = (committed + grid_visual) as f32 * stride;
            return (content_h - self.viewport_height as f32).max(0.0);
        }

        let view = self.active_view();
        let rows =
            view.cached_visual_rows(self.viewport_width, metrics.cell_width, count_visual_rows);
        let content_h = rows as f32 * stride;
        (content_h - self.viewport_height as f32).max(0.0)
    }

    /// Max local scroll within the currently loaded file/match window.
    pub(crate) fn local_window_max_scroll(&self) -> f32 {
        let metrics = self.renderer.metrics();
        let view = self.active_view();
        let rows =
            view.cached_visual_rows(self.viewport_width, metrics.cell_width, count_visual_rows);
        (rows as f32 * metrics.row_stride - self.viewport_height as f32).max(0.0)
    }

    /// Scrollbar / stats Y for the active session (global for FILES).
    pub(crate) fn stats_scroll_y(&self) -> f32 {
        if !self.has_active_terminal() {
            return 0.0;
        }
        let terminal = self.active_terminal();
        let stride = self.renderer.metrics().row_stride;
        let max_y = self.max_scroll_offset();
        // While a window jump is in flight, report the target so the thumb does not spring back.
        if terminal.is_file_session() {
            if let Some(pending) = &terminal.pending_file_window {
                let raw = pending.new_start as f32 * stride + pending.scroll_y;
                return raw.clamp(0.0, max_y);
            }
        }
        let local = terminal.scroll_offset_y;
        if !terminal.is_file_session() {
            return local.clamp(0.0, max_y);
        }
        let view = terminal.active_view();
        // Ordinal base + local mapping hits the f32 ~2^24 px precision
        // boundary on very large files (see max_scroll_offset, #237).
        let y = if view.uses_match_index() {
            // Small match sets: local visual Y only (match_window_start stays 0).
            // Large sets: ordinal base + local within the materialized window.
            if view.match_offsets.len() <= crate::file_match::MATCH_WINDOW_LINES {
                local
            } else {
                view.match_window_start as f32 * stride + local
            }
        } else if terminal.file_backed.is_some() {
            terminal.buffer_line_start as f32 * stride + local
        } else {
            local
        };
        y.clamp(0.0, max_y)
    }

    /// 1-based line at the **top** of the viewport and total lines for the status bar.
    ///
    /// An earlier revision used the bottom line so EOF (without a snap) would
    /// not read short by one viewport; the at-EOF snap below makes the top
    /// line safe, and the top line is what "at top of scrollback" should read
    /// as 1 (#212).
    pub(crate) fn viewport_line_position(&self) -> (u64, u64) {
        if !self.has_active_terminal() {
            return (0, 0);
        }
        let metrics = self.renderer.metrics();
        let stride = metrics.row_stride.max(0.001);
        let terminal = self.active_terminal();
        let view = terminal.active_view();

        if terminal.is_file_session() {
            if let Some(backed) = &terminal.file_backed {
                let view = terminal.active_view();
                // Filtered match-index tabs report match ordinals, not file line numbers.
                if view.uses_match_index() && view.match_scan_pos.is_none() {
                    let total = view.match_offsets.len() as u64;
                    if total == 0 {
                        return (0, 0);
                    }
                    let local_y = terminal.scroll_offset_y;
                    let index =
                        view.ensure_visual_row_index(self.viewport_width, metrics.cell_width);
                    let top_visual = (local_y / stride).floor().max(0.0) as usize;
                    let top_visual = top_visual.min(index.total_rows().saturating_sub(1));
                    let flat = index
                        .flat_at_visual_row(top_visual)
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    let cur = (view.match_window_start as u64 + flat as u64 + 1).min(total.max(1));
                    let at_eof = local_y + 1.0 >= self.local_window_max_scroll() || total <= 1;
                    let cur = if at_eof { total } else { cur };
                    return (cur, total);
                }
                let total = backed.index.total_lines();
                if total == 0 {
                    return (0, 0);
                }
                let (base, local_y, pin_end) = if let Some(pending) = &terminal.pending_file_window
                {
                    (
                        pending.new_start,
                        pending.scroll_y,
                        pending.scroll_y >= 1.0e20,
                    )
                } else {
                    (terminal.buffer_line_start, terminal.scroll_offset_y, false)
                };
                if pin_end {
                    return (total, total);
                }
                let index = view.ensure_visual_row_index(self.viewport_width, metrics.cell_width);
                let top_visual = (local_y / stride).floor().max(0.0) as usize;
                let top_visual = top_visual.min(index.total_rows().saturating_sub(1));
                let flat = index
                    .flat_at_visual_row(top_visual)
                    .map(|(i, _)| i)
                    .unwrap_or(0);
                let cur = (base + flat as u64 + 1).min(total.max(1));
                // At (or past) max scroll, snap to last file line so EOF reads `N / N`.
                // Near the f32 ~2^24 px boundary the 1 px tolerance is finer than
                // the quantization, but both sides share the same quantized max,
                // so the clamp still compares equal at EOF (#237).
                let at_eof = self.stats_scroll_y() + 1.0 >= self.max_scroll_offset();
                let cur = if at_eof { total } else { cur };
                return (cur, total);
            }
            return (0, 0);
        }

        if self.paints_live_vt_grid() {
            // Monotonic lines-ever-seen (dropped + retained + live rows), not the
            // capped ring size — otherwise Follow shows ~1000/1000 forever and
            // looks like paging through 1000-line chunks.
            let ever = terminal.buffer.dropped_count() as u64
                + terminal.buffer.records_len() as u64
                + terminal.ingest.size().1 as u64;
            if ever == 0 {
                return (0, 0);
            }
            return (ever, ever);
        }

        let total = view.flat_lines.len() as u64;
        if total == 0 {
            return (0, 0);
        }
        let local_y = terminal.scroll_offset_y;
        let index = view.ensure_visual_row_index(self.viewport_width, metrics.cell_width);
        let top_visual = (local_y / stride).floor().max(0.0) as usize;
        let top_visual = top_visual.min(index.total_rows().saturating_sub(1));
        let flat = index
            .flat_at_visual_row(top_visual)
            .map(|(i, _)| i)
            .unwrap_or(0);
        let cur = (flat as u64 + 1).min(total);
        let at_eof = local_y + 1.0 >= self.local_window_max_scroll() || total <= 1;
        let cur = if at_eof { total } else { cur };
        (cur, total)
    }

    pub(crate) fn current_max_scroll_x(&self) -> f32 {
        let view = self.active_view();
        if view.wrap_lines {
            return 0.0;
        }
        let metrics = self.renderer.metrics();
        max_scroll_x(&view.flat_lines, self.viewport_width, metrics.cell_width)
    }

    pub(crate) fn set_wrap_lines(&mut self, wrap: bool) {
        let view = self.active_view_mut();
        view.wrap_lines = wrap;
        view.invalidate_visual_rows_cache();
        self.active_terminal_mut().scroll_x = 0.0;
        self.mark_viewport_dirty();
        // Persist Wrap with the workspace snapshot (#109).
        self.sync_active_project_from_terminals();
        self.last_stats_at = None;
    }

    pub(crate) fn scroll_horizontal(&mut self, delta: f32) {
        if self.active_view().wrap_lines || delta == 0.0 {
            return;
        }
        let max_x = self.current_max_scroll_x();
        let terminal = self.active_terminal_mut();
        terminal.scroll_x = (terminal.scroll_x + delta).clamp(0.0, max_x);
        self.mark_viewport_dirty();
    }

    pub(crate) fn set_scroll_x(&mut self, offset: f32) {
        if self.active_view().wrap_lines {
            return;
        }
        let max_x = self.current_max_scroll_x();
        self.active_terminal_mut().scroll_x = offset.clamp(0.0, max_x);
        self.mark_viewport_dirty();
    }

    pub(crate) fn selection_at(&mut self, x: f32, y: f32, extend: bool, click_count: u32) {
        // Live-grid Follow paints the VT screen, not LogView flat_lines. Materialize
        // before mapping pixels so selection hits the visible text (not empty/stale flat_lines).
        if !extend && self.paints_live_vt_grid() {
            self.active_view_mut().auto_follow = false;
            self.materialize_live_terminal_tab();
            self.active_terminal_mut().scroll_offset_y = self.max_scroll_offset();
            self.last_stats_at = None;
        } else if !extend {
            self.active_view_mut().auto_follow = false;
        }

        let view = self.active_view();
        let metrics = self.renderer.metrics();
        let cell_width = metrics.cell_width;
        let visual = build_visual_lines(
            &view.flat_lines,
            view.wrap_lines,
            self.viewport_width,
            metrics.cell_width,
        );
        if visual.is_empty() {
            self.active_terminal_mut().selection = None;
            return;
        }
        let terminal = self.active_terminal();
        let pos = pos_at_pixel(
            x,
            y,
            terminal.scroll_offset_y,
            terminal.scroll_x,
            view.wrap_lines,
            metrics,
            &visual,
            &view.flat_lines,
        );
        // auto_follow cleared above when leaving live grid or starting a new selection.

        // Collapse toggle: disclosure gutter, or anywhere on a collapsed preview row.
        if !extend && click_count == 1 {
            if let Some(line) = self.active_view().flat_lines.get(pos.line_index) {
                let disclosure_hit = x < (LEFT_PAD as f32 + cell_width as f32 * 1.5);
                if line.collapsed || (line.collapsible && line.line_index == 0 && disclosure_hit) {
                    let id = line.record_id;
                    self.active_view_mut().toggle_record_collapse(id);
                    self.active_terminal_mut().selection = None;
                    self.mark_viewport_dirty();
                    self.last_stats_at = None;
                    return;
                }
            }
        }

        if !extend && click_count >= 3 {
            if let Some(sel) = record_selection_at(&self.active_view().flat_lines, pos) {
                self.active_terminal_mut().selection = Some(sel);
                return;
            }
        }
        if !extend && click_count >= 2 {
            if let Some(sel) = word_selection_at(&self.active_view().flat_lines, pos) {
                self.active_terminal_mut().selection = Some(sel);
                return;
            }
        }

        let terminal = self.active_terminal_mut();
        if extend {
            if let Some(sel) = terminal.selection.as_mut() {
                sel.caret = pos;
            } else {
                terminal.selection = Some(TextSelection::new(pos, pos));
            }
        } else {
            terminal.selection = Some(TextSelection::new(pos, pos));
        }
    }

    pub fn selection_text(&self) -> Option<String> {
        if !self.has_active_terminal() {
            return None;
        }
        let sel = self.active_terminal().selection.filter(|s| !s.is_empty())?;
        Some(selection_plain_text(&self.active_view().flat_lines, &sel))
    }

    /// Open the OSC 8 hyperlink under a viewport point (if any).
    ///
    /// Called on pointer-up without a drag; a miss is a silent no-op.
    pub(crate) fn open_link_at(&mut self, x: f32, y: f32) {
        if !self.has_active_terminal() {
            return;
        }
        let view = self.active_view();
        let metrics = self.renderer.metrics();
        let visual = build_visual_lines(
            &view.flat_lines,
            view.wrap_lines,
            self.viewport_width,
            metrics.cell_width,
        );
        if visual.is_empty() {
            return;
        }
        let terminal = self.active_terminal();
        let pos = pos_at_pixel(
            x,
            y,
            terminal.scroll_offset_y,
            terminal.scroll_x,
            view.wrap_lines,
            metrics,
            &visual,
            &view.flat_lines,
        );
        let Some(line) = view.flat_lines.get(pos.line_index) else {
            return;
        };
        let off = pos.byte_offset.min(line.raw.len());
        let mut cursor = 0usize;
        for seg in &line.segments {
            let seg_end = cursor + seg.text.len();
            if off >= cursor && off < seg_end {
                if let Some(uri) = seg.style.as_ref().and_then(|s| s.link.clone()) {
                    open_url(&uri);
                }
                return;
            }
            cursor = seg_end;
        }
    }
}

/// Scheme allow-list + shell-metacharacter rejection for [`open_url`].
fn openable_uri(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return false;
    }
    !uri.chars().any(|c| {
        matches!(
            c,
            '"' | '\'' | '%' | '&' | '|' | '<' | '>' | '^' | '!' | '`' | '$' | ';' | '\\'
        )
    })
}

/// Launch the OS opener for an OSC 8 URI (xdg-open / open / cmd start).
///
/// URIs come from log content, i.e. untrusted input: only http/https is
/// opened at all, and Windows `cmd /c start` additionally re-parses its
/// argument string, so shell metacharacters are rejected outright (#audit-2).
fn open_url(uri: &str) {
    if !openable_uri(uri) {
        return;
    }
    let result = if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", uri])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(uri).spawn()
    } else {
        std::process::Command::new("xdg-open")
            .arg(uri)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
    };
    if let Err(e) = result {
        let _ = e; // opener missing / spawn failed — non-fatal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::visible::flat_lines_from_raw_lines;

    #[test]
    fn openable_uri_allowlists_scheme_and_metacharacters() {
        assert!(openable_uri("https://example.com/a?b=1"));
        assert!(openable_uri("http://localhost:8080/x"));
        assert!(!openable_uri("file:///C:/Windows/System32/calc.exe"));
        assert!(!openable_uri("https://x.com/a&calc"));
        assert!(!openable_uri("https://x.com/%PATH%"));
        assert!(!openable_uri("not-a-url"));
    }

    #[test]
    fn scroll_to_row_uses_visual_rows_when_wrap_is_on() {
        let mut engine = Engine::new();
        let long_line = "x".repeat(500);
        let (width, cell) = (engine.viewport_width, engine.renderer.metrics().cell_width);
        let expected_visual = {
            let view = engine.active_view_mut();
            view.wrap_lines = true;
            view.flat_lines = Arc::new(flat_lines_from_raw_lines(
                &[long_line, "second".to_string()],
                0,
            ));
            // The long line wraps to several visual rows; flat line 1 starts
            // after all of them (issue #159).
            let visual = view
                .ensure_visual_row_index(width, cell)
                .visual_end_of_flat(0);
            assert!(visual > 1, "line should wrap to multiple rows");
            visual
        };

        // Zero-height viewport: any target row is below the fold, so the
        // engine must scroll to the row bottom.
        engine.viewport_height = 0;
        engine.scroll_to_row_index(1);

        let metrics = engine.renderer.metrics();
        let expected = expected_visual as f32 * metrics.row_stride + metrics.row_height;
        let scrolled = engine.active_terminal().scroll_offset_y;
        assert!(
            (scrolled - expected).abs() < 0.01,
            "scroll {scrolled} should land at visual row top {expected}"
        );
    }
}
