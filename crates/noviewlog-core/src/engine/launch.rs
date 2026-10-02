//! Launch lifecycle: apply a launch config, restart the session, switch format.

use super::*;

impl Engine {
    pub fn set_launch(&mut self, launch: LaunchConfig) {
        if let Some(path) = &launch.config_path {
            match std::fs::read_to_string(path) {
                Ok(text) => {
                    self.config = load_config_from_yaml(&text);
                    self.formats = merge_formats(
                        &crate::core::config::all_format_presets(&self.config),
                        &HashMap::new(),
                    );
                    // The in-memory config now comes from the launch file,
                    // NOT the user's config.yaml (issue #110): disable
                    // persistence so a later debounced flush cannot overwrite
                    // the user's file.
                    self.config_persist_disabled = true;
                }
                // A file that is simply absent is not a launch config —
                // keep persistence as-is.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                // Any other read failure (permissions, transient IO, ...)
                // must also disable persistence: the in-memory config may be
                // stale, and flushing it could overwrite the user's YAML
                // (issue #255).
                Err(_) => self.config_persist_disabled = true,
            }
        }
        if let Some(preset) = launch.preset.clone() {
            self.preset_apply(&preset);
        }

        self.auto_start_launch = launch.has_process_launch();
        self.ensure_valid_state();
        let default_format = self.current_format();
        let term_size = self.viewport_pty_size();
        let id = self.active_terminal().id.clone();

        // Update active terminal's launch and reset its session state.
        {
            let terminal = self.active_terminal_mut();
            if let Some(cwd) = launch.cwd.clone().filter(|s| !s.is_empty()) {
                terminal.cwd = cwd;
            }
            terminal.launch = launch;
            terminal.process_started = false;
            terminal.running = false;
            terminal.exit_code = None;
            terminal.pending_spawn = None;
            terminal.pending_stdin.clear();
            terminal.buffer.clear();
            terminal
                .ingest
                .reset_with_size(term_size.cols as usize, term_size.rows as usize);
            terminal.reset_viewport();
            for view in &mut terminal.views {
                view.clear_flat_lines();
            }
            if terminal.views.is_empty() {
                let name = terminal.primary_tab_name();
                terminal.views = vec![LogView::from_runtime(&name, Vec::new())];
                terminal.active_view = 0;
            }
            terminal.sync_primary_tab_identity();
            if terminal.is_file_session() {
                terminal.disable_follow_all_views();
            }
            terminal.parser = RecordParser::new(default_format);
        }

        if let Some(mut pty) = self.ptys.remove(&id) {
            pty.stop();
        }

        let log_file = self.active_terminal().launch.log_file.clone();
        let has_command = self.active_terminal().launch.command.is_some();
        if let Some(path) = log_file {
            self.configure_active_as_file_session(&path);
            self.active_terminal_mut().process_started = true;
            self.start_log_file_load(&path);
        } else if has_command {
            self.active_terminal_mut().process_started = true;
            self.start_launch_process();
        } else {
            #[cfg(not(test))]
            {
                self.start_interactive_shell();
            }
        }
    }

    pub(crate) fn restart(&mut self) {
        let (has_command, log_file) = {
            let launch = &self.active_terminal().launch;
            (launch.command.is_some(), launch.log_file.clone())
        };
        let format = self.current_format();
        let term = self.viewport_pty_size();
        {
            let terminal = self.active_terminal_mut();
            terminal.buffer.clear();
            terminal.file_backed = None;
            terminal.pending_file_window = None;
            terminal.buffer_line_start = 0;
            terminal.buffer_line_end = 0;
            terminal.parser = RecordParser::new(format);
            terminal
                .ingest
                .reset_with_size(term.cols as usize, term.rows as usize);
            for view in &mut terminal.views {
                view.clear_flat_lines();
            }
            terminal.scroll_offset_y = 0.0;
            // Type-ahead typed before the restart must not flush into the
            // fresh process (same semantics as `set_launch`).
            terminal.pending_stdin.clear();
        }
        let id = self.active_terminal().id.clone();
        if let Some(mut pty) = self.ptys.remove(&id) {
            pty.stop();
        }
        if has_command {
            self.start_launch_process();
        } else if let Some(path) = log_file {
            self.active_terminal_mut().running = false;
            self.start_log_file_load(&path);
        } else {
            // Interactive shell: clear log and spawn a fresh shell.
            self.start_interactive_shell();
        }
    }

    pub(crate) fn set_format(&mut self, id: &str) {
        if !self.formats.contains_key(id) || !self.has_active_terminal() {
            return;
        }
        if self.format_id == id {
            return;
        }
        self.format_id = id.to_string();
        let format = self.current_format();
        {
            let terminal = self.active_terminal_mut();
            if let Some(rec) = terminal.parser.flush_pending() {
                terminal.buffer.add(rec);
            }
            let lines = terminal.buffer.raw_lines();
            let records = reparse_lines(&lines, format.clone());
            terminal.buffer.replace_all(records);
            terminal.parser = RecordParser::new(format);
            terminal.selection = None;
        }
        self.mark_all_views_dirty();
        self.rebuild_if_needed();
        self.mark_viewport_dirty();
        self.last_stats_at = None;
        self.status_message = format!("Format: {id}");
        self.push_event(json!({"type":"status","message": self.status_message}));
    }
}
