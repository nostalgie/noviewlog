//! Sliding file-window read, prefetch, and absolute scrollbar mapping.

use super::*;

impl Engine {
    pub(crate) fn maybe_prefetch_file_window(&mut self) {
        if !self.has_active_terminal() || self.active_terminal().file_load.is_some() {
            return;
        }
        // Match-index tabs own the viewport via apply_match_window — sliding the
        // shared file window causes thrashing/jumps while filters are active.
        if self.active_view().uses_match_index() {
            return;
        }
        if self.active_terminal().pending_file_window.is_some() {
            return;
        }

        let metrics = self.renderer.metrics();
        let row_stride = metrics.row_stride;
        let window_lines = self.file_view_window_lines() as u64;
        // Never slide farther than half a window — PREFETCH_RAW_LINES can exceed
        // FILE_VIEW_WINDOW_LINES and would drop the visible region (black frames).
        let step = (PREFETCH_RAW_LINES as u64).min(window_lines / 2).max(1);
        let (need_up, need_down, scroll_y, content_h, window_start, _window_end, total_lines) = {
            let terminal = self.active_terminal();
            let Some(backed) = &terminal.file_backed else {
                return;
            };
            let view = terminal.active_view();
            let rows =
                view.cached_visual_rows(self.viewport_width, metrics.cell_width, count_visual_rows);
            let content_h = rows as f32 * row_stride;
            let local_max = (content_h - self.viewport_height as f32).max(0.0);
            // Clamp: a stale global offset must not look like "near bottom".
            let scroll_y = terminal.scroll_offset_y.clamp(0.0, local_max);
            let near_top = scroll_y <= PREFETCH_SCROLL_PX;
            let near_bottom =
                scroll_y + self.viewport_height as f32 >= content_h - PREFETCH_SCROLL_PX;
            (
                near_top && terminal.buffer_line_start > 0,
                near_bottom && terminal.buffer_line_end < backed.index.total_lines(),
                scroll_y,
                content_h,
                terminal.buffer_line_start,
                terminal.buffer_line_end,
                backed.index.total_lines(),
            )
        };

        if need_up {
            let new_start = window_start.saturating_sub(step);
            let dropped = window_start - new_start;
            let scroll_adjust = if self.active_view().wrap_lines && dropped > 0 {
                let view = self.active_view();
                let n = dropped.min(view.flat_lines.len() as u64) as usize;
                count_visual_rows(
                    &view.flat_lines[..n],
                    true,
                    self.viewport_width,
                    metrics.cell_width,
                ) as f32
                    * row_stride
            } else {
                dropped as f32 * row_stride
            };
            let local_max_guess = content_h + scroll_adjust;
            let new_local = (scroll_y + scroll_adjust).clamp(0.0, local_max_guess);
            self.request_file_window_at(new_start, new_local, false);
        } else if need_down {
            let new_start = (window_start + step).min(total_lines.saturating_sub(window_lines));
            if new_start <= window_start {
                return;
            }
            let advanced = new_start - window_start;
            let scroll_adjust = if self.active_view().wrap_lines && advanced > 0 {
                let view = self.active_view();
                let n = advanced.min(view.flat_lines.len() as u64) as usize;
                count_visual_rows(
                    &view.flat_lines[..n],
                    true,
                    self.viewport_width,
                    metrics.cell_width,
                ) as f32
                    * row_stride
            } else {
                advanced as f32 * row_stride
            };
            let new_local = (scroll_y - scroll_adjust).max(0.0);
            self.request_file_window_at(new_start, new_local, false);
        }
    }

    /// Map a whole-file scrollbar offset to a loaded window + local scroll.
    ///
    /// Never applies a local `scroll_offset_y` past the current window (that painted black).
    pub(crate) fn scroll_file_to_global_offset(&mut self, global_offset: f32) {
        if !self.has_active_terminal() || self.active_terminal().file_backed.is_none() {
            return;
        }
        let stride = self.renderer.metrics().row_stride;
        let viewport_h = self.viewport_height as f32;
        let max_scroll = self.max_scroll_offset();
        let global_offset = global_offset.clamp(0.0, max_scroll);

        let (total, start, end) = {
            let terminal = self.active_terminal();
            let backed = terminal.file_backed.as_ref().unwrap();
            let start = terminal.buffer_line_start;
            let loaded = terminal.buffer.records_len() as u64;
            let end = if loaded > 0 {
                start
                    .saturating_add(loaded)
                    .min(terminal.buffer_line_end.max(start.saturating_add(loaded)))
            } else {
                terminal.buffer_line_end
            };
            let end = end.max(start);
            (backed.index.total_lines(), start, end)
        };
        if total == 0 {
            self.active_terminal_mut().scroll_offset_y = 0.0;
            self.mark_viewport_dirty();
            return;
        }

        let window = self.file_view_window_lines() as u64;
        let current_len = end.saturating_sub(start);
        let mapped = map_global_scroll_to_window(
            global_offset,
            max_scroll,
            viewport_h,
            stride,
            total,
            window,
            start,
            current_len,
        );

        if mapped.stay_local {
            let local_max = self.local_window_max_scroll();
            self.active_terminal_mut().scroll_offset_y = mapped.local_raw.clamp(0.0, local_max);
            self.maybe_prefetch_file_window();
            self.mark_viewport_dirty();
            return;
        }

        // Do not cap to raw window height — WRAP can make visual local scroll
        // much larger; finish_file_window clamps to local_window_max_scroll().
        let local = mapped.local_raw.max(0.0);

        if mapped.new_start == start && end > start {
            let local_max = self.local_window_max_scroll();
            let local = if mapped.near_eof {
                local_max
            } else {
                local.clamp(0.0, local_max)
            };
            self.active_terminal_mut().scroll_offset_y = local;
            self.maybe_prefetch_file_window();
            self.mark_viewport_dirty();
            return;
        }

        // Keep showing the current window until the new chunk lands (no black flash).
        // Near EOF: pin_to_end; finish clamps to real local_max (Wrap ON can make
        // visual height > raw window * stride).
        self.request_file_window_at(mapped.new_start, local, mapped.near_eof);
        self.mark_viewport_dirty();
    }

    pub(crate) fn file_view_window_lines(&self) -> usize {
        self.config
            .max_scrollback_lines
            .min(WINDOW_RAW_LINES)
            .min(FILE_VIEW_WINDOW_LINES)
    }

    pub(crate) fn request_file_window_at(
        &mut self,
        new_start: u64,
        scroll_y: f32,
        pin_to_end: bool,
    ) {
        let window = self.file_view_window_lines() as u64;
        let (end_line, same_pending) = {
            let terminal = self.active_terminal();
            let Some(backed) = &terminal.file_backed else {
                return;
            };
            let same = terminal
                .pending_file_window
                .as_ref()
                .is_some_and(|p| p.new_start == new_start);
            let end_line = (new_start + window).min(backed.index.total_lines());
            (end_line, same)
        };
        if same_pending {
            if let Some(pending) = self.active_terminal_mut().pending_file_window.as_mut() {
                pending.scroll_y = scroll_y;
                pending.pin_to_end = pin_to_end;
            }
            self.mark_viewport_dirty();
            return;
        }
        if end_line <= new_start {
            return;
        }
        self.active_terminal_mut().pending_file_window = Some(PendingFileWindow {
            new_start,
            scroll_y,
            pin_to_end,
            next_line: new_start,
            end_line,
            lines: Vec::new(),
        });
        self.mark_viewport_dirty();

        // Read the whole window on a worker thread (issue #55); the result is
        // applied by [`Engine::apply_file_io_results`] when it lands.
        let (shared, index, term_id) = {
            let terminal = self.active_terminal();
            let backed = terminal.file_backed.as_ref().expect("checked above");
            (
                backed.file.clone(),
                backed.index.clone(),
                terminal.id.clone(),
            )
        };
        let count = (end_line - new_start) as usize;
        let inbox = self.file_io_done.clone();
        // Copy for the spawn-failure branch: the closure owns the original.
        let spawn_failed_term = term_id.clone();
        let worker = std::thread::Builder::new()
            .name("noviewlog-file-window".into())
            .spawn(move || {
                let result =
                    crate::file_index::read_lines_shared(&shared, &index, new_start, count);
                let mut inbox = inbox.lock().unwrap_or_else(|e| e.into_inner());
                inbox.push(FileIoDone::Window {
                    term_id,
                    new_start,
                    scroll_y,
                    result,
                });
            });
        if let Err(err) = worker {
            self.file_window_spawn_failed(&spawn_failed_term, new_start, scroll_y, err);
        }
    }

    /// A failed file-window worker spawn (issue #148): the read never
    /// happens, so degrade through the same path as a failed read — drop the
    /// pending window and surface a status error.
    pub(super) fn file_window_spawn_failed(
        &mut self,
        term_id: &str,
        new_start: u64,
        scroll_y: f32,
        err: std::io::Error,
    ) {
        let message = format!("File window read failed to start: {err}");
        self.apply_window_result(term_id, new_start, scroll_y, Err(message));
    }

    pub(super) fn pending_window_ready(&self) -> bool {
        self.active_terminal()
            .pending_file_window
            .as_ref()
            .is_some_and(|p| p.next_line >= p.end_line)
    }

    pub(super) fn finish_active_pending_window(&mut self) {
        let pending = self
            .active_terminal_mut()
            .pending_file_window
            .take()
            .expect("checked by pending_window_ready");
        self.finish_file_window(self.active_terminal, pending);
    }

    pub(super) fn apply_window_result(
        &mut self,
        term_id: &str,
        new_start: u64,
        _scroll_y: f32,
        result: Result<Vec<String>, String>,
    ) {
        let Some(idx) = self.terminals.iter().position(|t| t.id == term_id) else {
            return;
        };
        // Superseded (a newer window was requested, or the pending was
        // cleared by reload/stop): drop the stale read.
        let stale = !self.terminals[idx]
            .pending_file_window
            .as_ref()
            .is_some_and(|p| p.new_start == new_start);
        match result {
            Ok(lines) if !stale => {
                let terminal = &mut self.terminals[idx];
                if let Some(pending) = terminal.pending_file_window.as_mut() {
                    pending.lines = lines;
                    pending.next_line = pending.end_line;
                    // Keep `pending.scroll_y`: a later request may have
                    // updated the desired scroll while this read was in flight.
                }
                // Inactive terminals finish when they next become active.
                if idx == self.active_terminal {
                    self.finish_active_pending_window();
                }
            }
            Err(message) if !stale => {
                self.terminals[idx].pending_file_window = None;
                if idx == self.active_terminal {
                    self.status_message = message.clone();
                    self.push_event(json!({"type":"status","message": message}));
                }
            }
            _ => {}
        }
    }

    pub(crate) fn finish_file_window(&mut self, term_idx: usize, pending: PendingFileWindow) {
        let format = self.current_format();
        let raw_count = pending.lines.len() as u64;
        let desired_scroll = pending.scroll_y;
        let pin_to_end = pending.pin_to_end;
        {
            let terminal = &mut self.terminals[term_idx];
            terminal.parser = RecordParser::new(format);
            let mut records = Vec::new();
            for line in pending.lines {
                records.extend(terminal.parser.push_line(line));
            }
            if let Some(last) = terminal.parser.flush_pending() {
                records.push(last);
            }
            terminal.buffer.replace_all(records);
            terminal.buffer_line_start = pending.new_start;
            terminal.buffer_line_end = pending.new_start + raw_count;
            terminal.scroll_offset_y = desired_scroll;
            terminal.selection = None;
            terminal.pending_file_window = None;
        }
        self.mark_all_views_dirty();
        let _ = self.rebuild_if_needed();
        // Window finishes only run for the active terminal (inactive results
        // are stashed until activation), so the active-based clamp is safe.
        let local_max = self.local_window_max_scroll();
        {
            let terminal = &mut self.terminals[term_idx];
            terminal.scroll_offset_y = if pin_to_end {
                local_max
            } else {
                terminal.scroll_offset_y.clamp(0.0, local_max)
            };
        }
        self.mark_viewport_dirty();
    }
}
