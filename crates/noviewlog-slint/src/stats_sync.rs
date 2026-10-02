//! Push typed engine [`StatsSnapshot`] into Slint models / chrome properties.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use noviewlog_core::core::types::{clamp_max_scrollback_lines, FilterType};
use noviewlog_core::{StatsSnapshot, StatsTerminal, TERMINAL_TAB_NAME};
use slint::{Model, SharedString, Timer, VecModel};

use crate::launch_preview::{format_launch_preview, LaunchPreview};
use crate::ui::{AppWindow, FilterInfo, ProjectInfo, TabInfo, TerminalInfo};

/// Pending debounced find `search_set` payload: query, regex, case, whole-word.
pub type FindPending = Option<(String, bool, bool, bool)>;

/// Apply one typed stats snapshot to all Slint chrome / models.
///
/// Returns `true` when tab, terminal, or filter models changed (caller may force paint).
pub fn apply_stats(
    stats: &StatsSnapshot,
    tabs: &Rc<VecModel<TabInfo>>,
    terminals: &Rc<VecModel<TerminalInfo>>,
    files: &Rc<VecModel<TerminalInfo>>,
    projects: &Rc<VecModel<ProjectInfo>>,
    filters: &Rc<VecModel<FilterInfo>>,
    ui: &AppWindow,
    terminal_tab_active: &Rc<Cell<bool>>,
    syncing_scroll: &Rc<Cell<bool>>,
    has_selection: &Rc<Cell<bool>>,
    pty_running: &Rc<Cell<bool>>,
    viewport_font_size: &Rc<Cell<f32>>,
    syncing_follow: &Rc<Cell<bool>>,
    find_resync: &Rc<Cell<bool>>,
    find_stats_tab: &Rc<Cell<i32>>,
    find_pending: &Rc<RefCell<FindPending>>,
    find_debounce: &Rc<Timer>,
) -> bool {
    let tabs_changed = apply_stats_to_tabs(stats, tabs, ui, terminal_tab_active);
    let terms_changed = apply_stats_to_terminals(stats, terminals, files, projects, ui);
    let filters_changed = apply_stats_to_filters(stats, filters, ui);
    apply_stats_to_find(
        stats,
        ui,
        find_resync,
        find_stats_tab,
        find_pending,
        find_debounce,
    );
    apply_stats_to_scroll(stats, ui, syncing_scroll);
    apply_stats_to_selection(stats, has_selection, ui);
    apply_stats_to_running(stats, pty_running);
    apply_stats_to_view_chrome(stats, ui, viewport_font_size, syncing_follow);
    apply_stats_to_launch_preview(stats, ui);
    tabs_changed || terms_changed || filters_changed
}

fn apply_stats_to_tabs(
    stats: &StatsSnapshot,
    tabs: &Rc<VecModel<TabInfo>>,
    ui: &AppWindow,
    terminal_tab_active: &Rc<Cell<bool>>,
) -> bool {
    let active = stats.active_tab as i32;
    let can_restore = stats.can_restore_closed_tab;
    let is_terminal_tab = stats.is_terminal_tab;

    let mut changed = false;
    if ui.get_active_tab_index() != active {
        ui.set_active_tab_index(active);
        changed = true;
    }
    if ui.get_can_restore_tab() != can_restore {
        ui.set_can_restore_tab(can_restore);
        changed = true;
    }
    terminal_tab_active.set(is_terminal_tab);

    let next: Vec<TabInfo> = stats
        .tabs
        .iter()
        .map(|tab| {
            let index = tab.index as i32;
            let name: String = if tab.name.is_empty() {
                if index == 0 {
                    TERMINAL_TAB_NAME.to_string()
                } else {
                    // Stable default matching what the UI shows optimistically
                    // on creation ("Tab 2" for the first extra tab), so the
                    // chip does not visibly snap right after tab-add.
                    format!("Tab {}", index + 1)
                }
            } else {
                tab.name.clone()
            };
            TabInfo {
                index,
                name: SharedString::from(name),
                is_terminal_tab: tab.is_terminal_tab,
            }
        })
        .collect();

    if model_differs(tabs, &next) {
        tabs.set_vec(next);
        changed = true;
    }

    changed
}

fn apply_stats_to_scroll(stats: &StatsSnapshot, ui: &AppWindow, syncing: &Rc<Cell<bool>>) {
    let scroll_y = stats.scroll_y;
    let max_scroll_y = stats.max_scroll_y;
    let scroll_x = stats.scroll_x;
    let max_scroll_x = stats.max_scroll_x;
    let wrap_lines = stats.wrap_lines;
    let show_h = !wrap_lines && max_scroll_x > 0.5;

    syncing.set(true);
    let max_y = max_scroll_y.max(0.0);
    let max_x = max_scroll_x.max(0.0);
    let next_y = scroll_y.clamp(0.0, max_y);
    let next_x = scroll_x.clamp(0.0, max_x);
    // Always refresh extents (thumb size); skip scroll position if unchanged to avoid churn.
    ui.set_max_scroll_y(max_y);
    ui.set_max_scroll_x(max_x);
    if (ui.get_scroll_y() - next_y).abs() > 0.5 {
        ui.set_scroll_y(next_y);
    }
    if (ui.get_scroll_x() - next_x).abs() > 0.5 {
        ui.set_scroll_x(next_x);
    }
    if ui.get_show_hscroll() != show_h {
        ui.set_show_hscroll(show_h);
    }
    syncing.set(false);
}

fn apply_stats_to_terminals(
    stats: &StatsSnapshot,
    terminals: &Rc<VecModel<TerminalInfo>>,
    files: &Rc<VecModel<TerminalInfo>>,
    projects: &Rc<VecModel<ProjectInfo>>,
    ui: &AppWindow,
) -> bool {
    let active = stats.active_terminal as i32;

    let mut changed = false;
    if ui.get_active_terminal_index() != active {
        ui.set_active_terminal_index(active);
        changed = true;
    }
    if ui.get_is_file_session() != stats.is_file_session {
        ui.set_is_file_session(stats.is_file_session);
        changed = true;
    }
    if ui.get_terminals_section_expanded() != stats.terminals_section_expanded {
        ui.set_terminals_section_expanded(stats.terminals_section_expanded);
        changed = true;
    }
    if ui.get_files_section_expanded() != stats.files_section_expanded {
        ui.set_files_section_expanded(stats.files_section_expanded);
        changed = true;
    }

    let active_project_id = stats.active_project_id.as_deref().unwrap_or("");
    if ui.get_active_project_id().as_str() != active_project_id {
        ui.set_active_project_id(SharedString::from(active_project_id));
        changed = true;
    }
    let active_project_name = stats
        .active_project_id
        .as_ref()
        .and_then(|id| stats.projects.iter().find(|p| &p.id == id))
        .map(|p| p.name.as_str())
        .unwrap_or("");
    if ui.get_active_project_name().as_str() != active_project_name {
        ui.set_active_project_name(SharedString::from(active_project_name));
        changed = true;
    }

    let next_terms: Vec<TerminalInfo> =
        stats.terminals.iter().map(stats_terminal_to_info).collect();
    if model_differs(terminals, &next_terms) {
        terminals.set_vec(next_terms);
        changed = true;
    }

    let next_files: Vec<TerminalInfo> = stats.files.iter().map(stats_terminal_to_info).collect();
    if model_differs(files, &next_files) {
        files.set_vec(next_files);
        changed = true;
    }

    let next_projects: Vec<ProjectInfo> = stats
        .projects
        .iter()
        .map(|p| ProjectInfo {
            index: p.index as i32,
            id: SharedString::from(p.id.as_str()),
            name: SharedString::from(p.name.as_str()),
            program_count: p.program_count as i32,
            active: stats
                .active_project_id
                .as_ref()
                .is_some_and(|id| id == &p.id),
        })
        .collect();
    if model_differs(projects, &next_projects) {
        projects.set_vec(next_projects);
        changed = true;
    }

    changed
}

fn stats_terminal_to_info(term: &StatsTerminal) -> TerminalInfo {
    let label = if term.label.is_empty() {
        "."
    } else {
        term.label.as_str()
    };
    TerminalInfo {
        index: term.index as i32,
        id: SharedString::from(term.id.as_str()),
        label: SharedString::from(label),
        cwd: SharedString::from(term.cwd.as_str()),
        running: term.running,
        has_launch: term.has_launch,
        launch_command: SharedString::from(term.launch_command.as_str()),
        launch_args: SharedString::from(term.launch_args.as_str()),
        launch_cwd: SharedString::from(term.launch_cwd.as_str()),
        launch_wsl: term.launch_wsl,
        launch_wsl_distro: SharedString::from(term.launch_wsl_distro.as_str()),
    }
}

/// True when the model's rows differ from `next`, i.e. `set_vec(next)` would
/// change content. Slint derives `PartialEq` over every generated field, so
/// element equality is a full content diff.
fn model_differs<T: PartialEq + Clone + 'static>(model: &VecModel<T>, next: &[T]) -> bool {
    if model.row_count() != next.len() {
        return true;
    }
    for (i, item) in next.iter().enumerate() {
        let Some(cur) = model.row_data(i) else {
            return true;
        };
        if cur != *item {
            return true;
        }
    }
    false
}

fn apply_stats_to_filters(
    stats: &StatsSnapshot,
    filters: &Rc<VecModel<FilterInfo>>,
    ui: &AppWindow,
) -> bool {
    let active = stats.active_tab as i32;
    let is_terminal_tab = stats.is_terminal_tab;
    let editable = !is_terminal_tab && active != 0;
    let mut changed = false;
    if ui.get_filters_editable() != editable {
        ui.set_filters_editable(editable);
        changed = true;
    }

    let next: Vec<FilterInfo> = stats
        .filters
        .iter()
        .map(|f| {
            let filter_type = match f.filter_type {
                FilterType::Include => "include",
                FilterType::Exclude => "exclude",
            };
            FilterInfo {
                id: SharedString::from(f.id.as_str()),
                filter_type: SharedString::from(filter_type),
                pattern: SharedString::from(f.pattern.as_str()),
                enabled: f.enabled,
                use_regex: f.use_regex,
            }
        })
        .collect();

    if filters.row_count() == next.len()
        && (0..next.len()).all(|i| filters.row_data(i).is_some_and(|cur| cur.id == next[i].id))
    {
        for (i, filt) in next.into_iter().enumerate() {
            let Some(cur) = filters.row_data(i) else {
                continue;
            };
            if cur.filter_type != filt.filter_type
                || cur.pattern != filt.pattern
                || cur.enabled != filt.enabled
                || cur.use_regex != filt.use_regex
            {
                filters.set_row_data(i, filt);
                changed = true;
            }
        }
        return changed;
    }

    if model_differs(filters, &next) {
        filters.set_vec(next);
        changed = true;
    }

    changed
}

fn apply_stats_to_find(
    stats: &StatsSnapshot,
    ui: &AppWindow,
    find_resync: &Rc<Cell<bool>>,
    find_stats_tab: &Rc<Cell<i32>>,
    find_pending: &Rc<RefCell<FindPending>>,
    find_debounce: &Rc<Timer>,
) {
    if !ui.get_find_open() {
        return;
    }

    let active = stats.active_tab as i32;
    let tab_changed = find_stats_tab.get() != active;
    find_stats_tab.set(active);
    // Tab switch: drop in-flight typing for the previous tab.
    if tab_changed {
        find_debounce.stop();
        find_pending.borrow_mut().take();
    }
    let open_resync = find_resync.replace(false);
    let has_pending = find_pending.borrow().is_some();
    // Never clobber the Find field while a debounced edit is in flight — that
    // race made the query snap back to the engine's (often empty) search when
    // focus left the TextInput and stats applied.
    let resync = tab_changed || (open_resync && !has_pending);

    let query = stats.search_query.as_str();
    let regex = stats.search_regex;
    let case_sensitive = stats.search_case_sensitive;
    let whole_word = stats.search_whole_word;
    let counter = stats.search_counter.as_str();
    let error = stats.search_error.as_deref().unwrap_or("");

    // Only push query/toggles on open or tab switch — never while the user types.
    if resync {
        let ui_query = ui.get_find_query();
        // If the user already typed into an empty engine search, keep the UI text.
        if (tab_changed || query != ui_query.as_str())
            && !(query.is_empty() && !ui_query.is_empty() && !tab_changed)
        {
            ui.set_find_query(SharedString::from(query));
        }
        ui.set_find_regex(regex);
        ui.set_find_case_sensitive(case_sensitive);
        ui.set_find_whole_word(whole_word);
    }

    let ui_query = ui.get_find_query();
    let status_query = if resync && !query.is_empty() {
        query
    } else {
        ui_query.as_str()
    };
    // Status counter is engine-truth; only show it when it matches the UI query
    // (avoids "No results" flash for text not yet flushed via debounce).
    let counter_applies = !has_pending && ui_query.as_str() == query;
    let status =
        if (!error.is_empty() && counter_applies) || status_query.is_empty() || !counter_applies {
            SharedString::default()
        } else if counter == "0/0" {
            SharedString::from("No results")
        } else if counter.is_empty() {
            SharedString::default()
        } else if let Some((cur, total)) = counter.split_once('/') {
            SharedString::from(format!("{cur} of {total}"))
        } else {
            SharedString::from(counter)
        };
    ui.set_find_status(status);
    if counter_applies {
        ui.set_find_error(SharedString::from(error));
    } else if !has_pending {
        ui.set_find_error(SharedString::default());
    }
}

fn apply_stats_to_selection(stats: &StatsSnapshot, has_selection: &Rc<Cell<bool>>, ui: &AppWindow) {
    let selected = stats.has_selection;
    has_selection.set(selected);
    ui.set_can_copy(selected);
}

fn apply_stats_to_running(stats: &StatsSnapshot, pty_running: &Rc<Cell<bool>>) {
    pty_running.set(stats.running);
}

fn apply_stats_to_launch_preview(stats: &StatsSnapshot, ui: &AppWindow) {
    let show = stats.has_active_terminal && !stats.is_file_session && !stats.running;
    if ui.get_show_launch_preview() != show {
        ui.set_show_launch_preview(show);
    }
    if !show {
        if !ui.get_launch_preview_text().is_empty() {
            ui.set_launch_preview_text(SharedString::default());
        }
        return;
    }
    let text = stats
        .terminals
        .iter()
        .find(|t| t.index == stats.active_terminal)
        .map(|t| {
            format_launch_preview(LaunchPreview {
                command: t.launch_command.as_str(),
                args: t.launch_args.as_str(),
                cwd: t.launch_cwd.as_str(),
                wsl: t.launch_wsl,
                wsl_distro: t.launch_wsl_distro.as_str(),
            })
        })
        .unwrap_or_else(|| "Type to open a shell".to_string());
    if ui.get_launch_preview_text().as_str() != text {
        ui.set_launch_preview_text(SharedString::from(text));
    }
}

fn apply_stats_to_view_chrome(
    stats: &StatsSnapshot,
    ui: &AppWindow,
    viewport_font_size: &Rc<Cell<f32>>,
    syncing_follow: &Rc<Cell<bool>>,
) {
    viewport_font_size.set(stats.viewport_font_size);

    let line_pos = if stats.viewport_line_total > 0 && stats.viewport_line > 0 {
        SharedString::from(format!(
            "{} / {}",
            stats.viewport_line, stats.viewport_line_total
        ))
    } else {
        SharedString::default()
    };
    if ui.get_line_position_text() != line_pos {
        ui.set_line_position_text(line_pos);
    }

    // Match-set truncation hint (issue #150): engine truth until the next
    // scan/invalidate resets it.
    if ui.get_matches_capped() != stats.match_capped {
        ui.set_matches_capped(stats.match_capped);
    }

    let capped = clamp_max_scrollback_lines(stats.max_scrollback_lines) as i32;
    if ui.get_max_scrollback_lines() != capped {
        ui.set_max_scrollback_lines(capped);
    }

    let wrap_lines = stats.wrap_lines;
    if ui.get_wrap_lines() != wrap_lines {
        ui.set_wrap_lines(wrap_lines);
    }

    let severity = if stats.severity_filter.is_empty() {
        "all"
    } else {
        stats.severity_filter.as_str()
    };
    if ui.get_severity_mode().as_str() != severity {
        ui.set_severity_mode(SharedString::from(severity));
    }

    // Slint `auto-follow` / callback `set-follow` ↔ engine `SetFollow` / stats `auto_follow`.
    let auto_follow = stats.auto_follow;
    if ui.get_auto_follow() != auto_follow {
        // Guard: if property write ever re-enters on_set_follow, do not echo SetFollow.
        syncing_follow.set(true);
        ui.set_auto_follow(auto_follow);
        syncing_follow.set(false);
    }

    let tab_count = stats.tab_count as i32;
    let active = stats.active_tab as i32;
    let is_terminal_tab = stats.is_terminal_tab;
    let can_restore = stats.can_restore_closed_tab;
    let on_terminal_tab = is_terminal_tab || active == 0;
    let can_close = tab_count > 1 && !on_terminal_tab;
    if ui.get_can_close_tab() != can_close {
        ui.set_can_close_tab(can_close);
    }
    if ui.get_can_restore_tab() != can_restore {
        ui.set_can_restore_tab(can_restore);
    }
    // The Terminal tab must not be renamed — mirror Close enablement for Tab → Rename.
    let can_rename = !on_terminal_tab;
    if ui.get_can_rename_tab() != can_rename {
        ui.set_can_rename_tab(can_rename);
    }
    // Drop inline rename if the target tab vanished (e.g. closed elsewhere).
    let renaming = ui.get_renaming_tab_index();
    if renaming >= 0 && renaming >= tab_count {
        ui.set_renaming_tab_index(-1);
        if ui.get_renaming_terminal_id().is_empty() {
            ui.set_rename_draft(SharedString::default());
        }
    }

    // Drop terminal rename if the target id vanished (or never matched after stats).
    let renaming_tid = ui.get_renaming_terminal_id();
    if !renaming_tid.is_empty()
        && !stats
            .terminals
            .iter()
            .chain(stats.files.iter())
            .any(|t| t.id.as_str() == renaming_tid.as_str())
    {
        ui.set_renaming_terminal_id(SharedString::default());
        if ui.get_renaming_tab_index() < 0 {
            ui.set_rename_draft(SharedString::default());
        }
    }
}

#[cfg(test)]
mod tests {
    use noviewlog_core::{parse_engine_event, EngineEvent, StatsProject, StatsTab};
    use slint::platform::software_renderer::{MinimalSoftwareWindow, RepaintBufferType};
    use slint::platform::{Platform, PlatformError, WindowAdapter};

    use super::*;

    thread_local! {
        static WINDOW: Rc<MinimalSoftwareWindow> =
            MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
    }

    struct TestPlatform;

    impl Platform for TestPlatform {
        fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
            Ok(WINDOW.with(|w| w.clone()))
        }
    }

    /// First caller wins the process-global platform slot; later callers reuse it.
    /// `create_window_adapter` hands out the calling thread's window, so every
    /// test thread gets its own headless `AppWindow`.
    fn install_platform() {
        slint::platform::set_platform(Box::new(TestPlatform)).ok();
    }

    fn test_ui() -> AppWindow {
        install_platform();
        AppWindow::new().expect("headless AppWindow")
    }

    fn stats(json: &str) -> StatsSnapshot {
        match parse_engine_event(json).expect("parse engine event") {
            EngineEvent::Stats(s) => s,
            other => panic!("expected stats event, got {other:?}"),
        }
    }

    fn base_stats() -> StatsSnapshot {
        stats(r#"{"type":"stats"}"#)
    }

    fn terminal(index: usize, id: &str, label: &str) -> StatsTerminal {
        StatsTerminal {
            index,
            id: id.to_string(),
            label: label.to_string(),
            running: false,
            cwd: String::new(),
            has_launch: false,
            program_id: None,
            launch_command: String::new(),
            launch_args: String::new(),
            launch_cwd: String::new(),
            launch_wsl: false,
            launch_wsl_distro: String::new(),
        }
    }

    struct FindCtx {
        resync: Rc<Cell<bool>>,
        stats_tab: Rc<Cell<i32>>,
        pending: Rc<RefCell<FindPending>>,
        debounce: Rc<Timer>,
    }

    impl FindCtx {
        fn new() -> Self {
            Self {
                resync: Rc::new(Cell::new(false)),
                stats_tab: Rc::new(Cell::new(-1)),
                pending: Rc::new(RefCell::new(None)),
                debounce: Rc::new(Timer::default()),
            }
        }
    }

    /// One headless `AppWindow` plus every model / cell `apply_stats` touches.
    struct Harness {
        ui: AppWindow,
        tabs: Rc<VecModel<TabInfo>>,
        terminals: Rc<VecModel<TerminalInfo>>,
        files: Rc<VecModel<TerminalInfo>>,
        projects: Rc<VecModel<ProjectInfo>>,
        filters: Rc<VecModel<FilterInfo>>,
        terminal_tab_active: Rc<Cell<bool>>,
        syncing_scroll: Rc<Cell<bool>>,
        has_selection: Rc<Cell<bool>>,
        pty_running: Rc<Cell<bool>>,
        viewport_font_size: Rc<Cell<f32>>,
        syncing_follow: Rc<Cell<bool>>,
        find: FindCtx,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                ui: test_ui(),
                tabs: Rc::new(VecModel::default()),
                terminals: Rc::new(VecModel::default()),
                files: Rc::new(VecModel::default()),
                projects: Rc::new(VecModel::default()),
                filters: Rc::new(VecModel::default()),
                terminal_tab_active: Rc::new(Cell::new(true)),
                syncing_scroll: Rc::new(Cell::new(false)),
                has_selection: Rc::new(Cell::new(false)),
                pty_running: Rc::new(Cell::new(false)),
                viewport_font_size: Rc::new(Cell::new(13.0)),
                syncing_follow: Rc::new(Cell::new(false)),
                find: FindCtx::new(),
            }
        }
    }

    fn apply(h: &Harness, s: &StatsSnapshot) -> bool {
        apply_stats(
            s,
            &h.tabs,
            &h.terminals,
            &h.files,
            &h.projects,
            &h.filters,
            &h.ui,
            &h.terminal_tab_active,
            &h.syncing_scroll,
            &h.has_selection,
            &h.pty_running,
            &h.viewport_font_size,
            &h.syncing_follow,
            &h.find.resync,
            &h.find.stats_tab,
            &h.find.pending,
            &h.find.debounce,
        )
    }

    // ---- Model diff rules ----

    #[test]
    fn apply_stats_populates_models_and_reports_change() {
        let h = Harness::new();
        let mut s = base_stats();
        s.active_tab = 1;
        s.can_restore_closed_tab = true;
        s.is_terminal_tab = true;
        s.tabs = vec![
            StatsTab {
                index: 0,
                name: String::new(),
                is_terminal_tab: true,
            },
            StatsTab {
                index: 1,
                name: String::new(),
                is_terminal_tab: false,
            },
        ];

        assert!(apply(&h, &s));
        assert_eq!(h.ui.get_active_tab_index(), 1);
        assert!(h.ui.get_can_restore_tab());
        assert!(h.terminal_tab_active.get());
        assert_eq!(h.tabs.row_count(), 2);
        let terminal_tab = h.tabs.row_data(0).expect("row 0");
        assert_eq!(terminal_tab.name.as_str(), TERMINAL_TAB_NAME);
        assert!(terminal_tab.is_terminal_tab);
        let view_tab = h.tabs.row_data(1).expect("row 1");
        assert_eq!(view_tab.name.as_str(), "Tab 2");
        assert!(!view_tab.is_terminal_tab);
    }

    #[test]
    fn apply_stats_same_snapshot_twice_is_noop() {
        let h = Harness::new();
        let mut s = base_stats();
        s.tabs = vec![StatsTab {
            index: 0,
            name: String::new(),
            is_terminal_tab: true,
        }];

        assert!(apply(&h, &s));
        assert!(!apply(&h, &s));
    }

    #[test]
    fn terminal_label_change_is_detected() {
        let h = Harness::new();
        let mut s = base_stats();
        s.tabs = vec![StatsTab {
            index: 0,
            name: String::new(),
            is_terminal_tab: true,
        }];
        s.terminals = vec![terminal(0, "t0", "")];

        assert!(apply(&h, &s));
        assert_eq!(h.terminals.row_data(0).unwrap().label.as_str(), ".");

        s.terminals[0].label = "shell".into();
        s.terminals[0].cwd = "/tmp".into();
        s.terminals[0].running = true;
        assert!(apply(&h, &s));
        let row = h.terminals.row_data(0).unwrap();
        assert_eq!(row.label.as_str(), "shell");
        assert_eq!(row.cwd.as_str(), "/tmp");
        assert!(row.running);
    }

    #[test]
    fn filter_update_same_ids_updates_in_place() {
        let h = Harness::new();
        let mut s = stats(
            r#"{"type":"stats","filters":[
                {"id":"f1","type":"include","pattern":"error","enabled":true,"use_regex":false}
            ]}"#,
        );
        assert!(apply(&h, &s));
        assert_eq!(h.filters.row_count(), 1);
        assert_eq!(h.filters.row_data(0).unwrap().pattern.as_str(), "error");

        // Same id sequence, changed fields → id-stable row update, not a reset.
        s = stats(
            r#"{"type":"stats","filters":[
                {"id":"f1","type":"include","pattern":"warn","enabled":false,"use_regex":true}
            ]}"#,
        );
        assert!(apply(&h, &s));
        let row = h.filters.row_data(0).unwrap();
        assert_eq!(row.pattern.as_str(), "warn");
        assert!(!row.enabled);
        assert!(row.use_regex);
        assert!(!apply(&h, &s));

        // Id sequence changed → whole-model diff replaces the rows.
        s = stats(
            r#"{"type":"stats","filters":[
                {"id":"f2","type":"exclude","pattern":"debug","enabled":true,"use_regex":false}
            ]}"#,
        );
        assert!(apply(&h, &s));
        assert_eq!(h.filters.row_data(0).unwrap().id.as_str(), "f2");
    }

    #[test]
    fn projects_active_flag_differs() {
        let h = Harness::new();
        let mut s = base_stats();
        s.projects = vec![StatsProject {
            index: 0,
            id: "p1".into(),
            name: "Proj".into(),
            program_count: 2,
        }];
        s.active_project_id = Some("p1".into());

        assert!(apply(&h, &s));
        let row = h.projects.row_data(0).unwrap();
        assert_eq!(row.name.as_str(), "Proj");
        assert!(row.active);

        s.active_project_id = None;
        assert!(apply(&h, &s));
        assert!(!h.projects.row_data(0).unwrap().active);
    }

    // ---- Match-cap hint ----

    #[test]
    fn matches_capped_hint_follows_stats() {
        let h = Harness::new();
        let mut s = base_stats();
        assert!(!h.ui.get_matches_capped(), "default must hide the hint");

        s.match_capped = true;
        apply(&h, &s);
        assert!(h.ui.get_matches_capped());

        // Engine reset (next scan / invalidate) clears it again.
        s.match_capped = false;
        apply(&h, &s);
        assert!(!h.ui.get_matches_capped());
    }

    // ---- Find resync rules ----

    #[test]
    fn find_closed_is_untouched() {
        let h = Harness::new();
        h.ui.set_find_open(false);
        h.ui.set_find_query("user".into());
        h.ui.set_find_status("1 of 3".into());
        h.find.resync.set(true);
        *h.find.pending.borrow_mut() = Some(("typ".into(), false, false, false));

        let mut s = base_stats();
        s.search_query = "engine".into();
        s.search_counter = "1/3".into();
        apply(&h, &s);

        assert_eq!(h.ui.get_find_query().as_str(), "user");
        assert_eq!(h.ui.get_find_status().as_str(), "1 of 3");
        assert!(h.find.resync.get());
        assert!(h.find.pending.borrow().is_some());
    }

    #[test]
    fn tab_switch_drops_in_flight_typing() {
        let h = Harness::new();
        h.ui.set_find_open(true);
        let mut s = base_stats();
        s.active_tab = 0;
        apply(&h, &s);

        *h.find.pending.borrow_mut() = Some(("typ".into(), false, false, false));
        h.ui.set_find_query("typ".into());

        // New tab with an empty engine search: pending dropped, UI cleared.
        let mut next = base_stats();
        next.active_tab = 1;
        assert!(apply(&h, &next));
        assert!(h.find.pending.borrow().is_none());
        assert_eq!(h.ui.get_find_query().as_str(), "");

        // New tab with an engine search: query replaced.
        let mut other = base_stats();
        other.active_tab = 2;
        other.search_query = "next".into();
        apply(&h, &other);
        assert!(h.find.pending.borrow().is_none());
        assert_eq!(h.ui.get_find_query().as_str(), "next");
    }

    #[test]
    fn resync_never_clobbers_pending_typing() {
        let h = Harness::new();
        h.ui.set_find_open(true);
        apply(&h, &base_stats());

        h.ui.set_find_query("typ".into());
        h.ui.set_find_status("stale".into());
        h.find.resync.set(true);
        *h.find.pending.borrow_mut() = Some(("typ".into(), false, false, false));

        let mut s = base_stats();
        s.search_regex = true;
        s.search_case_sensitive = true;
        s.search_whole_word = true;
        s.search_counter = "2/5".into();
        apply(&h, &s);

        assert_eq!(h.ui.get_find_query().as_str(), "typ");
        assert!(!h.ui.get_find_regex());
        assert!(!h.ui.get_find_case_sensitive());
        assert!(!h.ui.get_find_whole_word());
        assert_eq!(h.ui.get_find_status().as_str(), "");
        assert_eq!(h.ui.get_find_error().as_str(), "");
        assert!(h.find.pending.borrow().is_some());
    }

    #[test]
    fn resync_pushes_engine_query_without_pending() {
        let h = Harness::new();
        h.ui.set_find_open(true);
        apply(&h, &base_stats());
        h.ui.set_find_query("old".into());

        let mut s = base_stats();
        s.search_query = "boom".into();
        s.search_regex = true;
        s.search_case_sensitive = true;
        s.search_whole_word = true;
        s.search_counter = "2/5".into();
        h.find.resync.set(true);
        apply(&h, &s);

        assert_eq!(h.ui.get_find_query().as_str(), "boom");
        assert!(h.ui.get_find_regex());
        assert!(h.ui.get_find_case_sensitive());
        assert!(h.ui.get_find_whole_word());
        assert_eq!(h.ui.get_find_status().as_str(), "2 of 5");
    }

    #[test]
    fn resync_keeps_user_text_when_engine_query_empty() {
        let h = Harness::new();
        h.ui.set_find_open(true);
        apply(&h, &base_stats());
        h.ui.set_find_query("user".into());

        let mut s = base_stats();
        s.search_regex = true;
        h.find.resync.set(true);
        apply(&h, &s);

        assert_eq!(h.ui.get_find_query().as_str(), "user");
        assert!(h.ui.get_find_regex());
    }

    #[test]
    fn counter_only_applies_to_matching_query() {
        let h = Harness::new();
        h.ui.set_find_open(true);
        let mut s = base_stats();
        s.search_query = "boom".into();
        h.find.resync.set(true);
        apply(&h, &s);
        assert_eq!(h.ui.get_find_query().as_str(), "boom");

        s.search_counter = "3/7".into();
        apply(&h, &s);
        assert_eq!(h.ui.get_find_status().as_str(), "3 of 7");

        s.search_counter = "0/0".into();
        apply(&h, &s);
        assert_eq!(h.ui.get_find_status().as_str(), "No results");

        s.search_counter = "1/1".into();
        s.search_error = Some("bad regex".into());
        apply(&h, &s);
        assert_eq!(h.ui.get_find_status().as_str(), "");
        assert_eq!(h.ui.get_find_error().as_str(), "bad regex");

        // UI query no longer matches the engine → counter + error suppressed.
        h.ui.set_find_query("unflushed".into());
        apply(&h, &s);
        assert_eq!(h.ui.get_find_status().as_str(), "");
        assert_eq!(h.ui.get_find_error().as_str(), "");
    }
}
