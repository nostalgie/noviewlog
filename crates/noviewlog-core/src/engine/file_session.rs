use super::*;
use std::sync::atomic::{AtomicBool, Ordering};

/// How often the tick sweeps open file sessions for external changes
/// (issue #151). One cheap stat per loaded session per sweep.
pub(crate) const FILE_WATCH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

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
                        }
                    } else if !content_done {
                        self.status_message =
                            format!("Loading: {display_path}… ({content_lines_read} lines)");
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
                            let shifted = terminal.buffer.add(last);
                            terminal.buffer_line_start += shifted as u64;
                        }
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
            self.request_file_window_at(new_start, new_local);
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
            self.request_file_window_at(new_start, new_local);
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
        let max_start = total.saturating_sub(window);
        let target_line = ((global_offset / stride).floor() as u64).min(total.saturating_sub(1));

        // Pin the last window when the thumb is at / near EOF.
        let near_eof = max_scroll <= 0.5
            || global_offset + viewport_h >= max_scroll
            || target_line >= max_start;

        let (new_start, local_raw) = if near_eof {
            let local = (global_offset - max_start as f32 * stride).max(0.0);
            (max_start, local)
        } else {
            let win_len = end.saturating_sub(start).max(1);
            let margin = (win_len / 5).max(1);
            let comfortably_inside =
                target_line >= start.saturating_add(margin) && target_line + margin < end;
            if comfortably_inside {
                let local = global_offset - start as f32 * stride;
                let local_max = self.local_window_max_scroll();
                self.active_terminal_mut().scroll_offset_y = local.clamp(0.0, local_max);
                self.maybe_prefetch_file_window();
                self.mark_viewport_dirty();
                return;
            }
            let new_start = target_line.saturating_sub(window / 3).min(max_start);
            let local = (global_offset - new_start as f32 * stride).max(0.0);
            (new_start, local)
        };

        let local_cap = window as f32 * stride;
        let local = local_raw.min(local_cap);

        if new_start == start && end > start {
            let local_max = self.local_window_max_scroll();
            let local = if near_eof {
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
        // Near EOF: ask for the bottom of the window; finish clamps to real local_max
        // (Wrap ON can make visual height > raw window * stride).
        let pending_local = if near_eof { f32::MAX } else { local };
        self.request_file_window_at(new_start, pending_local);
        self.mark_viewport_dirty();
    }

    pub(crate) fn file_view_window_lines(&self) -> usize {
        self.config
            .max_scrollback_lines
            .min(WINDOW_RAW_LINES)
            .min(FILE_VIEW_WINDOW_LINES)
    }

    pub(crate) fn request_file_window_at(&mut self, new_start: u64, scroll_y: f32) {
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
    fn file_window_spawn_failed(
        &mut self,
        term_id: &str,
        new_start: u64,
        scroll_y: f32,
        err: std::io::Error,
    ) {
        let message = format!("File window read failed to start: {err}");
        self.apply_window_result(term_id, new_start, scroll_y, Err(message));
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

    fn pending_window_ready(&self) -> bool {
        self.active_terminal()
            .pending_file_window
            .as_ref()
            .is_some_and(|p| p.next_line >= p.end_line)
    }

    /// Monotonic request id for background match-window reads; results with a
    /// stale id are dropped.
    fn next_match_window_req(&mut self) -> u64 {
        self.match_window_req = self.match_window_req.wrapping_add(1);
        self.match_window_req
    }

    /// Apply a finished match-window read (issue #55).
    fn apply_match_window_result(
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

    fn finish_active_pending_window(&mut self) {
        let pending = self
            .active_terminal_mut()
            .pending_file_window
            .take()
            .expect("checked by pending_window_ready");
        self.finish_file_window(self.active_terminal, pending);
    }

    fn apply_window_result(
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

    fn apply_match_scan_result(
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
                let view = &mut self.terminals[idx].views[view_idx];
                view.match_scan_pos = Some(next);
                if !is_active {
                    return;
                }
                let pct = if file_size == 0 {
                    100
                } else {
                    ((next as f32 / file_size as f32) * 100.0) as u32
                };
                // Throttle status churn: update every ~5% (same idea as file indexing).
                let prev_pct = self
                    .status_message
                    .strip_prefix("Scanning filters… ")
                    .and_then(|rest| rest.split('%').next())
                    .and_then(|s| s.parse::<u32>().ok())
                    .unwrap_or(0);
                if pct >= 99 || pct / 5 > prev_pct / 5 {
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

    pub(crate) fn finish_file_window(&mut self, term_idx: usize, pending: PendingFileWindow) {
        let format = self.current_format();
        let raw_count = pending.lines.len() as u64;
        let desired_scroll = pending.scroll_y;
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
            terminal.scroll_offset_y = terminal.scroll_offset_y.clamp(0.0, local_max);
        }
        self.mark_viewport_dirty();
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

        let window = crate::file_match::MATCH_WINDOW_LINES;
        let max_start = total.saturating_sub(window);
        let target = ((global_offset / stride).floor() as usize).min(total.saturating_sub(1));
        let near_end =
            max_scroll <= 0.5 || global_offset + viewport_h >= max_scroll || target >= max_start;

        let (new_start, local_raw) = if near_end {
            let local = (global_offset - max_start as f32 * stride).max(0.0);
            (max_start, local)
        } else {
            let start = self.active_view().match_window_start;
            let loaded = self.active_view().flat_lines.len();
            let end = start.saturating_add(loaded);
            let margin = (loaded / 5).max(1);
            let comfortably_inside =
                loaded > 0 && target >= start.saturating_add(margin) && target + margin < end;
            if comfortably_inside {
                let local = global_offset - start as f32 * stride;
                let local_max = self.local_window_max_scroll();
                self.active_terminal_mut().scroll_offset_y = local.clamp(0.0, local_max);
                self.mark_viewport_dirty();
                return;
            }
            let new_start = target.saturating_sub(window / 3).min(max_start);
            let local = (global_offset - new_start as f32 * stride).max(0.0);
            (new_start, local)
        };

        self.active_view_mut().match_window_start = new_start;
        self.active_terminal_mut().scroll_offset_y = local_raw;
        self.apply_match_window();
        let local_max = self.local_window_max_scroll();
        let terminal = self.active_terminal_mut();
        terminal.scroll_offset_y = if near_end {
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
    fn match_scan_spawn_failed(
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
    fn match_scan_cap(&self) -> usize {
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
        let margin = (loaded / 5).max(1);
        let need_recenter = loaded == 0
            || target < start.saturating_add(margin)
            || (end > start && target + margin >= end);
        if !need_recenter {
            let local_max = self.local_window_max_scroll();
            self.active_terminal_mut().scroll_offset_y = local.clamp(0.0, local_max);
            return;
        }
        start = target.saturating_sub(window / 4).min(max_start);
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
    fn match_window_spawn_failed(
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
        }
        self.status_message = "Scanning filters… 0% (0 matches)".to_string();
        self.push_event(json!({"type":"status","message": self.status_message}));
        self.mark_viewport_dirty();
        self.last_stats_at = None;
    }
}

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
    fn file_window_spawn_failure_drops_pending_and_reports() {
        let mut engine = Engine::new();
        let term_id = engine.terminals[0].id.clone();
        engine.terminals[0].pending_file_window = Some(PendingFileWindow {
            new_start: 10,
            scroll_y: 2.0,
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
}
