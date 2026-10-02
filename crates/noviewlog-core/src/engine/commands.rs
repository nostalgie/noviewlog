use super::*;
use crate::core::types::default_true;

/// Host → engine commands (JSON `cmd` tag, snake_case).
///
/// **Vocabulary:** `Tab*` variants operate on [`crate::log_view::LogView`]
/// entries inside the active terminal (UI/JSON name “tab”, Rust type `LogView`).
///
/// **Aliases (kept for wire compatibility):**
/// - [`Command::LoadPreset`] ≡ [`Command::PresetApply`]
/// - [`Command::SaveConfig`] creates a preset from the active tab
///   (`preset_create_from_tab`); it does **not** write user `config.yaml`
#[derive(Debug, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Start {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
    },
    Stop {
        #[serde(default)]
        terminal_id: Option<String>,
    },
    Stdin {
        #[serde(default)]
        text: String,
        #[serde(default)]
        bytes: Option<Vec<u8>>,
    },
    FilterAdd {
        #[serde(rename = "type")]
        filter_type: FilterType,
        pattern: String,
        /// Regex mode for the new rule; omitted → true (legacy).
        #[serde(default = "default_true")]
        regex: bool,
    },
    FilterClear,
    FilterToggle {
        id: String,
        enabled: bool,
    },
    FilterRemove {
        id: String,
    },
    FilterUpdate {
        id: String,
        pattern: String,
    },
    FilterSet {
        filters: Vec<FilterRule>,
    },
    Scroll {
        offset: f32,
    },
    ScrollLines {
        delta: i32,
    },
    ScrollPage {
        direction: i32,
    },
    ScrollTo {
        pos: String,
    },
    Resize {
        width: u32,
        height: u32,
    },
    LoadFile {
        path: String,
    },
    ReloadFile {
        #[serde(default)]
        terminal_id: Option<String>,
    },
    Restart,
    SetFormat {
        format_id: String,
    },
    SetFollow {
        follow: bool,
    },
    /// Alias of [`Command::PresetApply`] (legacy wire name).
    LoadPreset {
        name: String,
    },
    /// Misnamed legacy alias: creates a preset from the active tab, not a config save.
    SaveConfig,
    PresetGet {
        name: String,
    },
    PresetSave {
        name: String,
        #[serde(default)]
        filters: Vec<FilterRule>,
    },
    PresetDelete {
        name: String,
    },
    /// Canonical preset-apply command (prefer over [`Command::LoadPreset`]).
    PresetApply {
        name: String,
    },
    PresetCreateFromTab {
        name: String,
    },
    /// Add a filter tab (`LogView`) on the active terminal.
    TabAdd,
    TabClose {
        index: usize,
    },
    TabSwitch {
        index: usize,
    },
    TabRename {
        index: usize,
        name: String,
    },
    /// Reorder filter tabs on the active terminal. The Terminal tab (index 0) is pinned.
    TabMove {
        from_index: usize,
        to_index: usize,
    },
    TabRestore,
    SearchSet {
        query: String,
        regex: bool,
        #[serde(default)]
        case_sensitive: bool,
        #[serde(default)]
        whole_word: bool,
    },
    /// Live FILTERS draft preview (highlight only; does not change visibility).
    FilterDraftSet {
        pattern: String,
        #[serde(default)]
        use_regex: bool,
    },
    SearchGoto {
        delta: i32,
    },
    SetWrapLines {
        wrap: bool,
    },
    ScrollHorizontal {
        delta: f32,
    },
    SetScrollX {
        offset: f32,
    },
    SelectionAt {
        x: f32,
        y: f32,
        extend: bool,
        /// 1 = caret, 2 = word, 3+ = whole LogRecord span.
        #[serde(default = "default_click_count")]
        click_count: u32,
    },
    /// Open the OSC 8 hyperlink under the viewport point (no-op when none).
    OpenLinkAt {
        x: f32,
        y: f32,
    },
    SelectionClear,
    TerminalAdd,
    TerminalClose {
        #[serde(default)]
        terminal_id: Option<String>,
    },
    TerminalSwitch {
        terminal_id: String,
    },
    TerminalMove {
        terminal_id: String,
        to_index: usize,
    },
    TerminalRename {
        terminal_id: String,
        name: String,
    },
    TerminalStart {
        #[serde(default)]
        terminal_id: Option<String>,
    },
    ProjectOpen {
        project_id: String,
    },
    ProjectCreate {
        name: String,
    },
    ProjectRename {
        project_id: String,
        name: String,
    },
    ProjectDelete {
        project_id: String,
    },
    ProgramSetLaunch {
        #[serde(default)]
        terminal_id: Option<String>,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
        #[serde(default)]
        wsl: bool,
        #[serde(default)]
        wsl_distro: Option<String>,
    },
    SetSettings {
        max_scrollback_lines: usize,
    },
    /// Persist collapsible sidebar section expand state.
    SetSidebarExpanded {
        terminals: bool,
        files: bool,
    },
    SetViewportFontSize {
        size: f32,
    },
    /// Whether the host viewport control has keyboard focus (blinking caret gate).
    SetViewportFocus {
        focused: bool,
    },
    /// Set severity reading mode on the active Tab/View (orthogonal to include/exclude).
    SeveritySet {
        mode: String,
    },
    /// Toggle collapse for one multiline Record in the active Tab/View.
    RecordCollapseToggle {
        record_id: u64,
    },
    /// Expand all currently filtered multiline Records in the active Tab/View.
    RecordsExpandAll,
    /// Collapse all multiline Records in the active Tab/View.
    RecordsCollapseAll,
}

fn default_click_count() -> u32 {
    1
}

impl Engine {
    pub fn apply_command(&mut self, cmd: Command) -> Result<(), String> {
        match cmd {
            Command::Start { command, args, cwd } => {
                if self.active_terminal().is_file_session() {
                    self.status_message = "File terminal is view-only".to_string();
                    self.push_event(json!({"type":"status","message": self.status_message}));
                } else {
                    {
                        let terminal = self.active_terminal_mut();
                        terminal.launch.command = Some(command);
                        terminal.launch.args = args;
                        terminal.launch.cwd = cwd;
                        terminal.launch.wsl = false;
                        terminal.launch.wsl_distro = None;
                        terminal.launch.log_file = None;
                        terminal.process_started = true;
                    }
                    self.start_launch_process();
                }
            }
            Command::Stop { terminal_id } => self.stop(terminal_id.as_deref()),
            Command::Stdin { text, bytes } => {
                if let Some(data) = bytes {
                    self.handle_key(&data);
                } else {
                    let mut data = text.into_bytes();
                    if !data.ends_with(b"\n") {
                        data.push(b'\n');
                    }
                    self.handle_key(&data);
                }
            }
            Command::FilterAdd {
                filter_type,
                pattern,
                regex,
            } => self.add_filter(filter_type, &pattern, regex),
            Command::FilterClear => {
                if self.active_terminal().active_view == 0 {
                    // The Terminal tab has no filters.
                } else {
                    self.active_view_mut().clear_filters();
                    self.reset_file_match_viewport();
                    self.sync_active_project_from_terminals();
                }
            }
            Command::FilterToggle { id, enabled } => self.filter_toggle(&id, enabled),
            Command::FilterRemove { id } => self.filter_remove(&id),
            Command::FilterUpdate { id, pattern } => self.filter_update(&id, &pattern),
            Command::FilterSet { filters } => {
                if self.active_terminal().active_view == 0 {
                    // The Terminal tab has no filters.
                } else {
                    let (compiled, notices): (Vec<_>, Vec<_>) =
                        filters.into_iter().map(compile_filter_checked).fold(
                            (Vec::new(), Vec::new()),
                            |(mut rules, mut warns), (r, w)| {
                                rules.push(r);
                                warns.extend(w);
                                (rules, warns)
                            },
                        );
                    self.active_view_mut().set_filters(compiled);
                    for w in notices {
                        self.status_message = w.clone();
                        self.push_event(json!({"type":"status","message": w}));
                    }
                    self.reset_file_match_viewport();
                    self.sync_active_project_from_terminals();
                }
            }
            Command::Scroll { offset } => {
                if self.active_terminal().is_file_session()
                    && self.active_terminal().file_backed.is_some()
                {
                    if self.active_view().uses_match_index() {
                        self.scroll_match_to_global_offset(offset);
                    } else {
                        self.scroll_file_to_global_offset(offset);
                    }
                } else {
                    self.scroll_to_offset(offset);
                }
            }
            Command::ScrollLines { delta } => self.scroll_by_lines(delta),
            Command::ScrollPage { direction } => self.scroll_page(direction),
            Command::ScrollTo { pos } => match pos.as_str() {
                "end" | "bottom" => self.scroll_to_end(),
                _ => self.scroll_to_start(),
            },
            Command::Resize { width, height } => {
                self.viewport_width = width;
                self.viewport_height = height;
                self.sync_terminal_geometry();
                self.mark_viewport_dirty();
            }
            Command::LoadFile { path } => self.open_log_file_command(&path),
            Command::ReloadFile { terminal_id } => self.reload_file_command(terminal_id.as_deref()),
            Command::Restart => self.restart(),
            Command::SetFormat { format_id } => self.set_format(&format_id),
            Command::SetFollow { follow } => {
                if self.active_terminal().is_file_session() {
                    // File sessions never Follow.
                    self.active_view_mut().auto_follow = false;
                    self.last_stats_at = None;
                } else {
                    self.active_view_mut().auto_follow = follow;
                    if follow {
                        self.active_terminal_mut().scroll_offset_y = self.max_scroll_offset();
                        self.mark_viewport_dirty();
                    } else {
                        self.materialize_live_terminal_tab();
                        let max = self.max_scroll_offset();
                        let y = self.active_terminal().scroll_offset_y.min(max);
                        self.active_terminal_mut().scroll_offset_y = y;
                    }
                    self.last_stats_at = None;
                }
                // Persist Follow with the workspace snapshot (#109).
                self.sync_active_project_from_terminals();
            }
            Command::LoadPreset { name } => self.preset_apply(&name),
            Command::SaveConfig => self.preset_create_from_tab(&self.preset_name.clone()),
            Command::PresetGet { name } => self.preset_get(&name),
            Command::PresetSave { name, filters } => self.preset_save(&name, filters),
            Command::PresetDelete { name } => self.preset_delete(&name),
            Command::PresetApply { name } => self.preset_apply(&name),
            Command::PresetCreateFromTab { name } => self.preset_create_from_tab(&name),
            Command::TabAdd => self.add_tab(),
            Command::TabClose { index } => self.close_tab(index),
            Command::TabSwitch { index } => self.switch_tab(index),
            Command::TabRename { index, name } => self.rename_tab(index, &name),
            Command::TabMove {
                from_index,
                to_index,
            } => self.tab_move(from_index, to_index),
            Command::TabRestore => self.restore_tab(),
            Command::SearchSet {
                query,
                regex,
                case_sensitive,
                whole_word,
            } => self.search_set(&query, regex, case_sensitive, whole_word),
            Command::FilterDraftSet { pattern, use_regex } => {
                self.filter_draft_set(&pattern, use_regex)
            }
            Command::SearchGoto { delta } => self.search_goto(delta),
            Command::SetWrapLines { wrap } => self.set_wrap_lines(wrap),
            Command::ScrollHorizontal { delta } => self.scroll_horizontal(delta),
            Command::SetScrollX { offset } => self.set_scroll_x(offset),
            Command::SelectionAt {
                x,
                y,
                extend,
                click_count,
            } => {
                self.selection_at(x, y, extend, click_count);
                self.mark_viewport_dirty();
            }
            Command::SelectionClear => {
                self.active_terminal_mut().selection = None;
                self.mark_viewport_dirty();
            }
            Command::OpenLinkAt { x, y } => {
                self.open_link_at(x, y);
            }
            Command::TerminalAdd => self.terminal_add(),
            Command::TerminalClose { terminal_id } => self.terminal_close(terminal_id.as_deref()),
            Command::TerminalSwitch { terminal_id } => self.terminal_switch(&terminal_id),
            Command::TerminalMove {
                terminal_id,
                to_index,
            } => self.terminal_move(&terminal_id, to_index),
            Command::TerminalRename { terminal_id, name } => {
                self.terminal_rename(&terminal_id, &name)
            }
            Command::TerminalStart { terminal_id } => self.terminal_start(terminal_id.as_deref()),
            Command::ProjectOpen { project_id } => self.project_open(&project_id),
            Command::ProjectCreate { name } => self.project_create(&name),
            Command::ProjectRename { project_id, name } => self.project_rename(&project_id, &name),
            Command::ProjectDelete { project_id } => self.project_delete(&project_id),
            Command::ProgramSetLaunch {
                terminal_id,
                command,
                args,
                cwd,
                wsl,
                wsl_distro,
            } => {
                self.program_set_launch(terminal_id.as_deref(), command, args, cwd, wsl, wsl_distro)
            }
            Command::SetSettings {
                max_scrollback_lines,
            } => self.set_settings(max_scrollback_lines),
            Command::SetSidebarExpanded { terminals, files } => {
                self.set_sidebar_expanded(terminals, files);
            }
            Command::SetViewportFontSize { size } => self.set_viewport_font_size(size),
            Command::SetViewportFocus { focused } => self.set_viewport_focus(focused),
            Command::SeveritySet { mode } => {
                let Some(parsed) = crate::core::types::SeverityFilter::parse(&mode) else {
                    return Err(format!("unknown severity mode: {mode}"));
                };
                self.active_view_mut().set_severity_filter(parsed);
                self.reset_file_match_viewport();
                // Persist the new severity mode with the workspace (#109).
                self.sync_active_project_from_terminals();
                self.last_stats_at = None;
                self.mark_viewport_dirty();
            }
            Command::RecordCollapseToggle { record_id } => {
                self.active_view_mut().toggle_record_collapse(record_id);
                self.last_stats_at = None;
                self.mark_viewport_dirty();
            }
            Command::RecordsExpandAll => {
                let terminal = self.active_terminal_mut();
                let view_idx = terminal.active_view;
                let records = terminal.buffer.records().to_vec();
                terminal.views[view_idx].expand_all_multiline(&records);
                self.last_stats_at = None;
                self.mark_viewport_dirty();
            }
            Command::RecordsCollapseAll => {
                self.active_view_mut().collapse_all_multiline();
                self.last_stats_at = None;
                self.mark_viewport_dirty();
            }
        }
        Ok(())
    }
}
