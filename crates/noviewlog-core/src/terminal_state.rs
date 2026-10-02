//! Per-terminal session state (`TerminalState`).
//!
//! A **terminal** is an independent session (PTY shell, process, or read-only
//! log file). Inside it, `views` holds the Terminal tab plus filter tabs (`LogView`).
//! JSON/UI still call those tabs via `tab_*` commands.

use std::collections::VecDeque;
use std::path::Path;
use std::time::Instant;

use crate::core::buffer::RecordBuffer;
use crate::core::config::RuntimeConfig;
use crate::core::parser::RecordParser;
use crate::core::terminal::TerminalIngest;
use crate::core::types::{LaunchConfig, LogFormat, TabConfig};
use crate::file_index::FileBackedLog;
use crate::file_load::FileLoadHandle;
use crate::log_view::{LogView, TERMINAL_TAB_NAME};
use crate::viewport_layout::TextSelection;

pub const MAX_CLOSED_TABS: usize = 15;

/// Chunked swap of the in-memory file window while scrolling giant logs.
pub struct PendingFileWindow {
    pub new_start: u64,
    pub scroll_y: f32,
    pub next_line: u64,
    pub end_line: u64,
    pub lines: Vec<String>,
}

/// A spawn parked while a worker thread resolves the executable (issue #59).
/// The UI thread never probes PATH / the registry; `advance_pending_spawns`
/// applies the plan when resolution lands.
#[derive(Debug, Clone)]
pub struct PendingSpawn {
    /// Original (unresolved) command as given to the resolver.
    pub command: String,
    pub args: Vec<String>,
    /// Expanded working directory the resolution ran against.
    pub workdir: String,
    /// PTY generation token bumped when the spawn was queued.
    pub generation: u64,
    /// Human-readable spawn line for status / error messages.
    pub cmdline: String,
    /// Status prefix for spawn failures ("Failed to start" / "Shell failed").
    pub fail_prefix: String,
}

/// Runtime state for one terminal session (shell/process + Terminal/filter tabs).
pub struct TerminalState {
    pub id: String,
    /// Live working directory (spawn cwd, updated via OSC 7 when available).
    pub cwd: String,
    /// Optional sidebar title override; when `Some(non-empty)`, preferred by [`Self::label`].
    pub custom_title: Option<String>,
    /// Linked [`crate::core::types::ProgramConfig::id`] when this session belongs to an open Project.
    pub program_id: Option<String>,
    pub launch: LaunchConfig,
    pub views: Vec<LogView>,
    pub active_view: usize,
    pub closed_tabs: VecDeque<TabConfig>,
    pub buffer: RecordBuffer,
    pub parser: RecordParser,
    pub ingest: TerminalIngest,
    pub running: bool,
    pub exit_code: Option<i32>,
    pub process_started: bool,
    /// Monotonic PTY session token for this terminal. Compared to [`crate::pty::PtyEvent::Exit`].
    pub pty_generation: u64,
    pub last_line_at: Option<Instant>,
    pub scroll_offset_y: f32,
    pub scroll_x: f32,
    pub selection: Option<TextSelection>,
    pub scroll_to_row: Option<usize>,
    pub file_load: Option<FileLoadHandle>,
    /// When the pending `file_load` last showed progress (issue #253). A
    /// load whose worker died or blocks forever would otherwise pin the
    /// fast tick cadence forever; once this is older than
    /// [`crate::file_load`] `FILE_LOAD_STALL_TIMEOUT` the engine fails the
    /// load. `None` while no load is pending or progress was just observed.
    pub file_load_stalled_at: Option<Instant>,
    /// On-demand reads after a file load completes.
    pub file_backed: Option<FileBackedLog>,
    /// True when the watched file changed on disk after open (issue #151):
    /// truncated, appended, rewritten, or deleted. Cleared by a reload.
    pub file_changed: bool,
    /// First raw file line number currently held in `buffer`.
    pub buffer_line_start: u64,
    /// One past the last raw file line in `buffer`.
    pub buffer_line_end: u64,
    pub pending_file_window: Option<PendingFileWindow>,
    /// Spawn queued but not started yet: executable resolution runs on a
    /// worker thread (issue #59). `None` when there is nothing in flight.
    pub pending_spawn: Option<PendingSpawn>,
    /// Keystrokes typed while `pending_spawn` was in flight; flushed into the
    /// PTY as soon as it starts so early typing is not lost.
    pub pending_stdin: Vec<u8>,
}

impl TerminalState {
    /// True while a pending background load should keep the fast tick
    /// cadence (issue #253): pending AND not stalled past
    /// `FILE_LOAD_STALL_TIMEOUT`, so a dead or blocked worker cannot wedge
    /// the cadence forever.
    pub fn file_load_active(&self) -> bool {
        self.file_load.is_some()
            && self
                .file_load_stalled_at
                .is_none_or(|at| at.elapsed() < crate::file_load::FILE_LOAD_STALL_TIMEOUT)
    }

    /// Pure stall-backstop decision (issue #253): a load that produced no
    /// events since `stalled_since` is failed once the timeout lapses,
    /// measured against `now`. Exposed as a pure function so the backstop is
    /// testable without actually waiting 120 s.
    pub fn file_load_stall_expired(stalled_since: Instant, now: Instant) -> bool {
        now.duration_since(stalled_since) >= crate::file_load::FILE_LOAD_STALL_TIMEOUT
    }
    pub fn new(
        id: String,
        launch: LaunchConfig,
        runtime: &RuntimeConfig,
        format: &LogFormat,
        max_records: usize,
    ) -> Self {
        let cwd = resolve_initial_cwd(&launch);
        let _ = runtime;
        Self {
            id,
            cwd,
            custom_title: None,
            program_id: None,
            launch,
            views: vec![LogView::from_runtime(TERMINAL_TAB_NAME, Vec::new())],
            active_view: 0,
            closed_tabs: VecDeque::new(),
            buffer: RecordBuffer::new(max_records),
            parser: RecordParser::new(format.clone()),
            ingest: TerminalIngest::new(),
            running: false,
            exit_code: None,
            process_started: false,
            pty_generation: 0,
            last_line_at: None,
            scroll_offset_y: 0.0,
            scroll_x: 0.0,
            selection: None,
            scroll_to_row: None,
            file_load: None,
            file_load_stalled_at: None,
            file_backed: None,
            file_changed: false,
            buffer_line_start: 0,
            buffer_line_end: 0,
            pending_file_window: None,
            pending_spawn: None,
            pending_stdin: Vec::new(),
        }
    }

    /// Sidebar / tab label: custom title when set; else file basename or cwd segment.
    pub fn label(&self) -> String {
        if let Some(title) = self
            .custom_title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return title.to_string();
        }
        if let Some(path) = self.file_session_path() {
            return Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(path)
                .to_string();
        }
        cwd_label(&self.cwd)
    }

    /// Path for an open / loading log file, if this terminal is a file session.
    pub fn file_session_path(&self) -> Option<&str> {
        self.file_backed
            .as_ref()
            .map(|b| b.path.as_str())
            .or(self.launch.log_file.as_deref())
            .or_else(|| self.file_load.as_ref().map(|f| f.path.as_str()))
    }

    /// View-only log file terminal (no shell / no stdin).
    pub fn is_file_session(&self) -> bool {
        self.file_load.is_some() || self.file_backed.is_some() || self.launch.log_file.is_some()
    }

    /// Basename for the pinned primary tab when this is a file session.
    pub fn primary_tab_name(&self) -> String {
        if let Some(path) = self.file_session_path() {
            return Path::new(path)
                .file_name()
                .and_then(|n| n.to_str())
                .filter(|s| !s.is_empty())
                .unwrap_or(path)
                .to_string();
        }
        TERMINAL_TAB_NAME.to_string()
    }

    /// Keep index-0 tab named for the session kind; clear filters on that tab.
    pub fn sync_primary_tab_identity(&mut self) {
        let name = self.primary_tab_name();
        let is_file = self.is_file_session();
        if let Some(tab) = self.views.first_mut() {
            if tab.name != name {
                tab.name = name;
            }
            if !tab.filters().is_empty() {
                tab.clear_filters();
            }
            if is_file {
                tab.auto_follow = false;
            }
        }
    }

    /// Disable Follow on every view (file sessions).
    pub fn disable_follow_all_views(&mut self) {
        for view in &mut self.views {
            view.auto_follow = false;
        }
    }

    // Callers never hold zero sessions (terminal_close refuses), but stay
    // total rather than index-panic if that invariant ever regresses.
    pub fn active_view(&self) -> &LogView {
        let idx = self.clamped_active_idx();
        self.views
            .get(idx)
            .expect("TerminalState always holds at least one view")
    }

    pub fn active_view_mut(&mut self) -> &mut LogView {
        let idx = self.clamped_active_idx();
        let len = self.views.len();
        if len == 0 {
            self.views
                .push(LogView::from_runtime(TERMINAL_TAB_NAME, Vec::new()));
        }
        let idx = idx.min(self.views.len() - 1);
        &mut self.views[idx]
    }

    fn clamped_active_idx(&self) -> usize {
        debug_assert!(
            !self.views.is_empty(),
            "TerminalState always holds at least one view"
        );
        self.active_view.min(self.views.len().saturating_sub(1))
    }

    pub fn reset_viewport(&mut self) {
        self.scroll_offset_y = 0.0;
        self.scroll_x = 0.0;
        self.selection = None;
        self.scroll_to_row = None;
        self.pending_file_window = None;
    }

    pub fn ensure_terminal_tab_view(&mut self, _runtime: &RuntimeConfig) {
        if self.views.is_empty() {
            let name = self.primary_tab_name();
            self.views = vec![LogView::from_runtime(&name, Vec::new())];
            self.active_view = 0;
            if self.is_file_session() {
                self.disable_follow_all_views();
            }
            return;
        }
        self.sync_primary_tab_identity();
        self.active_view = self.active_view.min(self.views.len() - 1);
    }
}

pub fn next_terminal_id(_existing: &[TerminalState]) -> String {
    format!("terminal-{}", crate::core::types::unique_time_suffix())
}

/// Home directory for cwd fallbacks and `~` labels (issue #69). Windows
/// normally has no `HOME`; `USERPROFILE` is the per-user root there.
fn home_env() -> Option<String> {
    if cfg!(windows) {
        std::env::var("USERPROFILE")
            .ok()
            .or_else(|| std::env::var("HOME").ok())
    } else {
        std::env::var("HOME").ok()
    }
}

fn is_home_path(trimmed: &str, home: &str) -> bool {
    trimmed == home || trimmed == format!("{home}/") || trimmed == format!("{home}\\")
}

pub fn resolve_initial_cwd(launch: &LaunchConfig) -> String {
    if let Some(cwd) = launch.cwd.as_deref().filter(|s| !s.is_empty()) {
        return cwd.to_string();
    }
    std::env::current_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .or_else(home_env)
        .unwrap_or_else(|| ".".to_string())
}

pub fn cwd_label(cwd: &str) -> String {
    let trimmed = cwd.trim();
    if trimmed.is_empty() || trimmed == "." {
        return ".".to_string();
    }
    if let Some(home) = home_env() {
        if is_home_path(trimmed, &home) {
            return "~".to_string();
        }
    }
    // Terminal cwd can arrive with Windows separators even on POSIX hosts
    // (WSL, remote shells), so split on either — same rule as is_home_path.
    let base = trimmed.trim_end_matches(['/', '\\']);
    let label = match base.rfind(['/', '\\']) {
        Some(i) => &base[i + 1..],
        None => base,
    };
    if label.is_empty() {
        trimmed.to_string()
    } else {
        label.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_created_in_same_millisecond_are_distinct() {
        // Issue #76: millisecond-only ids collided for fast consecutive creates.
        let mut seen = std::collections::HashSet::new();
        for _ in 0..500 {
            assert!(seen.insert(next_terminal_id(&[])));
        }
        let projects = vec![crate::core::types::ProjectConfig {
            id: "p".into(),
            name: "p".into(),
            default_cwd: None,
            path_hint: None,
            active_program: 0,
            programs: vec![],
        }];
        let mut pseen = std::collections::HashSet::new();
        for _ in 0..500 {
            assert!(pseen.insert(crate::core::types::next_project_id(&projects)));
            assert!(pseen.insert(crate::core::types::next_program_id(&[])));
        }
    }

    #[test]
    fn home_path_matches_with_both_separators() {
        // Issue #69: `~` must recognize USERPROFILE-style paths with either
        // separator and reject sibling directories.
        let home = if cfg!(windows) {
            r"C:\Users\tester"
        } else {
            "/home/tester"
        };
        assert!(is_home_path(home, home));
        assert!(is_home_path(&format!("{home}/"), home));
        assert!(is_home_path(&format!("{home}\\"), home));
        assert!(!is_home_path(&format!("{home}\\projects"), home));
        assert!(!is_home_path("/tmp", home));
    }

    #[test]
    fn cwd_label_shows_last_component_outside_home() {
        assert_eq!(cwd_label("C:\\projects\\NoViewLog"), "NoViewLog");
        assert_eq!(cwd_label("/var/log"), "log");
        assert_eq!(cwd_label("."), ".");
        assert_eq!(cwd_label("  "), ".");
    }
}
