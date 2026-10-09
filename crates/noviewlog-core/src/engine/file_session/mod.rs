//! FILES session lifecycle, sliding windows, and match-index path.
//!
//! Split for readability (god-module backlog):
//! - [`window`] — sliding window prefetch / absolute scroll / finish
//! - [`match_path`] — whole-file match scan and match-window materialize
//! - this module — open/reload/watch lifecycle + I/O inbox dispatch

mod match_path;
mod window;

use super::*;

/// How often the tick sweeps open file sessions for external changes
/// (issue #151). One cheap stat per loaded session per sweep.
pub(crate) const FILE_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

/// Absolute scrollbar → window mapping fractions (issue #339).
/// Treat max_scroll this small as "already at EOF" (degenerate short files).
const SCROLL_NEAR_EOF_MAX: f32 = 0.5;
/// Keep the target this far from loaded-window edges before accepting a
/// pure-local scroll (`loaded / N`).
pub(super) const SCROLL_EDGE_MARGIN_DIV: usize = 5;
/// Absolute scrollbar recenter: place the target this far into the new window
/// (`window / N`). Wheel recenter in [`Engine::apply_match_window`] uses
/// [`match_path::MATCH_WINDOW_RECENTER_LEAD_DIV`] (tighter — intentional).
const SCROLL_RECENTER_LEAD_DIV: usize = 3;

/// Result of mapping a global scrollbar offset onto a sliding window.
pub(super) struct MappedGlobalScroll {
    new_start: u64,
    local_raw: f32,
    near_eof: bool,
    /// Target already sits comfortably inside the current window.
    stay_local: bool,
}

/// Shared mapper for file-line and match-ordinal absolute scrollbar jumps.
pub(super) fn map_global_scroll_to_window(
    global_offset: f32,
    max_scroll: f32,
    viewport_h: f32,
    stride: f32,
    total: u64,
    window: u64,
    current_start: u64,
    current_len: u64,
) -> MappedGlobalScroll {
    let max_start = total.saturating_sub(window);
    let target = ((global_offset / stride).floor() as u64).min(total.saturating_sub(1));
    // Only pin the last window when the thumb is actually at the track bottom.
    // `target >= max_start` alone misclassified mid-file drags (e.g. lines
    // 16k–20k on a 26k file with a 10k window) as EOF and snapped the thumb to
    // the end (#339).
    let at_track_bottom = viewport_h > 0.0 && global_offset + viewport_h >= max_scroll - 1.0;
    let near_eof = max_scroll <= SCROLL_NEAR_EOF_MAX || at_track_bottom;

    if near_eof {
        let local = (global_offset - max_start as f32 * stride).max(0.0);
        return MappedGlobalScroll {
            new_start: max_start,
            local_raw: local,
            near_eof: true,
            stay_local: false,
        };
    }

    let end = current_start.saturating_add(current_len);
    let margin = (current_len / SCROLL_EDGE_MARGIN_DIV as u64).max(1);
    let comfortably_inside =
        current_len > 0 && target >= current_start.saturating_add(margin) && target + margin < end;
    if comfortably_inside {
        let local = global_offset - current_start as f32 * stride;
        return MappedGlobalScroll {
            new_start: current_start,
            local_raw: local,
            near_eof: false,
            stay_local: true,
        };
    }

    let new_start = target
        .saturating_sub(window / SCROLL_RECENTER_LEAD_DIV as u64)
        .min(max_start);
    let local = (global_offset - new_start as f32 * stride).max(0.0);
    MappedGlobalScroll {
        new_start,
        local_raw: local,
        near_eof: false,
        stay_local: false,
    }
}

/// Progress / result of a background whole-file match scan (issue #55).
pub(crate) enum MatchScanProgress {
    Progressing { next: u64, match_count: usize },
    Done { offsets: Vec<u64>, capped: bool },
    Failed(String),
}

/// A completed background file I/O job, posted by a worker thread into the
/// engine inbox and applied on the UI tick (issue #55: no synchronous file
/// reads on the event-loop thread during load or scroll).
pub(crate) enum FileIoDone {
    /// Sliding-window read for a file session (scroll / prefetch).
    Window {
        term_id: String,
        new_start: u64,
        scroll_y: f32,
        result: Result<Vec<String>, String>,
    },
    /// Whole-file match scan for a view (file filter tab).
    MatchScan {
        term_id: String,
        view_idx: usize,
        token: u64,
        progress: MatchScanProgress,
    },
    /// Match-window read for a view's viewport (filter tab recenter).
    MatchWindow {
        term_id: String,
        view_idx: usize,
        req: u64,
        start: usize,
        new_local: f32,
        result: Result<Vec<String>, String>,
    },
}

impl Engine {
    pub(crate) fn open_log_file_command(&mut self, path: &str) {
        self.ensure_valid_state();
        let path = path.to_string();

        // Re-open: switch to an existing terminal for the same file and reload.
        if let Some(idx) = self.terminals.iter().position(|t| {
            t.launch.log_file.as_deref() == Some(path.as_str())
                || t.file_session_path() == Some(path.as_str())
        }) {
            self.active_terminal = idx;
            self.configure_active_as_file_session(&path);
            self.start_log_file_load(&path);
            self.sync_active_project_from_terminals();
            self.mark_viewport_dirty();
            self.last_stats_at = None;
            return;
        }

        // Never convert a live PTY session into a file row — always open a
        // dedicated file session (appears under FILES in the sidebar).
        self.terminal_add_blank();
        self.configure_active_as_file_session(&path);
        self.start_log_file_load(&path);
        self.sync_active_project_from_terminals();
    }

    /// Reload a file session from disk. Missing path → status error, session kept.
    pub(crate) fn reload_file_command(&mut self, terminal_id: Option<&str>) {
        self.ensure_valid_state();
        if let Some(id) = terminal_id {
            self.terminal_switch(id);
        }
        if !self.has_active_terminal() || !self.active_terminal().is_file_session() {
            self.status_message = "Reload is for file sessions".to_string();
            self.push_event(json!({"type":"status","message": self.status_message}));
            return;
        }
        let Some(path) = self
            .active_terminal()
            .file_session_path()
            .map(str::to_string)
        else {
            self.status_message = "File session has no path".to_string();
            self.push_event(json!({"type":"status","message": self.status_message}));
            return;
        };
        self.start_log_file_load(&path);
    }

    pub(crate) fn maybe_lazy_load_active_file(&mut self) {
        if !self.has_active_terminal() || !self.active_terminal().is_file_session() {
            return;
        }
        let terminal = self.active_terminal();
        if terminal.file_backed.is_some() || terminal.file_load.is_some() {
            return;
        }
        let Some(path) = terminal.launch.log_file.clone() else {
            return;
        };
        self.start_log_file_load(&path);
    }

    pub(crate) fn configure_active_as_file_session(&mut self, path: &str) {
        let parent = std::path::Path::new(path)
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .filter(|s| !s.is_empty());
        let id = self.active_terminal().id.clone();
        {
            let terminal = self.active_terminal_mut();
            terminal.launch.log_file = Some(path.to_string());
            terminal.launch.command = None;
            terminal.launch.args.clear();
            terminal.launch.wsl = false;
            terminal.launch.wsl_distro = None;
            terminal.process_started = true;
            terminal.running = false;
            if let Some(cwd) = parent {
                terminal.cwd = cwd;
            }
            terminal.sync_primary_tab_identity();
            terminal.disable_follow_all_views();
        }
        // Stop any PTY that might still be attached (should not happen on a
        // blank terminal, but keeps reopen/reload safe).
        if let Some(mut pty) = self.ptys.remove(&id) {
            pty.stop();
        }
    }

    pub(crate) fn start_log_file_load(&mut self, path: &str) {
        // Open, sniff, transcode, read, and index all run on a worker thread
        // (issue #55); the engine drains events in `advance_file_load`.
        let load = crate::file_load::spawn_file_load(path);
        let display_path = load.path.clone();
        let term = self.viewport_pty_size();

        {
            let terminal = self.active_terminal_mut();
            // Dropping the previous handle stops its worker.
            terminal.file_load = None;
            terminal.file_load_stalled_at = None;
            terminal.file_backed = None;
            terminal.file_changed = false;
            terminal.pending_file_window = None;
            terminal.pending_open_scroll_eof = false;
            terminal.buffer_line_start = 0;
            terminal.buffer_line_end = 0;
            terminal.buffer.clear();
            terminal
                .ingest
                .reset_with_size(term.cols as usize, term.rows as usize);
            for view in &mut terminal.views {
                view.clear_flat_lines();
            }
            terminal.file_load = Some(load);
            terminal.running = false;
        }

        self.status_message = format!("Loading: {display_path}…");
        self.push_event(json!({"type":"status","message": self.status_message}));
    }

    /// Apply load events drained from the background worker (issue #55).
    /// Loads for *inactive* terminals (e.g. project-restore FILE sessions,
    /// issue #234) progress too: each pending load briefly borrows the active
    /// slot that `push_lines` and friends write through, then the original
    /// active terminal is restored.
    pub(crate) fn advance_file_load(&mut self) {
        if !self.has_active_terminal() {
            return;
        }
        let original = self.active_terminal;
        for idx in 0..self.terminals.len() {
            if self.terminals[idx].file_load.is_none() {
                continue;
            }
            self.active_terminal = idx;
            self.advance_active_file_load();
        }
        self.active_terminal = original.min(self.terminals.len() - 1);
    }

    fn advance_active_file_load(&mut self) {
        let Some(mut load) = self.active_terminal_mut().file_load.take() else {
            return;
        };
        let events = load.drain(crate::file_load::LOAD_EVENTS_PER_TICK);
        let events = if events.is_empty() {
            // Stall guard (issue #253). Probe liveness WITHOUT consuming
            // events silently: a Done/Failed that lands between the drain and
            // the check comes back in `probed` (a bare try_recv probe used to
            // eat it and misclassify the load as stalled).
            let (disconnected, probed) = load.probe();
            let stalled = self.active_terminal().file_load_stalled_at;
            let timed_out = stalled
                .is_some_and(|at| TerminalState::file_load_stall_expired(at, Instant::now()));
            if probed.is_empty() {
                if disconnected || timed_out {
                    // A disconnected worker can never post Done/Failed; a
                    // silent one gets the timeout backstop so the fast tick
                    // cadence cannot wedge forever.
                    self.fail_active_file_load("File load stalled — load aborted".to_string());
                    return;
                }
                let terminal = self.active_terminal_mut();
                if terminal.file_load_stalled_at.is_none() {
                    terminal.file_load_stalled_at = Some(Instant::now());
                }
                terminal.file_load = Some(load);
                return;
            }
            probed
        } else {
            events
        };
        // Progress observed: reset the stall clock (issue #253).
        self.active_terminal_mut().file_load_stalled_at = None;

        let display_path = load.path.clone();
        for event in events {
            match event {
                crate::file_load::LoadEvent::Progress {
                    lines,
                    content_done,
                    index_done,
                    content_lines_read,
                    index_progress,
                    tail_start_line,
                } => {
                    let was_empty = self.active_terminal().buffer.raw_lines_len() == 0;
                    let got_lines = !lines.is_empty();
                    if got_lines {
                        // Quiet ingest during load — paint only on first/last content batch.
                        self.push_lines(lines, false);
                        if let Some(tail_start) = tail_start_line {
                            let terminal = self.active_terminal_mut();
                            terminal.buffer_line_start = tail_start;
                            terminal.buffer_line_end = tail_start + content_lines_read;
                        }
                    }
                    if was_empty && got_lines || (content_done && got_lines) {
                        self.mark_all_views_dirty();
                        self.mark_viewport_dirty();
                    }
                    if content_done && !index_done {
                        let index_pct = (index_progress * 100.0) as u32;
                        // Throttle status churn: update only every ~5% while indexing.
                        if index_pct.is_multiple_of(5) || index_pct >= 99 {
                            self.status_message = format!(
                                "Indexing: {display_path}… ({index_pct}%, {content_lines_read} lines visible)"
                            );
                            self.push_event(
                                json!({"type":"status","message": self.status_message}),
                            );
                        }
                    } else if !content_done {
                        self.status_message =
                            format!("Loading: {display_path}… ({content_lines_read} lines)");
                        self.push_event(json!({"type":"status","message": self.status_message}));
                    }
                }
                crate::file_load::LoadEvent::Done {
                    backed,
                    content_lines_read,
                    tail_start_line,
                } => {
                    let total = backed.index.total_lines();
                    {
                        let terminal = self.active_terminal_mut();
                        if let Some(last) = terminal.parser.flush_pending() {
                            let _ = terminal.buffer.add(last);
                        }
                        // Window counters come from the load result, not ring shifts.
                        terminal.buffer_line_start = tail_start_line;
                        terminal.buffer_line_end = tail_start_line + content_lines_read;
                        terminal.file_backed = Some(*backed);
                        terminal.file_load = None;
                        terminal.file_load_stalled_at = None;
                    }
                    self.mark_all_views_dirty();
                    self.mark_viewport_dirty();
                    self.status_message =
                        format!("Opened: {display_path} ({total} lines, scroll for full file)");
                    self.push_event(json!({"type":"status","message": self.status_message}));
                    // Large files open on a capped tail chunk — pin EOF once rebuilt.
                    // Small files that fully fit in one window stay at scroll 0.
                    let window = self.file_view_window_lines() as u64;
                    let pin_eof = !self.active_view().uses_match_index()
                        && (content_lines_read < total || total > window);
                    self.active_terminal_mut().pending_open_scroll_eof = pin_eof;
                    // The handle stays out; drop the borrowed one.
                    return;
                }
                crate::file_load::LoadEvent::Failed(message) => {
                    let terminal = self.active_terminal_mut();
                    terminal.file_load = None;
                    terminal.file_load_stalled_at = None;
                    self.status_message = message.clone();
                    self.push_event(json!({"type":"status","message": message}));
                    return;
                }
            }
        }
        // Still loading: put the handle back.
        if self.active_terminal().file_load.is_none() {
            self.active_terminal_mut().file_load = Some(load);
        }
    }

    /// Drop the active terminal's pending load and surface a failure status
    /// event. Used both for worker-reported failures and for the stall guard
    /// (disconnected worker / timeout backstop, issue #253).
    fn fail_active_file_load(&mut self, message: String) {
        {
            let terminal = self.active_terminal_mut();
            terminal.file_load = None;
            terminal.file_load_stalled_at = None;
        }
        self.status_message = message.clone();
        self.push_event(json!({"type":"status","message": message}));
    }

    /// Detect external truncation / append / rewrite of open file sessions
    /// (issue #151): a throttled size+mtime stat per loaded session on the
    /// tick. A drifted session flips into the `file_changed` state until the
    /// user reloads; no automatic reload (manual-reload spec decision).
    pub(crate) fn poll_file_changes(&mut self) {
        if !self
            .last_file_watch_at
            .is_none_or(|at| at.elapsed() >= FILE_WATCH_INTERVAL)
        {
            return;
        }
        self.last_file_watch_at = Some(Instant::now());
        let mut changed: Vec<String> = Vec::new();
        for terminal in &mut self.terminals {
            // Still loading: the worker reads what is on disk; the open-time
            // baseline catches any drift on the next sweep.
            if terminal.file_load.is_some() || terminal.file_changed {
                continue;
            }
            let drifted = terminal
                .file_backed
                .as_ref()
                .is_some_and(|backed| backed.changed_on_disk());
            if drifted {
                terminal.file_changed = true;
                changed.push(terminal.label());
            }
        }
        if changed.is_empty() {
            return;
        }
        self.status_message = format!(
            "File changed on disk — Reload to refresh: {}",
            changed.join(", ")
        );
        self.push_event(json!({"type":"status","message": self.status_message}));
        // Flush stats promptly so the host sees the file_changed flag.
        self.last_stats_at = None;
    }

    /// After [`LoadEvent::Done`], scroll to EOF once flat lines and the global
    /// scroll range reflect the indexed file (not the provisional tail chunk).
    pub(crate) fn maybe_apply_open_eof_scroll(&mut self) {
        let original = self.active_terminal;
        for idx in 0..self.terminals.len() {
            if !self.terminals[idx].pending_open_scroll_eof {
                continue;
            }
            if self.terminals[idx].file_load.is_some() {
                continue;
            }
            self.active_terminal = idx;
            self.terminals[idx].pending_open_scroll_eof = false;
            if self.active_view().uses_match_index() {
                continue;
            }
            self.scroll_to_end();
        }
        self.active_terminal = original.min(self.terminals.len().saturating_sub(1));
    }

    /// Apply background file I/O results posted by worker threads (issue #55).
    /// Runs at the head of [`Engine::tick`] so landed windows/scans render in
    /// the same tick. Called again in tests to pump completion.
    pub(crate) fn apply_file_io_results(&mut self) {
        // Finish a window that landed while its terminal was inactive.
        if self.has_active_terminal() && self.pending_window_ready() {
            self.finish_active_pending_window();
        }
        let done: Vec<FileIoDone> =
            std::mem::take(&mut *self.file_io_done.lock().unwrap_or_else(|e| e.into_inner()));
        for item in done {
            match item {
                FileIoDone::Window {
                    term_id,
                    new_start,
                    scroll_y,
                    result,
                } => self.apply_window_result(&term_id, new_start, scroll_y, result),
                FileIoDone::MatchScan {
                    term_id,
                    view_idx,
                    token,
                    progress,
                } => self.apply_match_scan_result(&term_id, view_idx, token, progress),
                FileIoDone::MatchWindow {
                    term_id,
                    view_idx,
                    req,
                    start,
                    new_local,
                    result,
                } => self
                    .apply_match_window_result(&term_id, view_idx, req, start, new_local, result),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_tab(name: &str) -> crate::core::types::TabConfig {
        crate::core::types::TabConfig {
            name: name.to_string(),
            filters: Vec::new(),
            search_query: String::new(),
            search_regex: false,
            search_case_sensitive: false,
            search_whole_word: false,
            auto_follow: false,
            wrap_lines: false,
            severity: Default::default(),
        }
    }

    #[test]
    fn match_window_result_for_closed_view_is_dropped() {
        let mut engine = Engine::new();
        let term_id = engine.terminals[0].id.clone();
        engine.terminals[0]
            .views
            .push(LogView::from_tab_config(plain_tab("Filter")));
        engine.terminals[0].views[1].match_window_inflight = Some(7);

        // Simulate the tab being closed while the background read is in
        // flight; applying the result must not panic (issue #158).
        engine.terminals[0].views.remove(1);
        engine.apply_match_window_result(&term_id, 1, 7, 0, 0.0, Ok(Vec::new()));
        engine.apply_match_window_result(&term_id, 1, 7, 0, 0.0, Err("boom".to_string()));

        // A stale request for an existing view is still dropped silently.
        engine.terminals[0].views[0].match_window_inflight = None;
        engine.apply_match_window_result(&term_id, 0, 7, 0, 0.0, Ok(Vec::new()));
    }

    #[test]
    fn map_global_scroll_mid_file_is_not_eof() {
        let stride = 20.0;
        let viewport_h = 400.0;
        let total = 25_990_u64;
        let window = 2_000_u64;
        let max_start = total - window;
        let max_scroll = total as f32 * stride - viewport_h;
        // ~line 18k (the user's 16k–20k "black hole" band).
        let global = 18_000.0 * stride;
        let mapped = map_global_scroll_to_window(
            global, max_scroll, viewport_h, stride, total, window,
            max_start, // tail window resident
            window,
        );
        assert!(
            !mapped.near_eof,
            "mid-file absolute scroll must not pin EOF (new_start={})",
            mapped.new_start
        );
        assert!(
            mapped.new_start < max_start.saturating_sub(window / 4),
            "should recenter around line 18k, got new_start={} max_start={max_start}",
            mapped.new_start
        );
    }

    #[test]
    fn file_window_spawn_failure_drops_pending_and_reports() {
        let mut engine = Engine::new();
        let term_id = engine.terminals[0].id.clone();
        engine.terminals[0].pending_file_window = Some(PendingFileWindow {
            new_start: 10,
            scroll_y: 2.0,
            pin_to_end: false,
            next_line: 10,
            end_line: 20,
            lines: Vec::new(),
        });

        // A failed worker spawn must degrade like a failed read, not panic.
        engine.file_window_spawn_failed(&term_id, 10, 2.0, std::io::Error::other("boom"));

        assert!(engine.terminals[0].pending_file_window.is_none());
        assert_eq!(
            engine.status_message,
            "File window read failed to start: boom"
        );
        let event = engine.events.back().expect("status event");
        assert!(event.contains("File window read failed to start"));
    }

    #[test]
    fn match_scan_spawn_failure_resets_inflight_view() {
        let mut engine = Engine::new();
        let term_id = engine.terminals[0].id.clone();
        engine.terminals[0]
            .views
            .push(LogView::from_tab_config(plain_tab("Filter")));
        engine.terminals[0].active_view = 1;
        engine.terminals[0].views[1].match_scan_pos = Some(0);
        engine.terminals[0].views[1].match_scan_inflight = true;
        let token = engine.terminals[0].views[1].match_scan_token;

        engine.match_scan_spawn_failed(&term_id, 1, token, std::io::Error::other("boom"));

        // The view must not stay in-flight forever: the reset matches the
        // failed-scan path, so a later filter change can restart the scan.
        let view = &engine.terminals[0].views[1];
        assert!(view.match_offsets.is_empty());
        assert_eq!(view.match_scan_pos, None);
        assert!(!view.match_scan_inflight);
        assert_eq!(engine.status_message, "Filter scan failed to start: boom");
    }

    #[test]
    fn match_window_spawn_failure_clears_inflight_request() {
        let mut engine = Engine::new();
        let term_id = engine.terminals[0].id.clone();
        engine.terminals[0]
            .views
            .push(LogView::from_tab_config(plain_tab("Filter")));
        engine.terminals[0].active_view = 1;
        engine.terminals[0].views[1].match_window_inflight = Some(7);

        engine.match_window_spawn_failed(&term_id, 1, 7, 0, 0.0, std::io::Error::other("boom"));

        assert!(engine.terminals[0].views[1].match_window_inflight.is_none());
        assert_eq!(
            engine.status_message,
            "Filter window read failed to start: boom"
        );
    }

    #[test]
    fn match_index_reset_cancels_pending_file_window() {
        let mut engine = Engine::new();
        // `reset_file_match_viewport` only runs on file sessions.
        engine.terminals[0].launch.log_file = Some("dummy.log".into());
        engine.terminals[0]
            .views
            .push(LogView::from_tab_config(plain_tab("Filter")));
        engine.terminals[0].active_view = 1;
        // Include filter → match-index path.
        engine.terminals[0].views[1]
            .filters_mut()
            .push(crate::core::types::FilterRule {
                id: "f1".into(),
                name: None,
                filter_type: crate::core::types::FilterType::Include,
                pattern: "x".into(),
                enabled: true,
                use_regex: false,
                regex: None,
            });
        engine.terminals[0].buffer_line_start = 100;
        engine.terminals[0].pending_file_window = Some(PendingFileWindow {
            new_start: 200,
            scroll_y: 0.0,
            pin_to_end: false,
            next_line: 200,
            end_line: 300,
            lines: Vec::new(),
        });
        let term_id = engine.terminals[0].id.clone();

        engine.reset_file_match_viewport();
        assert!(
            engine.terminals[0].pending_file_window.is_none(),
            "entering match-index must drop in-flight sliding windows"
        );

        // A late worker result must not thrash buffer_line_start.
        engine.apply_window_result(&term_id, 200, 0.0, Ok(vec!["late".into()]));
        assert_eq!(engine.terminals[0].buffer_line_start, 100);
    }
}
