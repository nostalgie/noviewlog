use super::*;
use crate::spawn_resolver::ResolveResult;
use crate::terminal_state::PendingSpawn;

pub(crate) enum StartAction {
    File(String),
    Launch,
}

/// Human-readable exit status. Maps Windows NTSTATUS values that commonly appear
/// when ConPTY spawn goes wrong (e.g. Store App execution aliases).
fn format_process_exit_message(code: i32) -> String {
    const STATUS_DLL_INIT_FAILED: i32 = -1073741502; // 0xC0000142 as i32
    if code == STATUS_DLL_INIT_FAILED {
        return format!(
            "Process exited ({code} / 0xC0000142 STATUS_DLL_INIT_FAILED). \
             Often caused by a Microsoft Store node.exe stub under WindowsApps — \
             set Command to the full path of a real node.exe, or install Node from nodejs.org."
        );
    }
    format!("Process exited ({code})")
}

impl Engine {
    fn bump_pty_generation_for(&mut self, terminal_id: &str) -> u64 {
        let Some(term) = self.terminals.iter_mut().find(|t| t.id == terminal_id) else {
            return 0;
        };
        term.pty_generation = term.pty_generation.wrapping_add(1);
        term.pty_generation
    }

    pub(crate) fn poll_pty(&mut self) {
        // Spawns whose background resolution landed start here, before the
        // event drain, so fresh PTY output is ingested in the same tick.
        self.advance_pending_spawns();

        enum Coalesced {
            Bytes {
                id: String,
                data: Vec<u8>,
            },
            Exit {
                id: String,
                code: i32,
                generation: u64,
            },
        }

        let mut coalesced: Vec<Coalesced> = Vec::new();
        // Drain mode (issue #126): when the previous tick held work back, the
        // reader queue is backing up — widen the budget so ingest can outpace
        // the writer instead of pinning sustained throughput at base_budget ×
        // tick rate. The wall-clock guard below keeps either mode UI-safe.
        let mut byte_budget = if self.pty_hold.is_some() {
            PTY_INGEST_DRAIN_BYTES_PER_TICK
        } else {
            PTY_INGEST_BYTES_PER_TICK
        };
        let ingest_started = Instant::now();
        let mut more_pending = false;

        let mut pending_events: Vec<PtyEvent> = Vec::new();
        if let Some(held) = self.pty_hold.take() {
            pending_events.push(held);
        }
        loop {
            if byte_budget == 0 && pending_events.is_empty() {
                break;
            }
            let event = if let Some(ev) = pending_events.pop() {
                ev
            } else if byte_budget == 0 || ingest_started.elapsed() >= PTY_INGEST_TIME_BUDGET {
                break;
            } else {
                match self.pty_rx.try_recv() {
                    Ok(ev) => ev,
                    Err(_) => break,
                }
            };
            match event {
                PtyEvent::Bytes {
                    id,
                    data,
                    generation,
                } => {
                    // Stale output from a stopped / restarted session (issue
                    // #56): drop instead of polluting the new session's buffer.
                    if !self
                        .terminals
                        .iter()
                        .any(|t| t.id == id && t.pty_generation == generation)
                    {
                        continue;
                    }
                    if data.len() > byte_budget && !coalesced.is_empty() {
                        self.pty_hold = Some(PtyEvent::Bytes {
                            id,
                            data,
                            generation,
                        });
                        more_pending = true;
                        break;
                    }
                    byte_budget = byte_budget.saturating_sub(data.len());
                    if let Some(Coalesced::Bytes {
                        id: last_id,
                        data: buf,
                    }) = coalesced.last_mut()
                    {
                        if *last_id == id {
                            buf.extend_from_slice(&data);
                        } else {
                            coalesced.push(Coalesced::Bytes { id, data });
                        }
                    } else {
                        coalesced.push(Coalesced::Bytes { id, data });
                    }
                    if byte_budget == 0 {
                        if let Ok(ev) = self.pty_rx.try_recv() {
                            self.pty_hold = Some(ev);
                            more_pending = true;
                        }
                        break;
                    }
                }
                PtyEvent::Exit {
                    id,
                    code,
                    generation,
                } => {
                    coalesced.push(Coalesced::Exit {
                        id,
                        code,
                        generation,
                    });
                }
            }
        }
        if self.pty_hold.is_none() {
            if let Ok(ev) = self.pty_rx.try_recv() {
                self.pty_hold = Some(ev);
                more_pending = true;
            }
        } else {
            more_pending = true;
        }

        let mut active_changed = false;
        let mut chrome_changed = false;
        let mut respawn_shell_ids: Vec<String> = Vec::new();
        let active_id = self
            .terminals
            .get(self.active_terminal)
            .map(|t| t.id.clone());

        for event in coalesced {
            match event {
                Coalesced::Bytes { id, data } => {
                    let Some(term_idx) = self.terminals.iter().position(|t| t.id == id) else {
                        continue;
                    };
                    let is_active = active_id.as_ref() == Some(&id);
                    let skip_logview_patch = is_active && self.paints_live_vt_grid();
                    let old_overlay = if skip_logview_patch {
                        0
                    } else {
                        self.terminals[term_idx]
                            .views
                            .first()
                            .map(|v| v.overlay_len())
                            .unwrap_or(0)
                    };
                    let old_total = self.terminals[term_idx].buffer.records_len();

                    let (shifted_records, shifted_lines) = {
                        let term = &mut self.terminals[term_idx];
                        let (records, lines) =
                            term.ingest.feed(&data, &mut term.buffer, &mut term.parser);
                        if let Some(cwd) = term.ingest.take_cwd_update() {
                            term.cwd = cwd;
                            chrome_changed = true;
                        }
                        term.last_line_at = Some(Instant::now());
                        (records, lines)
                    };

                    if is_active {
                        active_changed = true;
                        if skip_logview_patch {
                            if shifted_lines > 0 {
                                let terminal = &mut self.terminals[term_idx];
                                for (i, view) in terminal.views.iter_mut().enumerate() {
                                    if i != 0 {
                                        view.mark_flat_lines_dirty();
                                    }
                                }
                            }
                            self.snap_follow_scroll_after_ingest(term_idx);
                        } else {
                            let overlay = self.terminals[term_idx].ingest.overlay_flat_lines();
                            let new_total = self.terminals[term_idx].buffer.records_len();
                            self.apply_active_pty_bytes_to_views(
                                term_idx,
                                old_overlay,
                                old_total,
                                overlay,
                                new_total,
                                shifted_records,
                                shifted_lines,
                            );
                            self.snap_follow_scroll_after_ingest(term_idx);
                        }
                    } else if shifted_lines > 0 {
                        for view in &mut self.terminals[term_idx].views {
                            view.mark_flat_lines_dirty();
                        }
                    }
                }
                Coalesced::Exit {
                    id,
                    code,
                    generation,
                } => {
                    if self
                        .ptys
                        .get(&id)
                        .is_some_and(|p| p.generation() != generation)
                    {
                        continue;
                    }
                    let Some(term) = self.terminals.iter_mut().find(|t| t.id == id) else {
                        continue;
                    };
                    if term.pty_generation != generation {
                        continue;
                    }
                    term.ingest.finish(&mut term.buffer, &mut term.parser);
                    term.running = false;
                    term.exit_code = Some(code);
                    let file_session = term.is_file_session();
                    let has_launch_command = term.launch.command.is_some();
                    self.ptys.remove(&id);
                    let message = format_process_exit_message(code);
                    chrome_changed = true;
                    if active_id.as_ref() == Some(&id) {
                        self.status_message = message.clone();
                        active_changed = true;
                        self.mark_all_views_dirty();
                        self.materialize_live_terminal_tab();
                        self.mark_viewport_dirty();
                        self.push_event(json!({"type":"exit","code": code, "message": message}));
                    }
                    // Programs with a saved command stay stopped; plain shells respawn.
                    if !file_session && !has_launch_command {
                        respawn_shell_ids.push(id);
                    }
                }
            }
        }

        if chrome_changed {
            self.last_stats_at = None;
        }
        if active_changed {
            // Follow is snapped in apply_active_pty_bytes / snap_follow_scroll_after_ingest
            // on every chunk. Paint dirty only at display cadence while more PTY work
            // remains — full paint per 256 KB made cat feel hitchy vs a native terminal.
            self.mark_viewport_dirty_after_pty_ingest(more_pending);
        }

        if more_pending {
            // Host must NOT immediate-retick. TICK_FAST (~33 ms) continues the drain.
            // Immediate schedule_host_tick + reader wake was a 1-core busy ingest loop.
            self.pty_drain_pending = true;
        }

        self.last_pty_poll_at = Some(Instant::now());

        for id in respawn_shell_ids {
            self.start_interactive_shell_for(&id);
        }
        self.flush_idle_pending();
    }

    /// Patch Terminal tab flat lines for a volatile VT update.
    /// Filter tabs are not live-patched; the active filter tab is dirtied only
    /// when Records commit so rebuild_if_needed can rescan the ≤30k ring.
    ///
    /// When the ring drops a flat-line prefix, anchors `scroll_offset_y` (and selection)
    /// so scrolled-up content does not slide under the viewport — same idea as FILES
    /// window `scroll_adjust`.
    fn apply_active_pty_bytes_to_views(
        &mut self,
        term_idx: usize,
        old_overlay: usize,
        old_total: usize,
        overlay: Vec<crate::core::types::FlatLine>,
        new_total: usize,
        shifted_records: usize,
        shifted_flat_lines: usize,
    ) {
        let row_stride = self.renderer.metrics().row_stride;
        let cell_width = self.renderer.metrics().cell_width;
        let viewport_width = self.viewport_width;

        let terminal = &mut self.terminals[term_idx];
        let active_view = terminal.active_view;
        let records_changed = new_total != old_total || shifted_flat_lines > 0;
        for (i, view) in terminal.views.iter_mut().enumerate() {
            if i == 0 {
                continue;
            }
            // Inactive filter tabs stay stale until selected. Overlay-only
            // frames must not rebuild or replace an active filter tab with
            // the ~40-row live screen (that was the 1↔11 jump).
            if !records_changed || i != active_view {
                continue;
            }
            view.mark_flat_lines_dirty();
        }

        let follow = terminal
            .views
            .first()
            .map(|v| v.auto_follow)
            .unwrap_or(false);
        let wrap = terminal
            .views
            .first()
            .map(|v| v.wrap_lines)
            .unwrap_or(false);

        // Visual height of the flat prefix that will disappear from the top.
        // Wrap OFF: 1 flat line == 1 visual row (matches scroll_by_lines).
        // Wrap ON: measure the prefix about to be drained (stable head only).
        let dropped_h = if shifted_flat_lines == 0 {
            0.0
        } else if !wrap {
            shifted_flat_lines as f32 * row_stride
        } else if let Some(tab) = terminal.views.first() {
            let n = shifted_flat_lines.min(tab.flat_lines.len().saturating_sub(old_overlay));
            if n == 0 {
                0.0
            } else {
                let rows =
                    count_visual_rows(&tab.flat_lines[..n], true, viewport_width, cell_width);
                rows as f32 * row_stride
            }
        } else {
            0.0
        };

        let TerminalState {
            views,
            buffer,
            scroll_offset_y,
            selection,
            ..
        } = terminal;
        let Some(terminal_tab) = views.get_mut(0) else {
            return;
        };
        let patched = terminal_tab.try_patch_committed_and_overlay(
            buffer,
            old_overlay,
            old_total,
            &overlay,
            new_total,
            shifted_records,
            shifted_flat_lines,
        );
        if !patched {
            terminal_tab.mark_flat_lines_dirty();
            terminal_tab.rebuild(buffer);
            terminal_tab.set_live_overlay(overlay);
        }

        // Anchor even if patch falls back to dirty rebuild — buffer already trimmed by `shifted`.
        if shifted_flat_lines > 0 {
            if !follow && dropped_h > 0.0 {
                *scroll_offset_y = (*scroll_offset_y - dropped_h).max(0.0);
            }
            if let Some(sel) = selection.as_mut() {
                shift_selection_after_prefix_drop(sel, shifted_flat_lines);
            }
        }
    }

    /// Follow + Terminal tab + running PTY: paint the VTE cell grid, not LogView overlay.
    pub(crate) fn paints_live_vt_grid(&self) -> bool {
        if !self.has_active_terminal() {
            return false;
        }
        let term = self.active_terminal();
        if term.is_file_session() || !term.running || term.active_view != 0 {
            return false;
        }
        let view = term.active_view();
        view.auto_follow && view.search_query.is_empty()
    }

    /// Rebuild Terminal tab committed prefix + overlay after leaving live-grid Follow.
    pub(crate) fn materialize_live_terminal_tab(&mut self) {
        if !self.has_active_terminal() {
            return;
        }
        let terminal = self.active_terminal_mut();
        if terminal.is_file_session() || terminal.active_view != 0 {
            return;
        }
        let overlay = terminal.ingest.overlay_flat_lines();
        let TerminalState { views, buffer, .. } = terminal;
        let view = &mut views[0];
        view.mark_flat_lines_dirty();
        view.rebuild(buffer);
        view.set_live_overlay(overlay);
    }

    /// Pin Follow to the content bottom immediately after ingest (not only in `render`),
    /// so stats/scrollbar do not lag a frame behind growing `max_scroll`.
    pub(crate) fn snap_follow_scroll_after_ingest(&mut self, term_idx: usize) {
        let Some(term) = self.terminals.get(term_idx) else {
            return;
        };
        if term.is_file_session() {
            return;
        }
        let Some(view) = term.views.first() else {
            return;
        };
        if !view.auto_follow || !view.search_query.is_empty() {
            return;
        }
        // max_scroll_offset uses active terminal — only snap when this term is active.
        if self.active_terminal != term_idx {
            return;
        }
        let max = self.max_scroll_offset();
        self.terminals[term_idx].scroll_offset_y = max;
    }

    pub(crate) fn start_launch_process(&mut self) {
        if self.active_terminal().is_file_session() {
            self.status_message = "File terminal is view-only".to_string();
            self.push_event(json!({"type":"status","message": self.status_message}));
            return;
        }
        let id = self.active_terminal().id.clone();
        let launch = self.active_terminal().launch.clone();
        let (command, args, cwd) = match resolve_process_launch(&launch) {
            Ok(resolved) => resolved,
            Err(err) => {
                self.active_terminal_mut().running = false;
                self.status_message = format!("Failed to start: {err}");
                self.push_event(json!({"type":"status","message": self.status_message}));
                return;
            }
        };
        let cmdline = crate::spawn_resolve::format_spawn_cmdline(&command, &args, cwd.as_deref());
        self.status_message = format!("Running: {cmdline}");
        self.push_event(json!({"type":"status","message": self.status_message}));
        // New Program: drop the previous child's screen. Keeping it as
        // scrollback made Follow (live grid) and overlay (wheel / Enter)
        // show different buffers, and typing after exit spawned a shell
        // on top of uname output.
        let term_size = self.viewport_pty_size();
        let generation = self.bump_pty_generation_for(&id);
        if let Some(pty) = self.ptys.get_mut(&id) {
            pty.stop();
        }
        {
            let terminal = self.active_terminal_mut();
            terminal.buffer.clear();
            terminal
                .ingest
                .reset_with_size(term_size.cols as usize, term_size.rows as usize);
            terminal.scroll_offset_y = 0.0;
            for view in &mut terminal.views {
                view.clear_flat_lines();
            }
        }
        self.active_terminal_mut().exit_code = None;
        self.active_terminal_mut().running = true;
        self.sync_terminal_geometry();
        let pending = PendingSpawn {
            command,
            args,
            workdir: expand_spawn_cwd(cwd),
            generation,
            cmdline,
            fail_prefix: "Failed to start".to_string(),
        };
        self.queue_spawn(&id, pending);
    }

    /// Start `$SHELL` / PowerShell / WSL bash for the active terminal.
    pub(crate) fn start_interactive_shell(&mut self) {
        let id = self.active_terminal().id.clone();
        self.start_interactive_shell_for(&id);
    }

    /// Start an interactive shell for a specific terminal (local / WSL / SSH later).
    pub(crate) fn start_interactive_shell_for(&mut self, terminal_id: &str) {
        let Some(term_idx) = self.terminals.iter().position(|t| t.id == terminal_id) else {
            return;
        };
        // Already running — nothing to do (avoids double-spawn races).
        if self.terminals[term_idx].running {
            return;
        }
        if self.terminals[term_idx].is_file_session() {
            return;
        }

        let mut launch = self.terminals[term_idx].launch.clone();
        if launch.cwd.is_none() {
            launch.cwd = Some(self.terminals[term_idx].cwd.clone());
        }
        let (command, args, cwd) = match resolve_interactive_shell(&launch, self.config.shell) {
            Ok(resolved) => resolved,
            Err(err) => {
                self.status_message = format!("Shell: {err}");
                self.push_event(json!({"type":"status","message": self.status_message}));
                return;
            }
        };
        let cmdline = crate::spawn_resolve::format_spawn_cmdline(&command, &args, cwd.as_deref());
        let is_active = term_idx == self.active_terminal;
        if is_active {
            self.status_message = format!("Shell: {cmdline}");
            self.push_event(json!({"type":"status","message": self.status_message}));
        }
        {
            let term = &mut self.terminals[term_idx];
            term.exit_code = None;
            term.process_started = true;
            term.running = true;
        }
        if is_active {
            self.sync_terminal_geometry();
        }
        let id = terminal_id.to_string();
        let generation = self.bump_pty_generation_for(&id);
        let pending = PendingSpawn {
            command,
            args,
            workdir: expand_spawn_cwd(cwd),
            generation,
            cmdline,
            fail_prefix: "Shell failed".to_string(),
        };
        self.queue_spawn(&id, pending);
        if is_active {
            self.mark_viewport_dirty();
        }
        self.last_stats_at = None;
    }

    /// Park a spawn for background executable resolution (issue #59), or start
    /// the PTY immediately when the resolution is already cached. Either way
    /// the UI thread performs **zero** filesystem / registry probing here.
    fn queue_spawn(&mut self, terminal_id: &str, pending: PendingSpawn) {
        let Some(term_idx) = self.terminals.iter().position(|t| t.id == terminal_id) else {
            return;
        };
        match self.spawn_resolver.request_fresh(
            &pending.command,
            pending.args.clone(),
            &pending.workdir,
        ) {
            None => {
                // Cold cache or retrying a stale failure: resolution runs on
                // the worker kicked above; `advance_pending_spawns` applies
                // it when it lands.
                self.terminals[term_idx].pending_spawn = Some(pending);
            }
            Some(result) => {
                self.finish_spawn(term_idx, pending, result);
            }
        }
    }

    /// Apply parked spawns whose background resolution has landed
    /// (issue #59). Called at the head of [`Engine::poll_pty`] so a freshly
    /// started PTY drains output in the same tick.
    pub(crate) fn advance_pending_spawns(&mut self) {
        let mut ready: Vec<(usize, PendingSpawn, ResolveResult)> = Vec::new();
        for (idx, term) in self.terminals.iter().enumerate() {
            let Some(pending) = term.pending_spawn.as_ref() else {
                continue;
            };
            if let Some(result) = self.spawn_resolver.request(
                &pending.command,
                pending.args.clone(),
                &pending.workdir,
            ) {
                ready.push((idx, pending.clone(), result));
            }
        }
        for (idx, pending, result) in ready {
            self.finish_spawn(idx, pending, result);
        }
    }

    /// Start the PTY from a resolved plan; surface failures and flush stdin
    /// buffered while resolution was in flight.
    fn finish_spawn(&mut self, term_idx: usize, pending: PendingSpawn, result: ResolveResult) {
        let Some(term) = self.terminals.get_mut(term_idx) else {
            return;
        };
        term.pending_spawn = None;
        // A newer session superseded this one (Stop / restart while resolving).
        if term.pty_generation != pending.generation || !term.running {
            return;
        }
        let id = term.id.clone();
        let is_active = term_idx == self.active_terminal;
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(err) => {
                self.fail_spawn(term_idx, &id, &pending, &err);
                return;
            }
        };
        let size = self.viewport_pty_size();
        let start_result = {
            let pty = self.ptys.entry(id.clone()).or_default();
            let _ = pty.set_size(size);
            pty.start_prepared(
                self.pty_tx.clone(),
                id.clone(),
                prepared,
                pending.generation,
                self.pty_activity_wake.clone(),
            )
        };
        if let Err(err) = start_result {
            self.fail_spawn(term_idx, &id, &pending, &err);
        } else {
            let term = &mut self.terminals[term_idx];
            term.ingest.ensure_live_screen(&mut term.buffer);
            let buffered = std::mem::take(&mut term.pending_stdin);
            if !buffered.is_empty() {
                if let Some(pty) = self.ptys.get_mut(&id) {
                    let _ = pty.write_bytes(&buffered);
                }
            }
            if is_active {
                self.mark_all_views_dirty();
                self.mark_viewport_dirty();
            }
            self.last_stats_at = None;
        }
    }

    /// Shared spawn-failure path: stop the session and always surface the
    /// error — the terminal may sit in the background while a slow
    /// resolution lands (issue #59). The status line and repaint stay
    /// active-tab-only so a background failure cannot overwrite what the
    /// user is currently looking at.
    fn fail_spawn(&mut self, term_idx: usize, id: &str, pending: &PendingSpawn, err: &str) {
        self.terminals[term_idx].running = false;
        self.ptys.remove(id);
        self.last_stats_at = None;
        let message = format!(
            "{}: {err} | resolved: {}",
            pending.fail_prefix, pending.cmdline
        );
        if term_idx == self.active_terminal {
            self.status_message = message.clone();
            self.mark_viewport_dirty();
        }
        self.push_event(json!({"type":"status","message": message}));
    }

    pub(crate) fn stop(&mut self, terminal_id: Option<&str>) {
        let id = terminal_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.active_terminal().id.clone());
        // Stale ids must not report success (issue #66): "Stopped" only when
        // the terminal actually exists.
        if !self.terminals.iter().any(|t| t.id == id) {
            self.status_message = format!("Cannot stop: unknown terminal {id}");
            self.push_event(json!({"type":"status","message": self.status_message}));
            self.last_stats_at = None;
            return;
        }
        if let Some(mut pty) = self.ptys.remove(&id) {
            pty.stop();
        }
        if let Some(terminal) = self.terminals.iter_mut().find(|t| t.id == id) {
            terminal.running = false;
            // Cancel a spawn whose resolution is still in flight (issue #59):
            // `finish_spawn` drops late results via the running/generation guard.
            terminal.pending_spawn = None;
            terminal.pending_stdin.clear();
        }
        self.status_message = "Stopped".to_string();
        self.push_event(json!({"type":"status","message": "Stopped"}));
        self.last_stats_at = None;
    }

    pub(crate) fn terminal_add(&mut self) {
        self.terminal_add_blank();
        self.start_interactive_shell();
    }

    /// Create and activate a terminal without starting a shell (used for file viewers).
    pub(crate) fn terminal_add_blank(&mut self) {
        let runtime = build_runtime_config(&self.config, Some(&self.preset_name));
        let format = self.current_format();
        let id = next_terminal_id(&self.terminals);
        let terminal = TerminalState::new(
            id,
            LaunchConfig::default(),
            &runtime,
            &format,
            self.config.max_scrollback_lines,
        );
        self.terminals.push(terminal);
        self.active_terminal = self.terminals.len() - 1;
        self.mark_viewport_dirty();
        self.last_stats_at = None;
    }

    pub(crate) fn terminal_switch(&mut self, terminal_id: &str) {
        let Some(idx) = self.terminals.iter().position(|t| t.id == terminal_id) else {
            // Stale id (closed elsewhere): surface instead of silent no-op (#66).
            self.status_message = format!("Cannot switch: unknown terminal {terminal_id}");
            self.push_event(json!({"type":"status","message": self.status_message}));
            return;
        };
        if idx != self.active_terminal {
            self.active_terminal = idx;
            self.mark_all_views_dirty();
            self.mark_viewport_dirty();
            self.last_stats_at = None;
        }
        self.maybe_lazy_load_active_file();
    }

    pub(crate) fn terminal_move(&mut self, terminal_id: &str, to_index: usize) {
        let Some(from) = self.terminals.iter().position(|t| t.id == terminal_id) else {
            return;
        };
        if self.terminals.len() < 2 {
            return;
        }
        let to = to_index.min(self.terminals.len() - 1);
        if from == to {
            return;
        }
        let item = self.terminals.remove(from);
        self.terminals.insert(to, item);
        let active = self.active_terminal;
        self.active_terminal = if active == from {
            to
        } else if from < active && to >= active {
            active - 1
        } else if from > active && to <= active {
            active + 1
        } else {
            active
        };
        self.last_stats_at = None;
        self.sync_active_project_from_terminals();
    }

    /// Set a custom sidebar title. Empty/whitespace or unknown id → no-op (does not clear).
    pub(crate) fn terminal_rename(&mut self, terminal_id: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let Some(term) = self.terminals.iter_mut().find(|t| t.id == terminal_id) else {
            return;
        };
        term.custom_title = Some(name.to_string());
        self.last_stats_at = None;
        self.sync_active_project_from_terminals();
    }

    pub(crate) fn terminal_start(&mut self, terminal_id: Option<&str>) {
        if let Some(id) = terminal_id {
            self.terminal_switch(id);
        }
        let (has_command, log_file) = {
            let launch = &self.active_terminal().launch;
            (launch.command.is_some(), launch.log_file.clone())
        };
        if has_command {
            self.active_terminal_mut().process_started = true;
            self.start_launch_process();
        } else if let Some(path) = log_file {
            self.active_terminal_mut().process_started = true;
            self.start_log_file_load(&path);
        } else {
            self.start_interactive_shell();
        }
    }

    pub(crate) fn terminal_close(&mut self, terminal_id: Option<&str>) {
        if self.terminals.is_empty() {
            return;
        }
        let idx = match terminal_id {
            Some(id) => self.terminals.iter().position(|t| t.id == id),
            None => Some(self.active_terminal),
        };
        let Some(idx) = idx else {
            return;
        };
        let is_file = self.terminals[idx].is_file_session();
        let live_count = self
            .terminals
            .iter()
            .filter(|t| !t.is_file_session())
            .count();

        // Never close the last live terminal (FILES may be empty).
        if !is_file && live_count <= 1 {
            return;
        }

        // Never leave the engine with zero sessions.
        if self.terminals.len() <= 1 {
            if is_file {
                // Last session is a file: replace with a blank live terminal.
                let id = self.terminals[idx].id.clone();
                if let Some(mut pty) = self.ptys.remove(&id) {
                    pty.stop();
                }
                super::tabs::cancel_terminal_match_scans(&mut self.terminals[idx]);
                self.terminals.remove(idx);
                self.ensure_valid_state();
                self.sync_active_project_from_terminals();
                self.mark_viewport_dirty();
                self.last_stats_at = None;
            }
            return;
        }

        let id = self.terminals[idx].id.clone();
        if let Some(mut pty) = self.ptys.remove(&id) {
            pty.stop();
        }
        super::tabs::cancel_terminal_match_scans(&mut self.terminals[idx]);
        self.terminals.remove(idx);
        if self.active_terminal >= self.terminals.len() {
            self.active_terminal = self.terminals.len() - 1;
        } else if idx < self.active_terminal {
            self.active_terminal -= 1;
        }
        self.mark_viewport_dirty();
        self.last_stats_at = None;
        self.sync_active_project_from_terminals();
    }
}

fn shift_selection_after_prefix_drop(sel: &mut TextSelection, dropped_flat: usize) {
    if dropped_flat == 0 {
        return;
    }
    let shift = |pos: &mut crate::viewport_layout::TextPos| {
        if pos.line_index < dropped_flat {
            pos.line_index = 0;
            pos.byte_offset = 0;
        } else {
            pos.line_index -= dropped_flat;
        }
    };
    shift(&mut sel.anchor);
    shift(&mut sel.caret);
}

/// Expand a launch cwd into the working directory the resolver / PTY see
/// (`~` expansion, parent-process cwd fallback) — the same input
/// `PtyManager::start_prepared` used to derive before resolution moved off
/// the UI thread (issue #59).
pub(crate) fn expand_spawn_cwd(cwd: Option<String>) -> String {
    crate::core::config::expand_path_opt(cwd)
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| ".".to_string())
}
