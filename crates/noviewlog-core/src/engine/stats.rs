use super::events::{StatsProject, StatsSnapshot, StatsTab, StatsTerminal};
use super::*;
use crate::core::types::FilterRule;

/// Per-active-tab fields for [`StatsSnapshot`], filled by [`Engine::active_tab_stats`].
///
/// Named fields replace the former 31-element positional tuple so a transposition
/// of same-typed neighbors cannot compile into a silent wrong stat.
#[derive(Debug, Clone)]
struct ActiveTabStats {
    tabs: Vec<StatsTab>,
    terminal_id: String,
    terminal_label: String,
    has_launch: bool,
    lines: usize,
    running: bool,
    exit_code: Option<i32>,
    active_tab: usize,
    tab_count: usize,
    dropped: usize,
    can_restore: bool,
    scroll_x: f32,
    has_selection: bool,
    auto_follow: bool,
    tab_name: String,
    filters: Vec<FilterRule>,
    search_query: String,
    search_regex: bool,
    search_case_sensitive: bool,
    search_whole_word: bool,
    search_counter: String,
    search_error: Option<String>,
    search_has_matches: bool,
    wrap_lines: bool,
    file_total_lines: u64,
    file_index_progress: f32,
    file_window_start: u64,
    file_lines_before: u64,
    file_loading: bool,
    file_changed: bool,
}

impl Default for ActiveTabStats {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            terminal_id: String::new(),
            terminal_label: String::new(),
            has_launch: false,
            lines: 0,
            running: false,
            exit_code: None,
            active_tab: 0,
            tab_count: 0,
            dropped: 0,
            can_restore: false,
            scroll_x: 0.0,
            has_selection: false,
            // Match the empty-engine wire defaults on StatsSnapshot.
            auto_follow: true,
            tab_name: String::new(),
            filters: Vec::new(),
            search_query: String::new(),
            search_regex: false,
            search_case_sensitive: false,
            search_whole_word: false,
            search_counter: String::new(),
            search_error: None,
            search_has_matches: false,
            wrap_lines: true,
            file_total_lines: 0,
            file_index_progress: 0.0,
            file_window_start: 0,
            file_lines_before: 0,
            file_loading: false,
            file_changed: false,
        }
    }
}

impl Engine {
    /// Active-terminal / active-tab stats slice, or [`ActiveTabStats::default`]
    /// when there is no terminal at `active_terminal`.
    fn active_tab_stats(&self) -> ActiveTabStats {
        let Some(terminal) = self.terminals.get(self.active_terminal) else {
            return ActiveTabStats::default();
        };
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
        let lines = if !terminal.is_file_session()
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
        };
        let file_index_progress = terminal
            .file_load
            .as_ref()
            .map(|l| l.index_progress)
            .unwrap_or(if terminal.file_backed.is_some() {
                1.0
            } else {
                0.0
            });
        // Wire aliases of buffer_line_start — keep both for serde stability (#338).
        let buffer_start = terminal.buffer_line_start;
        ActiveTabStats {
            tabs,
            terminal_id: terminal.id.clone(),
            terminal_label: terminal.label(),
            has_launch: terminal.launch.has_process_launch(),
            lines,
            running: terminal.running,
            exit_code: terminal.exit_code,
            active_tab: terminal.active_view,
            tab_count: terminal.views.len(),
            dropped: terminal.buffer.dropped_count(),
            can_restore: !terminal.closed_tabs.is_empty(),
            scroll_x: terminal.scroll_x,
            has_selection: terminal.selection.is_some_and(|s| !s.is_empty()),
            auto_follow: if terminal.is_file_session() {
                false
            } else {
                view.auto_follow
            },
            tab_name: view.name.clone(),
            filters: view.filters().to_vec(),
            search_query: view.search_query.clone(),
            search_regex: view.search_regex,
            search_case_sensitive: view.search_case_sensitive,
            search_whole_word: view.search_whole_word,
            search_counter: view.search_counter_label(),
            search_error: view.search_error.clone(),
            search_has_matches: !view.search_matches.is_empty(),
            wrap_lines: view.wrap_lines,
            file_total_lines: terminal
                .file_backed
                .as_ref()
                .map(|b| b.index.total_lines())
                .unwrap_or(0),
            file_index_progress,
            file_window_start: buffer_start,
            file_lines_before: buffer_start,
            file_loading: terminal.file_load.is_some(),
            file_changed: terminal.file_changed,
        }
    }

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
        let tab = self.active_tab_stats();
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
            lines: tab.lines,
            running: tab.running,
            status: self.status_message.clone(),
            exit_code: tab.exit_code,
            format_id: self.format_id.clone(),
            preset_name: self.preset_name.clone(),
            auto_follow: tab.auto_follow,
            tab_name: tab.tab_name,
            active_tab: tab.active_tab,
            tab_count: tab.tab_count,
            terminal_tab: 0,
            is_terminal_tab: tab.active_tab == 0,
            tabs: tab.tabs,
            dropped: tab.dropped,
            formats: format_ids,
            presets: preset_names,
            filters: tab.filters,
            search_query: tab.search_query,
            search_regex: tab.search_regex,
            search_case_sensitive: tab.search_case_sensitive,
            search_whole_word: tab.search_whole_word,
            search_counter: tab.search_counter,
            search_error: tab.search_error,
            search_has_matches: tab.search_has_matches,
            can_restore_closed_tab: tab.can_restore,
            wrap_lines: tab.wrap_lines,
            scroll_x: tab.scroll_x,
            max_scroll_x,
            scroll_y,
            max_scroll_y,
            has_selection: tab.has_selection,
            terminals,
            files,
            projects,
            active_project_id,
            active_terminal: active_terminal_idx,
            terminal_id: tab.terminal_id,
            terminal_label: tab.terminal_label,
            has_launch: tab.has_launch,
            has_active_terminal: self.has_active_terminal(),
            is_file_session,
            terminals_section_expanded: self.config.terminals_section_expanded,
            files_section_expanded: self.config.files_section_expanded,
            file_total_lines: tab.file_total_lines,
            file_index_progress: tab.file_index_progress,
            file_window_start: tab.file_window_start,
            file_lines_before: tab.file_lines_before,
            file_loading: tab.file_loading,
            match_capped,
            file_changed: tab.file_changed,
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
