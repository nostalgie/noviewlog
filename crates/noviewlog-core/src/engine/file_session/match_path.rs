//! Whole-file match scan and match-window materialization for file filter tabs.

use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// Match-window wheel recenter lead (`window / N`).
pub(super) const MATCH_WINDOW_RECENTER_LEAD_DIV: usize = 4;

/// Post a match-scan progress/result event into the engine inbox.
fn post_scan_event(
    inbox: &Arc<std::sync::Mutex<Vec<FileIoDone>>>,
    term_id: &str,
    view_idx: usize,
    token: u64,
    progress: MatchScanProgress,
) {
    let mut inbox = inbox.lock().unwrap_or_else(|e| e.into_inner());
    inbox.push(FileIoDone::MatchScan {
        term_id: term_id.to_string(),
        view_idx,
        token,
        progress,
    });
}

impl Engine {
    /// Monotonic request id for background match-window reads; results with a
    /// stale id are dropped.
    pub(super) fn next_match_window_req(&mut self) -> u64 {
        self.match_window_req = self.match_window_req.wrapping_add(1);
        self.match_window_req
    }

    /// Apply a finished match-window read (issue #55).
    pub(super) fn apply_match_window_result(
        &mut self,
        term_id: &str,
        view_idx: usize,
        req: u64,
        start: usize,
        new_local: f32,
        result: Result<Vec<String>, String>,
    ) {
        let Some(idx) = self.terminals.iter().position(|t| t.id == term_id) else {
            return;
        };
        // The view may have been closed while the read was in flight; a newer
        // request or an index invalidation owns the view now.
        let Some(view) = self.terminals[idx]
            .views
            .get_mut(view_idx)
            .filter(|v| v.match_window_inflight == Some(req))
        else {
            return;
        };
        view.match_window_inflight = None;
        let lines = match result {
            Ok(lines) => lines,
            Err(message) => {
                if idx == self.active_terminal && self.terminals[idx].active_view == view_idx {
                    self.status_message = message;
                }
                return;
            }
        };
        {
            let flat = crate::core::visible::flat_lines_from_raw_lines(&lines, start as u64);
            view.set_match_flat_lines(flat);
            view.match_window_start = start;
        }
        // scroll_offset_y is shared between a terminal's views — only the
        // active view may move it.
        if idx == self.active_terminal && self.terminals[idx].active_view == view_idx {
            let local_max = self.local_window_max_scroll();
            let terminal = &mut self.terminals[idx];
            terminal.scroll_offset_y = new_local.clamp(0.0, local_max);
            // Match window lines are a different index space than the prior view.
            terminal.selection = None;
            self.mark_viewport_dirty();
        }
    }

    pub(super) fn apply_match_scan_result(
        &mut self,
        term_id: &str,
        view_idx: usize,
        token: u64,
        progress: MatchScanProgress,
    ) {
        let Some(idx) = self.terminals.iter().position(|t| t.id == term_id) else {
            return;
        };
        {
            let view = self.terminals[idx].views.get_mut(view_idx);
            let Some(view) = view else { return };
            if view.match_scan_token != token {
                return; // filters changed mid-scan; a fresh scan owns the view
            }
        }
        let is_active = idx == self.active_terminal && self.terminals[idx].active_view == view_idx;
        let file_size = self.terminals[idx]
            .file_backed
            .as_ref()
            .map(|b| b.index.file_size())
            .unwrap_or(0);
        match progress {
            MatchScanProgress::Progressing { next, match_count } => {
                let pct = if file_size == 0 {
                    100
                } else {
                    ((next as f32 / file_size as f32) * 100.0) as u32
                };
                // Throttle status churn: update every ~5% (same idea as file indexing).
                // Percent lives on the view — never parse the status string back.
                let prev_pct = self.terminals[idx].views[view_idx].last_match_scan_pct;
                let report = is_active && (pct >= 99 || pct / 5 > prev_pct / 5);
                {
                    let view = &mut self.terminals[idx].views[view_idx];
                    view.match_scan_pos = Some(next);
                    if report {
                        view.last_match_scan_pct = pct;
                    }
                }
                if report {
                    self.status_message =
                        format!("Scanning filters… {pct}% ({match_count} matches)");
                    self.push_event(json!({"type":"status","message": self.status_message}));
                    // Repaint centered progress (empty viewport during scan).
                    self.mark_viewport_dirty();
                }
            }
            MatchScanProgress::Done { offsets, capped } => {
                let match_count = offsets.len();
                let view = &mut self.terminals[idx].views[view_idx];
                view.match_offsets = Arc::new(offsets);
                view.match_scan_pos = None;
                view.match_scan_inflight = false;
                // Persistent truncation hint (issue #150): stats carries this
                // to the UI until the next invalidate/restart.
                view.match_capped = capped;
                view.request_match_rebuild();
                if !is_active {
                    return;
                }
                self.status_message = if capped {
                    format!("Filter scan capped (first {match_count} matches)")
                } else {
                    format!("Filter scan complete ({match_count} matches)")
                };
                self.push_event(json!({"type":"status","message": self.status_message}));
                self.apply_match_window();
                self.mark_viewport_dirty();
                self.last_stats_at = None;
            }
            MatchScanProgress::Failed(message) => {
                let view = &mut self.terminals[idx].views[view_idx];
                view.match_offsets = Arc::new(Vec::new());
                view.match_scan_pos = None;
                view.match_scan_inflight = false;
                if !is_active {
                    return;
                }
                self.status_message = message.clone();
                self.push_event(json!({"type":"status","message": message}));
                self.mark_viewport_dirty();
                self.last_stats_at = None;
            }
        }
    }

    /// Map a whole-match-index scrollbar offset to a materialized match window + local scroll.
    ///
    /// During an in-progress scan the viewport stays empty at scroll 0 (no file-window jumps).
    pub(crate) fn scroll_match_to_global_offset(&mut self, global_offset: f32) {
        if !self.has_active_terminal() || !self.active_view().uses_match_index() {
            return;
        }
        if self.active_view().match_scan_pos.is_some() {
            self.active_terminal_mut().scroll_offset_y = 0.0;
            self.active_view_mut().match_window_start = 0;
            return;
        }

        let total = self.active_view().match_offsets.len();
        if total == 0 {
            self.active_terminal_mut().scroll_offset_y = 0.0;
            self.active_view_mut().match_window_start = 0;
            self.active_view_mut().set_match_flat_lines(Vec::new());
            self.mark_viewport_dirty();
            return;
        }

        // All matches fit in one window: scrollbar Y is local visual scroll (WRAP-aware),
        // same coordinate space as wheel / max_scroll_offset / stats_scroll_y.
        if total <= crate::file_match::MATCH_WINDOW_LINES {
            if self.active_view().match_window_start != 0
                || self.active_view().flat_lines.len() != total
            {
                self.active_view_mut().match_window_start = 0;
                self.active_terminal_mut().scroll_offset_y = 0.0;
                self.apply_match_window();
            }
            let max_scroll = self.local_window_max_scroll();
            self.active_terminal_mut().scroll_offset_y = global_offset.clamp(0.0, max_scroll);
            self.mark_viewport_dirty();
            self.last_stats_at = None;
            return;
        }

        let stride = self.renderer.metrics().row_stride;
        let viewport_h = self.viewport_height as f32;
        let max_scroll = self.max_scroll_offset();
        let global_offset = global_offset.clamp(0.0, max_scroll);

        let window = crate::file_match::MATCH_WINDOW_LINES as u64;
        let start = self.active_view().match_window_start as u64;
        let loaded = self.active_view().flat_lines.len() as u64;
        let mapped = map_global_scroll_to_window(
            global_offset,
            max_scroll,
            viewport_h,
            stride,
            total as u64,
            window,
            start,
            loaded,
        );

        if mapped.stay_local {
            let local_max = self.local_window_max_scroll();
            self.active_terminal_mut().scroll_offset_y = mapped.local_raw.clamp(0.0, local_max);
            self.mark_viewport_dirty();
            return;
        }

        let new_start = mapped.new_start as usize;
        self.active_view_mut().match_window_start = new_start;
        self.active_terminal_mut().scroll_offset_y = mapped.local_raw;
        self.apply_match_window();
        let local_max = self.local_window_max_scroll();
        let terminal = self.active_terminal_mut();
        terminal.scroll_offset_y = if mapped.near_eof {
            local_max
        } else {
            terminal.scroll_offset_y.clamp(0.0, local_max)
        };
        self.mark_viewport_dirty();
    }

    /// Advance whole-file match scan for the active file filter tab.
    pub(crate) fn advance_file_match_scan(&mut self) {
        if !self.has_active_terminal() || !self.active_terminal().is_file_session() {
            return;
        }
        if self.active_terminal().file_backed.is_none() {
            return;
        }

        let needs = {
            let view = self.active_view();
            view.uses_match_index()
        };
        if !needs {
            let view = self.active_view_mut();
            if view.match_scan_pos.is_some() || !view.match_offsets.is_empty() {
                view.clear_match_index();
                view.mark_flat_lines_dirty();
            }
            return;
        }

        // Start scan if filters require an index but none is running/complete.
        {
            let view = self.active_view_mut();
            if view.match_scan_pos.is_none()
                && view.match_offsets.is_empty()
                && view.is_flat_lines_dirty()
            {
                view.invalidate_match_index();
            }
        }

        let Some(from) = self.active_view().match_scan_pos else {
            return;
        };
        if self.active_view().match_scan_inflight {
            // A worker owns this scan; progress lands via apply_file_io_results.
            return;
        }

        let file_size = self
            .active_terminal()
            .file_backed
            .as_ref()
            .map(|b| b.index.file_size())
            .unwrap_or(0);

        let (filters, severity) = {
            let view = self.active_view();
            (view.filters().to_vec(), view.severity_filter)
        };
        let (shared, term_id, view_idx, token) = {
            let terminal = self.active_terminal();
            let backed = terminal.file_backed.as_ref().unwrap();
            (
                backed.file.clone(),
                terminal.id.clone(),
                terminal.active_view,
                terminal.active_view().match_scan_token,
            )
        };
        self.active_view_mut().match_scan_inflight = true;

        // Shared cancellation flag: bumped tokens / cleared filters flip it so
        // the worker stops scanning instead of running to the cap or EOF for
        // a result nobody will read (#206).
        let cancel = Arc::new(AtomicBool::new(false));
        self.active_view_mut().match_scan_cancel = Some(cancel.clone());

        // Scan the whole file on a worker thread (issue #55), reporting
        // progress per chunk; the UI thread never touches the file.
        let cap = self.match_scan_cap();
        let inbox = self.file_io_done.clone();
        // Copy for the spawn-failure branch: the closure owns the original.
        let spawn_failed_term = term_id.clone();
        let worker = std::thread::Builder::new()
            .name("noviewlog-match-scan".into())
            .spawn(move || {
                let filter_engine = crate::core::filter::FilterEngine::new(filters);
                let mut offsets: Vec<u64> = Vec::new();
                let mut pos = from;
                let outcome = loop {
                    if cancel.load(Ordering::Relaxed) {
                        // Superseded mid-scan: exit quietly, no result event.
                        return;
                    }
                    let mut file = shared.lock().unwrap_or_else(|e| e.into_inner());
                    match crate::file_match::scan_match_chunk_with_cap(
                        &mut file,
                        file_size,
                        pos,
                        crate::file_match::MATCH_SCAN_BYTES_PER_TICK,
                        &filter_engine,
                        severity,
                        &mut offsets,
                        cap,
                    ) {
                        Ok((next, done, capped)) => {
                            drop(file);
                            pos = next;
                            let count = offsets.len();
                            if done {
                                break Ok((offsets, capped));
                            }
                            let progress = MatchScanProgress::Progressing {
                                next,
                                match_count: count,
                            };
                            post_scan_event(&inbox, &term_id, view_idx, token, progress);
                        }
                        Err(message) => break Err(message),
                    }
                };
                let progress = match outcome {
                    // `capped` comes straight from the scanner (issue #150):
                    // the offset cap stopped the scan before file end.
                    Ok((offsets, capped)) => MatchScanProgress::Done { offsets, capped },
                    Err(message) => MatchScanProgress::Failed(message),
                };
                post_scan_event(&inbox, &term_id, view_idx, token, progress);
            });
        if let Err(err) = worker {
            self.match_scan_spawn_failed(&spawn_failed_term, view_idx, token, err);
        }
    }

    /// A failed match-scan worker spawn (issue #148): no worker owns the
    /// scan, so reset the view exactly like a failed scan (it must not stay
    /// in-flight forever) and surface a status error.
    pub(super) fn match_scan_spawn_failed(
        &mut self,
        term_id: &str,
        view_idx: usize,
        token: u64,
        err: std::io::Error,
    ) {
        let message = format!("Filter scan failed to start: {err}");
        self.apply_match_scan_result(term_id, view_idx, token, MatchScanProgress::Failed(message));
    }

    /// Match-offset cap for the running/next scan. Tests may shrink it to
    /// exercise truncation without generating 2M+ matching lines.
    pub(super) fn match_scan_cap(&self) -> usize {
        #[cfg(test)]
        {
            if let Some(cap) = self.match_scan_cap_override {
                return cap;
            }
        }
        crate::file_match::MAX_MATCH_OFFSETS
    }

    /// Materialize a window of match lines into the active view's flat_lines.
    ///
    /// `scroll_offset_y` is local within the current match window; global Y is
    /// `match_window_start * stride + scroll_offset_y` only when paging a large
    /// match set. When all matches fit in one window, scroll is purely local/visual.
    pub(crate) fn apply_match_window(&mut self) {
        if !self.has_active_terminal() || self.active_terminal().file_backed.is_none() {
            return;
        }
        if !self.active_view().uses_match_index() {
            return;
        }
        if self.active_view().match_scan_pos.is_some() {
            return;
        }

        let metrics = self.renderer.metrics();
        let stride = metrics.row_stride;
        let local = self.active_terminal().scroll_offset_y;
        let prev_start = self.active_view().match_window_start;
        let loaded = self.active_view().flat_lines.len();
        let total = self.active_view().match_offsets.len();
        let window = crate::file_match::MATCH_WINDOW_LINES;

        // Small match sets: keep the full list resident and only adjust local scroll.
        // Re-reading on every wheel tick made scroll feel frozen (hundreds of seeks).
        if total <= window && loaded == total && prev_start == 0 {
            let local_max = self.local_window_max_scroll();
            self.active_terminal_mut().scroll_offset_y = local.clamp(0.0, local_max);
            return;
        }

        let global_y = prev_start as f32 * stride + local;
        let max_start = total.saturating_sub(window);
        let target = if total == 0 {
            0
        } else {
            ((global_y / stride).floor() as usize).min(total.saturating_sub(1))
        };
        let mut start = prev_start.min(max_start);
        let end = start.saturating_add(loaded.min(window));
        // Margin must scale with the *loaded* window, not MATCH_WINDOW_LINES — otherwise
        // small match sets always look like "near the end" and re-read every tick.
        let margin = (loaded / SCROLL_EDGE_MARGIN_DIV).max(1);
        let need_recenter = loaded == 0
            || target < start.saturating_add(margin)
            || (end > start && target + margin >= end);
        if !need_recenter {
            let local_max = self.local_window_max_scroll();
            self.active_terminal_mut().scroll_offset_y = local.clamp(0.0, local_max);
            return;
        }
        start = target
            .saturating_sub(window / MATCH_WINDOW_RECENTER_LEAD_DIV)
            .min(max_start);
        let start = start.min(total);

        // The recenter read runs on a worker thread (issue #55): one in
        // flight per view, the latest request wins; the old window stays
        // visible until the new one lands.
        if self.active_view().match_window_inflight.is_some() {
            return;
        }
        let req = self.next_match_window_req();
        {
            let view = self.active_view_mut();
            view.match_window_inflight = Some(req);
        }
        let (shared, offsets, term_id, view_idx) = {
            let terminal = self.active_terminal();
            let backed = terminal.file_backed.as_ref().expect("checked above");
            (
                backed.file.clone(),
                terminal.active_view().match_offsets.clone(),
                terminal.id.clone(),
                terminal.active_view,
            )
        };
        let new_local = (global_y - start as f32 * stride).max(0.0);
        let inbox = self.file_io_done.clone();
        // Copy for the spawn-failure branch: the closure owns the original.
        let spawn_failed_term = term_id.clone();
        let worker = std::thread::Builder::new()
            .name("noviewlog-match-window".into())
            .spawn(move || {
                let result = {
                    let mut file = shared.lock().unwrap_or_else(|e| e.into_inner());
                    crate::file_match::read_match_window(&mut file, &offsets, start, window)
                };
                let mut inbox = inbox.lock().unwrap_or_else(|e| e.into_inner());
                inbox.push(FileIoDone::MatchWindow {
                    term_id,
                    view_idx,
                    req,
                    start,
                    new_local,
                    result,
                });
            });
        if let Err(err) = worker {
            self.match_window_spawn_failed(
                &spawn_failed_term,
                view_idx,
                req,
                start,
                new_local,
                err,
            );
        }
    }

    /// A failed match-window worker spawn (issue #148): the request would
    /// stay in-flight forever, so clear it through the same path as a failed
    /// read and surface a status error.
    pub(super) fn match_window_spawn_failed(
        &mut self,
        term_id: &str,
        view_idx: usize,
        req: u64,
        start: usize,
        new_local: f32,
        err: std::io::Error,
    ) {
        let message = format!("Filter window read failed to start: {err}");
        self.apply_match_window_result(term_id, view_idx, req, start, new_local, Err(message));
    }

    /// After filter/severity invalidate on a file session: empty viewport + scroll 0.
    pub(crate) fn reset_file_match_viewport(&mut self) {
        if !self.has_active_terminal() || !self.active_terminal().is_file_session() {
            return;
        }
        if !self.active_view().uses_match_index() {
            return;
        }
        // Empty until scan finishes (rebuild_if_needed skips while match_scan_pos is set).
        self.active_view_mut().clear_flat_lines();
        {
            let terminal = self.active_terminal_mut();
            terminal.scroll_offset_y = 0.0;
            // Stale selection indices point into the previous (unfiltered) window.
            terminal.selection = None;
            // Drop any in-flight sliding-window read (open-EOF pin / prefetch).
            // Otherwise `apply_file_io_results` during the match scan can land
            // that window and thrash `buffer_line_start` under the filter tab.
            terminal.pending_file_window = None;
        }
        self.status_message = "Scanning filters… 0% (0 matches)".to_string();
        self.push_event(json!({"type":"status","message": self.status_message}));
        self.mark_viewport_dirty();
        self.last_stats_at = None;
    }
}
