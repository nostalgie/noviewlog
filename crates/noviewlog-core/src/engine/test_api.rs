use super::*;

impl Engine {
    #[cfg(test)]
    pub fn active_is_file_session_for_test(&self) -> bool {
        self.has_active_terminal() && self.active_terminal().is_file_session()
    }

    #[cfg(test)]
    pub fn active_terminal_running_for_test(&self) -> bool {
        self.has_active_terminal() && self.active_terminal().running
    }

    #[cfg(test)]
    pub fn mark_active_process_started_for_test(&mut self) {
        if self.has_active_terminal() {
            self.active_terminal_mut().process_started = true;
        }
    }

    #[cfg(test)]
    pub fn process_started_for_test(&self) -> bool {
        self.has_active_terminal() && self.active_terminal().process_started
    }

    #[cfg(test)]
    pub fn pty_generation_for_test(&self) -> u64 {
        if !self.has_active_terminal() {
            return 0;
        }
        self.active_terminal().pty_generation
    }

    #[cfg(test)]
    pub fn exit_code_for_test(&self) -> Option<i32> {
        if !self.has_active_terminal() {
            return None;
        }
        self.active_terminal().exit_code
    }

    #[cfg(test)]
    pub fn inject_pty_exit_for_test(&self, id: &str, code: i32, generation: u64) {
        let _ = self.pty_tx.send(crate::pty::PtyEvent::Exit {
            id: id.to_string(),
            code,
            generation,
        });
    }

    #[cfg(test)]
    pub fn status_message_for_test(&self) -> String {
        self.status_message.clone()
    }

    #[cfg(test)]
    pub fn terminal_is_file_session_for_test(&self, index: usize) -> bool {
        self.terminals
            .get(index)
            .is_some_and(|t| t.is_file_session())
    }

    #[cfg(test)]
    pub fn finish_pending_file_window_for_test(&mut self) {
        // Window reads run on worker threads (issue #55): pump ticks until
        // the result lands in the inbox and is applied.
        for _ in 0..100_000 {
            if !self.has_active_terminal() || self.active_terminal().pending_file_window.is_none() {
                return;
            }
            self.apply_file_io_results();
            self.rebuild_if_needed();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("pending file window never completed");
    }

    #[cfg(test)]
    pub fn pending_file_window_for_test(&self) -> bool {
        self.has_active_terminal() && self.active_terminal().pending_file_window.is_some()
    }

    #[cfg(test)]
    pub fn rebuild_if_needed_for_test(&mut self) {
        self.rebuild_if_needed();
    }

    #[cfg(test)]
    pub fn mark_running_for_test(&mut self) {
        if self.has_active_terminal() {
            self.active_terminal_mut().running = true;
        }
    }

    #[cfg(test)]
    pub fn ensure_live_screen_for_test(&mut self) {
        if !self.has_active_terminal() {
            return;
        }
        let terminal = self.active_terminal_mut();
        terminal.ingest.ensure_live_screen(&mut terminal.buffer);
    }

    #[cfg(test)]
    pub fn live_screen_text_for_test(&self) -> String {
        if !self.has_active_terminal() {
            return String::new();
        }
        self.active_terminal().ingest.live_screen_lines().join("\n")
    }

    /// Append lines like PTY ingest (`mark_dirty = false`) so inactive views keep
    /// their record cursors and catch up incrementally when selected.
    #[cfg(test)]
    pub fn push_streaming_lines_for_test(&mut self, lines: impl IntoIterator<Item = String>) {
        self.push_lines(lines, false);
    }

    #[cfg(test)]
    pub fn view_record_cursor_for_test(&self, index: usize) -> Option<usize> {
        if !self.has_active_terminal() {
            return None;
        }
        self.active_terminal()
            .views
            .get(index)
            .map(|v| v.flat_lines_record_cursor)
    }

    #[cfg(test)]
    pub fn view_flat_line_count_for_test(&self, index: usize) -> Option<usize> {
        if !self.has_active_terminal() {
            return None;
        }
        self.active_terminal()
            .views
            .get(index)
            .map(|v| v.flat_lines.len())
    }

    #[cfg(test)]
    pub fn first_flat_line_for_test(&self) -> Option<String> {
        if !self.has_active_terminal() {
            return None;
        }
        self.active_view().flat_lines.first().map(|l| l.raw.clone())
    }

    #[cfg(test)]
    pub fn file_load_pending_for_test(&self) -> bool {
        self.has_active_terminal() && self.active_terminal().file_load.is_some()
    }

    #[cfg(test)]
    pub fn file_loads_pending_any_for_test(&self) -> bool {
        self.terminals.iter().any(|t| t.file_load.is_some())
    }

    #[cfg(test)]
    pub fn file_backed_count_for_test(&self) -> usize {
        self.terminals
            .iter()
            .filter(|t| t.file_backed.is_some())
            .count()
    }

    #[cfg(test)]
    pub fn finish_all_file_loads_for_test(&mut self) {
        // Time-bounded so a regression (background loads never advancing)
        // fails the test instead of hanging the suite; the worker thread may
        // need real time to emit its first events under a loaded test run.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if !self.file_loads_pending_any_for_test() {
                return;
            }
            self.advance_file_load();
            if std::time::Instant::now() > deadline {
                panic!("file loads did not drain within 10s");
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[cfg(test)]
    pub fn buffer_record_count_for_test(&self) -> usize {
        if !self.has_active_terminal() {
            return 0;
        }
        self.active_terminal().buffer.records_len()
    }

    #[cfg(test)]
    pub fn finish_file_load_for_test(&mut self) {
        while self.file_load_pending_for_test() {
            self.advance_file_load();
        }
    }

    #[cfg(test)]
    pub fn events_len_for_test(&self) -> usize {
        self.events.len()
    }

    #[cfg(test)]
    pub fn enqueue_event_for_test(&mut self, json: String) {
        self.events.push_back(json);
    }

    #[cfg(test)]
    pub fn terminal_running_for_test(&self, terminal_id: &str) -> Option<bool> {
        self.terminals
            .iter()
            .find(|t| t.id == terminal_id)
            .map(|t| t.running)
    }

    #[cfg(test)]
    pub fn tab_configs_for_test(&self) -> Vec<TabConfig> {
        self.active_terminal()
            .views
            .iter()
            .map(|v| v.to_tab_config())
            .collect()
    }

    #[cfg(test)]
    pub fn active_tab_index_for_test(&self) -> usize {
        self.active_terminal().active_view
    }

    #[cfg(test)]
    pub fn selection_text_for_test(&self) -> Option<String> {
        self.selection_text()
    }

    #[cfg(test)]
    pub fn set_selection_for_test(&mut self, sel: crate::viewport_layout::TextSelection) {
        self.active_terminal_mut().selection = Some(sel);
    }

    #[cfg(test)]
    pub fn host_work_pending_for_test(&self) -> bool {
        self.host_work_pending()
    }

    #[cfg(test)]
    pub fn scroll_x_for_test(&self) -> f32 {
        self.active_terminal().scroll_x
    }

    #[cfg(test)]
    pub fn wrap_lines_for_test(&self) -> bool {
        self.active_view().wrap_lines
    }

    #[cfg(test)]
    pub fn viewport_font_size_for_test(&self) -> f32 {
        self.renderer.font_size()
    }

    #[cfg(test)]
    pub fn viewport_row_stride_for_test(&self) -> f32 {
        self.renderer.metrics().row_stride
    }

    #[cfg(test)]
    pub fn viewport_dirty_for_test(&self) -> bool {
        self.viewport_dirty
    }

    #[cfg(test)]
    pub fn pty_cols_for_test(&self) -> u16 {
        self.viewport_pty_size().cols
    }

    #[cfg(test)]
    pub fn auto_follow_for_test(&self) -> bool {
        self.active_view().auto_follow
    }

    #[cfg(test)]
    pub fn paints_live_vt_grid_for_test(&self) -> bool {
        self.paints_live_vt_grid()
    }

    #[cfg(test)]
    pub fn scroll_offset_y_for_test(&self) -> f32 {
        self.active_terminal().scroll_offset_y
    }

    #[cfg(test)]
    pub fn max_scroll_offset_for_test(&self) -> f32 {
        self.max_scroll_offset()
    }

    #[cfg(test)]
    pub fn stats_scroll_y_for_test(&self) -> f32 {
        self.stats_scroll_y()
    }

    #[cfg(test)]
    pub fn buffer_line_start_for_test(&self) -> u64 {
        self.active_terminal().buffer_line_start
    }

    #[cfg(test)]
    pub fn buffer_line_end_for_test(&self) -> u64 {
        self.active_terminal().buffer_line_end
    }

    #[cfg(test)]
    pub fn local_window_max_scroll_for_test(&self) -> f32 {
        self.local_window_max_scroll()
    }

    #[cfg(test)]
    pub fn viewport_line_position_for_test(&self) -> (u64, u64) {
        self.viewport_line_position()
    }

    #[cfg(test)]
    pub fn file_view_window_lines_for_test(&self) -> usize {
        self.file_view_window_lines()
    }

    #[cfg(test)]
    pub fn file_total_lines_for_test(&self) -> u64 {
        self.active_terminal()
            .file_backed
            .as_ref()
            .map(|b| b.index.total_lines())
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub fn can_restore_closed_tab_for_test(&self) -> bool {
        !self.active_terminal().closed_tabs.is_empty()
    }

    #[cfg(test)]
    pub fn terminals_for_test(&self) -> Vec<(String, String, bool)> {
        self.terminals
            .iter()
            .map(|t| (t.id.clone(), t.label(), t.running))
            .collect()
    }

    #[cfg(test)]
    pub fn file_session_ids_for_test(&self) -> Vec<String> {
        self.terminals
            .iter()
            .filter(|t| t.is_file_session())
            .map(|t| t.id.clone())
            .collect()
    }

    #[cfg(test)]
    pub fn file_session_paths_for_test(&self) -> Vec<String> {
        self.terminals
            .iter()
            .filter_map(|t| t.file_session_path().map(str::to_string))
            .collect()
    }

    #[cfg(test)]
    pub fn file_backed_for_test(&self) -> bool {
        self.has_active_terminal() && self.active_terminal().file_backed.is_some()
    }

    #[cfg(test)]
    pub fn match_scan_pos_for_test(&self) -> Option<u64> {
        self.active_view().match_scan_pos
    }

    #[cfg(test)]
    pub fn match_offsets_len_for_test(&self) -> usize {
        self.active_view().match_offsets.len()
    }

    #[cfg(test)]
    pub fn match_capped_for_test(&self) -> bool {
        self.active_view().match_capped
    }

    #[cfg(test)]
    pub fn set_match_scan_cap_for_test(&mut self, cap: usize) {
        self.match_scan_cap_override = Some(cap);
    }

    #[cfg(test)]
    pub fn overlay_len_for_test(&self) -> usize {
        self.active_view().overlay_len()
    }

    #[cfg(test)]
    pub fn flat_lines_len_for_test(&self) -> usize {
        self.active_view().flat_lines.len()
    }

    #[cfg(test)]
    pub fn flat_line_texts_for_test(&self) -> Vec<String> {
        self.active_view()
            .flat_lines
            .iter()
            .map(|l| l.raw.clone())
            .collect()
    }

    #[cfg(test)]
    pub fn uses_match_index_for_test(&self) -> bool {
        self.active_view().uses_match_index()
    }

    #[cfg(test)]
    pub fn finish_file_match_scan_for_test(&mut self) {
        // Match scans and their initial match-window reads run on worker
        // threads (issue #55): post, then pump the inbox until both land.
        for _ in 0..100_000 {
            if self.active_view().match_scan_pos.is_none()
                && self.active_view().match_window_inflight.is_none()
            {
                break;
            }
            self.advance_file_match_scan();
            self.apply_file_io_results();
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let _ = self.rebuild_if_needed();
    }

    #[cfg(test)]
    pub fn active_view_name_for_test(&self) -> String {
        self.active_view().name.clone()
    }

    #[cfg(test)]
    pub fn terminal_add_blank_for_test(&mut self) {
        self.terminal_add_blank();
    }

    #[cfg(test)]
    pub fn poll_pty_for_test(&mut self) {
        self.poll_pty();
    }

    #[cfg(test)]
    pub fn set_spawn_resolver_for_test(&mut self, resolver: crate::spawn_resolver::SpawnResolver) {
        self.spawn_resolver = resolver;
    }

    #[cfg(test)]
    pub fn spawn_pending_for_test(&self, terminal_id: &str) -> bool {
        self.terminals
            .iter()
            .find(|t| t.id == terminal_id)
            .is_some_and(|t| t.pending_spawn.is_some())
    }

    #[cfg(test)]
    pub fn pending_stdin_len_for_test(&self, terminal_id: &str) -> usize {
        self.terminals
            .iter()
            .find(|t| t.id == terminal_id)
            .map(|t| t.pending_stdin.len())
            .unwrap_or(0)
    }

    /// The `(command, args, workdir)` a shell spawn for the active terminal
    /// would resolve to — the exact [`crate::spawn_resolver::SpawnResolveKey`]
    /// inputs, for warming the cache in tests.
    #[cfg(test)]
    pub fn active_shell_argv_for_test(&self) -> Option<(String, Vec<String>, String)> {
        let term = self.active_terminal();
        if term.launch.command.is_some() {
            return None;
        }
        let mut launch = term.launch.clone();
        if launch.cwd.is_none() {
            launch.cwd = Some(term.cwd.clone());
        }
        let (command, args, cwd) =
            crate::spawn_resolve::resolve_interactive_shell(&launch, self.config.shell).ok()?;
        let workdir = crate::engine::terminal_lifecycle::expand_spawn_cwd(cwd);
        Some((command, args, workdir))
    }

    /// The `(command, args, workdir)` a saved-launch spawn for the active
    /// terminal would resolve to.
    #[cfg(test)]
    pub fn active_launch_argv_for_test(&self) -> Option<(String, Vec<String>, String)> {
        let term = self.active_terminal();
        let launch = term.launch.clone();
        let (command, args, cwd) = crate::spawn_resolve::resolve_process_launch(&launch).ok()?;
        let workdir = crate::engine::terminal_lifecycle::expand_spawn_cwd(cwd);
        Some((command, args, workdir))
    }

    #[cfg(test)]
    pub fn has_pty_for_test(&self, terminal_id: &str) -> bool {
        self.ptys.contains_key(terminal_id)
    }

    #[cfg(test)]
    pub fn set_pty_generation_for_test(&mut self, generation: u64) {
        if self.has_active_terminal() {
            self.active_terminal_mut().pty_generation = generation;
        }
    }

    #[cfg(test)]
    pub fn active_terminal_index_for_test(&self) -> usize {
        self.active_terminal
    }

    #[cfg(test)]
    pub fn active_terminal_id_for_test(&self) -> String {
        self.active_terminal().id.clone()
    }

    #[cfg(test)]
    pub fn presets_for_test(&self) -> &HashMap<String, PresetConfig> {
        &self.config.presets
    }

    #[cfg(test)]
    pub fn active_tab_config_for_test(&self) -> crate::core::types::TabConfig {
        self.active_view().to_tab_config()
    }

    #[cfg(test)]
    pub fn search_match_index_for_test(&self) -> usize {
        self.active_view().search_match_index
    }

    #[cfg(test)]
    pub fn search_match_count_for_test(&self) -> usize {
        self.active_view().search_matches.len()
    }

    #[cfg(test)]
    pub fn search_counter_for_test(&self) -> String {
        self.active_view().search_counter_label()
    }

    #[cfg(test)]
    pub fn active_view_severity_for_test(&self) -> String {
        self.active_view().severity_filter.as_str().to_string()
    }

    #[cfg(test)]
    pub fn active_flat_line_count_for_test(&self) -> usize {
        self.active_view().flat_lines.len()
    }

    #[cfg(test)]
    pub fn rebuild_active_for_test(&mut self) {
        self.rebuild_if_needed();
    }

    #[cfg(test)]
    pub fn push_lines_for_test(&mut self, lines: impl IntoIterator<Item = String>) {
        self.push_lines(lines, true);
        self.rebuild_if_needed();
    }
}
