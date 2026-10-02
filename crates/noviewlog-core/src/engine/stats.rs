use super::events::{StatsProject, StatsSnapshot, StatsTab, StatsTerminal};
use super::*;

impl Engine {
    pub(crate) fn emit_stats(&mut self) {
        let now = Instant::now();
        let send = self
            .last_stats_at
            .is_none_or(|t| t.elapsed() > Duration::from_millis(250));
        if !send {
            return;
        }
        self.last_stats_at = Some(now);
        self.ensure_valid_state();

        let format_ids: Vec<String> = self.formats.keys().cloned().collect();
        let preset_names: Vec<String> = self.config.presets.keys().cloned().collect();
        let mut terminals: Vec<StatsTerminal> = Vec::new();
        let mut files: Vec<StatsTerminal> = Vec::new();
        for (i, t) in self.terminals.iter().enumerate() {
            let row = StatsTerminal {
                index: i,
                id: t.id.clone(),
                label: t.label(),
                running: t.running,
                cwd: t.cwd.clone(),
                has_launch: t.launch.has_process_launch(),
                program_id: t.program_id.clone(),
                launch_command: t.launch.command.clone().unwrap_or_default(),
                launch_args: t.launch.args.join(" "),
                launch_cwd: t.launch.cwd.clone().unwrap_or_default(),
                launch_wsl: t.launch.wsl,
                launch_wsl_distro: t.launch.wsl_distro.clone().unwrap_or_default(),
            };
            if t.is_file_session() {
                files.push(row);
            } else {
                terminals.push(row);
            }
        }
        let projects: Vec<StatsProject> = self
            .projects
            .projects
            .iter()
            .enumerate()
            .map(|(i, p)| StatsProject {
                index: i,
                id: p.id.clone(),
                name: p.name.clone(),
                program_count: p.programs.len(),
            })
            .collect();
        let active_project_id = self
            .active_project
            .and_then(|i| self.projects.projects.get(i).map(|p| p.id.clone()));
        let active_terminal_idx = self.active_terminal;
        let is_file_session = self
            .terminals
            .get(active_terminal_idx)
            .is_some_and(|t| t.is_file_session());
        let (
            tabs,
            terminal_id,
            terminal_label,
            has_launch,
            lines,
            running,
            exit_code,
            active_tab,
            tab_count,
            dropped,
            can_restore,
            scroll_x,
            _local_scroll_y,
            has_selection,
            auto_follow,
            tab_name,
            filters,
            search_query,
            search_regex,
            search_case_sensitive,
            search_whole_word,
            search_counter,
            search_error,
            search_has_matches,
            wrap_lines,
            file_total_lines,
            file_index_progress,
            file_window_start,
            file_lines_before,
            file_loading,
            file_changed,
        ) = if let Some(terminal) = self.terminals.get(active_terminal_idx) {
            let view = terminal.active_view();
            let tabs: Vec<StatsTab> = terminal
                .views
                .iter()
                .enumerate()
                .map(|(i, v)| StatsTab {
                    index: i,
                    name: v.name.clone(),
                    is_terminal_tab: i == 0,
                })
                .collect();
            (
                tabs,
                terminal.id.clone(),
                terminal.label(),
                terminal.launch.has_process_launch(),
                if !terminal.is_file_session()
                    && terminal.running
                    && terminal.active_view == 0
                    && view.auto_follow
                    && view.search_query.is_empty()
                {
                    // Same monotonic total as viewport_line_position (not ring-capped).
                    terminal.buffer.dropped_count()
                        + terminal.buffer.records_len()
                        + terminal.ingest.size().1
                } else {
                    view.flat_lines.len()
                },
                terminal.running,
                terminal.exit_code,
                terminal.active_view,
                terminal.views.len(),
                terminal.buffer.dropped_count(),
                !terminal.closed_tabs.is_empty(),
                terminal.scroll_x,
                terminal.scroll_offset_y,
                terminal.selection.is_some_and(|s| !s.is_empty()),
                if terminal.is_file_session() {
                    false
                } else {
                    view.auto_follow
                },
                view.name.clone(),
                view.filters().to_vec(),
                view.search_query.clone(),
                view.search_regex,
                view.search_case_sensitive,
                view.search_whole_word,
                view.search_counter_label(),
                view.search_error.clone(),
                !view.search_matches.is_empty(),
                view.wrap_lines,
                terminal
                    .file_backed
                    .as_ref()
                    .map(|b| b.index.total_lines())
                    .unwrap_or(0),
                terminal
                    .file_load
                    .as_ref()
                    .map(|l| l.index_progress)
                    .unwrap_or(if terminal.file_backed.is_some() {
                        1.0
                    } else {
                        0.0
                    }),
                terminal.buffer_line_start,
                terminal.buffer_line_start,
                terminal.file_load.is_some(),
                terminal.file_changed,
            )
        } else {
            (
                vec![],
                String::new(),
                String::new(),
                false,
                0usize,
                false,
                None,
                0usize,
                0usize,
                0usize,
                false,
                0.0f32,
                0.0f32,
                false,
                true,
                String::new(),
                vec![],
                String::new(),
                false,
                false,
                false,
                String::new(),
                None,
                false,
                true,
                0u64,
                0.0f32,
                0u64,
                0u64,
                false,
                false,
            )
        };
        // FILES scrollbar uses whole-file coordinates.
        let scroll_y = self.stats_scroll_y();
        let (viewport_line, viewport_line_total) = self.viewport_line_position();
        let max_scroll_x = if self.has_active_terminal() {
            self.current_max_scroll_x()
        } else {
            0.0
        };
        let max_scroll_y = if self.has_active_terminal() {
            self.max_scroll_offset()
        } else {
            0.0
        };
        let severity_filter = if let Some(terminal) = self.terminals.get(active_terminal_idx) {
            terminal.active_view().severity_filter.as_str().to_string()
        } else {
            "all".to_string()
        };
        let match_capped = if let Some(terminal) = self.terminals.get(active_terminal_idx) {
            terminal.active_view().match_capped
        } else {
            false
        };

        let snapshot = StatsSnapshot {
            event_type: "stats".to_string(),
            lines,
            running,
            status: self.status_message.clone(),
            exit_code,
            format_id: self.format_id.clone(),
            preset_name: self.preset_name.clone(),
            auto_follow,
            tab_name,
            active_tab,
            tab_count,
            terminal_tab: 0,
            is_terminal_tab: active_tab == 0,
            tabs,
            dropped,
            formats: format_ids,
            presets: preset_names,
            filters,
            search_query,
            search_regex,
            search_case_sensitive,
            search_whole_word,
            search_counter,
            search_error,
            search_has_matches,
            can_restore_closed_tab: can_restore,
            wrap_lines,
            scroll_x,
            max_scroll_x,
            scroll_y,
            max_scroll_y,
            has_selection,
            terminals,
            files,
            projects,
            active_project_id,
            active_terminal: active_terminal_idx,
            terminal_id,
            terminal_label,
            has_launch,
            has_active_terminal: self.has_active_terminal(),
            is_file_session,
            terminals_section_expanded: self.config.terminals_section_expanded,
            files_section_expanded: self.config.files_section_expanded,
            file_total_lines,
            file_index_progress,
            file_window_start,
            file_lines_before,
            file_loading,
            match_capped,
            file_changed,
            viewport_line,
            viewport_line_total,
            max_scrollback_lines: self.config.max_scrollback_lines,
            viewport_font_size: self.renderer.font_size(),
            severity_filter,
        };
        match serde_json::to_value(&snapshot) {
            Ok(value) => self.push_event(value),
            Err(err) => self.push_event(json!({
                "type": "status",
                "message": format!("stats serialize: {err}"),
            })),
        }
    }
}
