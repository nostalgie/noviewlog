//! Project / Program persistence and restore.

use super::*;
use crate::core::config::{program_workspace_snapshot, workspace_to_tab_configs};
use crate::core::types::{
    next_program_id, next_project_id, program_display_name, ProgramConfig, ProjectConfig,
    WorkspaceConfig,
};
use crate::log_view::TERMINAL_TAB_NAME;

impl Engine {
    /// Complete host startup: CLI launch, last-project restore, or a stopped boot terminal.
    ///
    /// CLI process / file launch is an explicit argument and MAY auto-start that
    /// one-shot session. Restoring the last Project MUST NOT start Programs.
    ///
    /// Returns `true` when the last active project was restored.
    pub fn finish_startup(&mut self, launch: LaunchConfig) -> bool {
        if launch.has_process_launch() {
            self.set_launch(launch);
            return false;
        }
        if !self.projects.projects.is_empty() {
            let idx = self
                .projects
                .active_project
                .min(self.projects.projects.len() - 1);
            let project_id = self.projects.projects[idx].id.clone();
            self.project_open(&project_id);
            return true;
        }
        self.set_launch(launch);
        false
    }

    /// Mark `projects.yaml` dirty; the write lands after the persistence
    /// debounce (or at app exit) — no per-gesture fsync (issue #62).
    pub(crate) fn persist_projects_store(&mut self) {
        if let Some(idx) = self.active_project {
            self.projects.active_project = idx;
        }
        #[cfg(test)]
        if self.skip_projects_persist {
            return;
        }
        self.projects_dirty = true;
        self.persist_changed_at = Some(Instant::now());
        // Fresh dirty mark re-arms the normal debounce (issue #253): the
        // failure backoff applies only to retries of the same change.
        self.persist_retry_delay = PERSIST_DEBOUNCE;
    }

    /// Snapshot live TERMINALS then FILES into the active Project's Programs.
    pub(crate) fn sync_active_project_from_terminals(&mut self) {
        let Some(proj_idx) = self.active_project else {
            return;
        };
        if proj_idx >= self.projects.projects.len() {
            self.active_project = None;
            return;
        }

        let mut programs: Vec<ProgramConfig> = Vec::new();
        for term in self.terminals.iter().filter(|t| !t.is_file_session()) {
            programs.push(program_from_terminal(term, &programs));
        }
        for term in self.terminals.iter().filter(|t| t.is_file_session()) {
            programs.push(program_from_terminal(term, &programs));
        }

        // Keep program_id links in sync with newly assigned ids (live then files).
        let mut i = 0usize;
        for term in self.terminals.iter_mut().filter(|t| !t.is_file_session()) {
            if let Some(p) = programs.get(i) {
                term.program_id = Some(p.id.clone());
            }
            i += 1;
        }
        for term in self.terminals.iter_mut().filter(|t| t.is_file_session()) {
            if let Some(p) = programs.get(i) {
                term.program_id = Some(p.id.clone());
            }
            i += 1;
        }

        self.projects.projects[proj_idx].programs = programs;
        self.persist_projects_store();
    }

    pub(crate) fn project_open(&mut self, project_id: &str) {
        let Some(proj_idx) = self
            .projects
            .projects
            .iter()
            .position(|p| p.id == project_id)
        else {
            self.status_message = format!("Unknown project: {project_id}");
            self.push_event(json!({"type":"status","message": self.status_message}));
            return;
        };

        // Stop all PTYs; FILES are replaced from this Project (not leftover sessions).
        let all_ids: Vec<String> = self.terminals.iter().map(|t| t.id.clone()).collect();
        for id in &all_ids {
            if let Some(mut pty) = self.ptys.remove(id) {
                pty.stop();
            }
        }
        // Dropping the old terminals must cancel any in-flight whole-file
        // match scans, same as tab close (#237) and terminal close (#255).
        for term in &mut self.terminals {
            super::tabs::cancel_terminal_match_scans(term);
        }
        self.terminals.clear();

        let runtime = build_runtime_config(&self.config, Some(&self.preset_name));
        let format = self.current_format();
        let max_records = self.config.max_scrollback_lines;
        let programs = self.projects.projects[proj_idx].programs.clone();
        let live_programs: Vec<&ProgramConfig> = programs
            .iter()
            .filter(|p| p.launch.log_file.is_none())
            .collect();
        let file_programs: Vec<&ProgramConfig> = programs
            .iter()
            .filter(|p| p.launch.log_file.is_some())
            .collect();

        let mut new_sessions: Vec<TerminalState> = Vec::new();
        if live_programs.is_empty() {
            let id = next_terminal_id(&new_sessions);
            let mut term =
                TerminalState::new(id, LaunchConfig::default(), &runtime, &format, max_records);
            term.running = false;
            new_sessions.push(term);
        } else {
            for (i, program) in live_programs.iter().enumerate() {
                let id = format!("terminal-{}-{i}", crate::core::types::unique_time_suffix());
                let mut term =
                    TerminalState::new(id, program.launch.clone(), &runtime, &format, max_records);
                apply_program_to_terminal(&mut term, program);
                new_sessions.push(term);
            }
        }

        for (i, program) in file_programs.iter().enumerate() {
            let id = format!(
                "terminal-file-{}-{i}",
                crate::core::types::unique_time_suffix()
            );
            let mut term =
                TerminalState::new(id, program.launch.clone(), &runtime, &format, max_records);
            apply_program_to_terminal(&mut term, program);
            configure_restored_file_session(&mut term);
            new_sessions.push(term);
        }

        self.terminals = new_sessions;
        // Prefer the Project's active_program among live sessions.
        let active_prog = self.projects.projects[proj_idx].active_program;
        if let Some(pos) = self
            .terminals
            .iter()
            .enumerate()
            .filter(|(_, t)| !t.is_file_session())
            .nth(active_prog)
            .map(|(i, _)| i)
        {
            self.active_terminal = pos;
        } else {
            self.active_terminal = 0;
        }
        self.active_project = Some(proj_idx);
        self.projects.active_project = proj_idx;
        self.persist_projects_store();
        // CLI `auto_start_launch` must not fire on restored Programs after open.
        self.auto_start_launch = false;
        self.begin_restored_file_loads();
        self.mark_all_views_dirty();
        self.mark_viewport_dirty();
        self.last_stats_at = None;
        self.status_message = format!("Opened project: {}", self.projects.projects[proj_idx].name);
        self.push_event(json!({"type":"status","message": self.status_message}));
    }

    /// Begin FILES loads for restored log-file Programs. Live Programs stay
    /// stopped until the user presses Start (or types into a blank Terminal).
    fn begin_restored_file_loads(&mut self) {
        let file_ids: Vec<String> = self
            .terminals
            .iter()
            .filter(|t| t.is_file_session())
            .map(|t| t.id.clone())
            .collect();
        if file_ids.is_empty() {
            return;
        }
        let resume_id = self
            .terminals
            .get(self.active_terminal)
            .map(|t| t.id.clone());
        for id in file_ids {
            self.terminal_switch(&id);
            if let Some(path) = self.active_terminal().launch.log_file.clone() {
                self.active_terminal_mut().process_started = true;
                self.start_log_file_load(&path);
            }
        }
        if let Some(id) = resume_id {
            self.terminal_switch(&id);
        }
    }

    pub(crate) fn project_create(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            self.status_message = "Project name required".to_string();
            self.push_event(json!({"type":"status","message": self.status_message}));
            return;
        }

        let project_id = next_project_id(&self.projects.projects);
        self.projects.projects.push(ProjectConfig {
            id: project_id.clone(),
            name: name.to_string(),
            default_cwd: None,
            path_hint: None,
            programs: Vec::new(),
            active_program: 0,
        });
        // Open immediately so live TERMINALS match the empty store. Leaving the new
        // Project active without opening would let sync copy the previous terminals.
        self.project_open(&project_id);
        self.status_message = format!("Created project: {name}");
        self.push_event(json!({"type":"status","message": self.status_message}));
    }

    pub(crate) fn project_rename(&mut self, project_id: &str, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let Some(proj) = self
            .projects
            .projects
            .iter_mut()
            .find(|p| p.id == project_id)
        else {
            return;
        };
        proj.name = name.to_string();
        self.persist_projects_store();
        self.last_stats_at = None;
    }

    pub(crate) fn project_delete(&mut self, project_id: &str) {
        let Some(idx) = self
            .projects
            .projects
            .iter()
            .position(|p| p.id == project_id)
        else {
            return;
        };
        self.projects.projects.remove(idx);
        if let Some(active) = self.active_project {
            if active == idx {
                self.active_project = None;
                for term in self.terminals.iter_mut() {
                    term.program_id = None;
                }
            } else if active > idx {
                self.active_project = Some(active - 1);
            }
        }
        if self.projects.active_project >= self.projects.projects.len()
            && !self.projects.projects.is_empty()
        {
            self.projects.active_project = self.projects.projects.len() - 1;
        }
        self.persist_projects_store();
        self.last_stats_at = None;
        self.status_message = "Project deleted".to_string();
        self.push_event(json!({"type":"status","message": self.status_message}));
    }

    pub(crate) fn program_set_launch(
        &mut self,
        terminal_id: Option<&str>,
        command: Option<String>,
        args: Vec<String>,
        cwd: Option<String>,
        wsl: bool,
        wsl_distro: Option<String>,
    ) {
        let id = terminal_id
            .map(|s| s.to_string())
            .unwrap_or_else(|| self.active_terminal().id.clone());
        let Some(term) = self.terminals.iter_mut().find(|t| t.id == id) else {
            return;
        };
        if term.is_file_session() {
            self.status_message = "File terminal is view-only".to_string();
            self.push_event(json!({"type":"status","message": self.status_message}));
            return;
        }
        let cmd = command
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty());
        term.launch.command = cmd;
        term.launch.args = args;
        term.launch.cwd = cwd.filter(|c| !c.trim().is_empty());
        term.launch.wsl = wsl;
        term.launch.wsl_distro = if wsl {
            wsl_distro
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        } else {
            None
        };
        term.launch.log_file = None;
        if let Some(ref cwd) = term.launch.cwd {
            term.cwd = cwd.clone();
        }
        self.last_stats_at = None;
        self.sync_active_project_from_terminals();
    }
}

fn program_from_terminal(term: &TerminalState, programs: &[ProgramConfig]) -> ProgramConfig {
    let tabs: Vec<_> = term.views.iter().map(|v| v.to_tab_config()).collect();
    let workspace = program_workspace_snapshot(&tabs, term.active_view);
    let id = term
        .program_id
        .clone()
        .unwrap_or_else(|| next_program_id(programs));
    let name = term
        .custom_title
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            if term.launch.has_process_launch() || term.is_file_session() {
                program_display_name(&term.launch)
            } else {
                term.label()
            }
        });
    ProgramConfig {
        id,
        name,
        launch: term.launch.clone(),
        workspace,
    }
}

fn apply_program_to_terminal(term: &mut TerminalState, program: &ProgramConfig) {
    term.program_id = Some(program.id.clone());
    if !program.name.trim().is_empty() {
        term.custom_title = Some(program.name.clone());
    }
    apply_workspace_tabs(term, &program.workspace);
    term.running = false;
    term.process_started = false;
    term.exit_code = None;
    // Live TERMINALS always open on the Terminal tab after restore. Saved
    // `active_tab` may be a filter tab; cold-open there showed an empty hint
    // with no Start on the tab itself. FILES keep their restored filter tab.
    if program.launch.log_file.is_none() {
        term.active_view = 0;
    }
}

fn configure_restored_file_session(term: &mut TerminalState) {
    if let Some(path) = term.launch.log_file.as_deref() {
        if let Some(parent) = std::path::Path::new(path).parent() {
            let cwd = parent.to_string_lossy().into_owned();
            if !cwd.is_empty() {
                term.cwd = cwd;
            }
        }
    }
    term.sync_primary_tab_identity();
    term.disable_follow_all_views();
}

fn apply_workspace_tabs(terminal: &mut TerminalState, workspace: &WorkspaceConfig) {
    let tabs = workspace_to_tab_configs(workspace);
    if tabs.is_empty() {
        terminal.views = vec![LogView::from_runtime(TERMINAL_TAB_NAME, Vec::new())];
        terminal.active_view = 0;
    } else {
        let mut views: Vec<LogView> = tabs.into_iter().map(LogView::from_tab_config).collect();
        if let Some(first) = views.first_mut() {
            if first.name.trim().is_empty() {
                first.name = TERMINAL_TAB_NAME.to_string();
            }
        }
        let active = workspace.active_tab.min(views.len().saturating_sub(1));
        terminal.views = views;
        terminal.active_view = active;
    }
    terminal.sync_primary_tab_identity();
}
