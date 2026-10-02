use std::collections::{HashMap, VecDeque};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::core::config::{
    build_runtime_config, load_bundled_config, load_config_from_yaml, load_preset,
    load_projects_store, load_user_config, missing_default_preset_warning, save_user_config,
};
use crate::core::formats::{get_builtin_format, merge_formats};
use crate::core::parser::{reparse_lines, RecordParser};
#[cfg(test)]
use crate::core::types::TabConfig;
use crate::core::types::{
    clamp_max_scrollback_lines, clamp_viewport_font_size, compile_filter_checked, next_filter_id,
    AppConfig, FilterRule, FilterType, LaunchConfig, LogFormat, PresetConfig, ProjectsStore,
    DEFAULT_MAX_SCROLLBACK_LINES,
};
use crate::core::visible::SearchPattern;
use crate::file_index::{PREFETCH_RAW_LINES, WINDOW_RAW_LINES};
use crate::file_load::FILE_VIEW_WINDOW_LINES;
use crate::log_view::LogView;
use crate::pty::{PtyActivityWake, PtyEvent, PtyManager};
use crate::spawn_resolve::{resolve_interactive_shell, resolve_process_launch};
use crate::spawn_resolver::SpawnResolver;
use crate::terminal_state::{next_terminal_id, PendingFileWindow, TerminalState, MAX_CLOSED_TABS};
use crate::viewport::ViewportRenderer;
use crate::viewport_layout::{
    build_visual_lines, content_width, count_visual_rows, max_cols, max_scroll_x, pos_at_pixel,
    record_selection_at, selection_plain_text, word_selection_at, TextSelection, VisualRowIndex,
    LEFT_PAD,
};
use portable_pty::PtySize;

/// Default / legacy alias for the scrollback retention cap (records ≈ lines).
pub const MAX_RECORDS: usize = DEFAULT_MAX_SCROLLBACK_LINES;
const PENDING_IDLE_FLUSH: Duration = Duration::from_millis(120);
/// Terminal tab block caret blink half-period (~classic terminal rate).
pub const CARET_BLINK_PERIOD: Duration = Duration::from_millis(530);

/// Empty Terminal-tab hint when the process session is stopped (ASCII only).
pub const EMPTY_TERMINAL_TAB_STOPPED: &str =
    "Type to open a shell — or use Start for the saved command";
/// Empty filter-tab hint when the process session is stopped (ASCII only).
/// Start is the TERMINALS row play control — not on the filter tab itself.
pub const EMPTY_FILTER_TAB_STOPPED: &str = "Session stopped — use Start on the TERMINALS row";
/// Minimum PTY/emulator column width.
///
/// Soft-wrap is display-only and always uses the real viewport width. The PTY
/// must stay *at least* this wide (and at least the viewport) so child tools
/// that honour `COLUMNS` do not hard-break lines at the viewport edge — that
/// made Wrap ON/OFF look identical (nothing left for soft-wrap / H-scroll).
const MIN_PTY_COLS: u16 = 500;
/// Prefetch an adjacent file chunk when scroll is within this many pixels of a window edge.
pub(crate) const PREFETCH_SCROLL_PX: f32 = 120.0;
/// Bound pending PTY `Bytes` events (~4 KB each) so the reader blocks under flood.
/// 384 × 4 KB ≈ 1.5 MB of queued output before kernel backpressure stalls the writer.
pub(crate) const PTY_QUEUE_CAPACITY: usize = 384;
/// Max PTY bytes fed through VTE / scrollback on a single UI tick.
/// Kept below ~512 KB so Follow paints stay smooth while still draining floods promptly.
pub(crate) const PTY_INGEST_BYTES_PER_TICK: usize = 256 * 1024;
/// Expanded ingest budget for a tick that starts with held-back PTY work
/// (issue #126): the previous tick hit the base budget, so the reader queue is
/// backing up and drain mode must outpace the writer to avoid the ~8 MB/s
/// ceiling imposed by the base budget at 30 Hz.
pub(crate) const PTY_INGEST_DRAIN_BYTES_PER_TICK: usize = 2 * 1024 * 1024;
/// Hard wall-clock guard for one poll_pty call (issue #126): even in drain
/// mode the UI thread must never spend more than this inside ingest.
pub(crate) const PTY_INGEST_TIME_BUDGET: Duration = Duration::from_millis(15);
/// Minimum time between Viewport paint dirty marks under continuous PTY flood.
/// Matches host `TICK_FAST` (~30 Hz) so Follow updates at display cadence, not per ingest chunk.
pub(crate) const VIEWPORT_PAINT_MIN_INTERVAL: Duration = Duration::from_millis(33);
/// Quiet period after the last projects/config change before the deferred
/// write lands (issue #62). One fsync burst-write per gesture storm, not per
/// tab switch / filter toggle / zoom notch.
const PERSIST_DEBOUNCE: Duration = Duration::from_millis(750);
/// Cap for the exponential retry backoff after consecutive persist failures
/// (issue #237): retries keep going quietly instead of spamming status events.
const PERSIST_RETRY_MAX: Duration = Duration::from_secs(30);

mod caret;
mod commands;
mod events;
mod file_session;
mod filters;
mod ingest;
mod launch;
mod persist;
mod projects;
mod render;
mod scroll_selection;
mod stats;
mod tabs;
mod terminal_lifecycle;
#[cfg(test)]
mod tests;

pub(crate) use file_session::FileIoDone;
#[cfg(test)]
mod test_api;

pub use commands::Command;
pub use events::{
    parse_engine_event, EngineEvent, StatsProject, StatsSnapshot, StatsTab, StatsTerminal,
};

use terminal_lifecycle::StartAction;

pub struct Engine {
    pub(crate) terminals: Vec<TerminalState>,
    pub(crate) active_terminal: usize,
    pub(crate) config: AppConfig,
    /// Persisted Projects / Programs (`~/.config/noviewlog/projects.yaml`;
    /// Windows: `%USERPROFILE%\.config\noviewlog\projects.yaml`).
    pub(crate) projects: ProjectsStore,
    /// Index into `projects.projects` when a Project is open; `None` = no active Project.
    pub(crate) active_project: Option<usize>,
    pub(crate) preset_name: String,
    pub(crate) format_id: String,
    pub(crate) formats: HashMap<String, LogFormat>,
    pub(crate) ptys: HashMap<String, PtyManager>,
    pub(crate) pty_rx: Receiver<PtyEvent>,
    pub(crate) pty_tx: SyncSender<PtyEvent>,
    /// Host callback when PTY bytes/exit are posted (coalesced wake).
    pub(crate) pty_activity_wake: Option<PtyActivityWake>,
    /// Leftover `Bytes`/`Exit` held after a budgeted `poll_pty` (cannot push back to mpsc).
    pub(crate) pty_hold: Option<PtyEvent>,
    /// Set when `poll_pty` hits the ingest budget with more work left.
    /// Host must schedule another tick via its timer — do **not** wake mid-tick
    /// (that caused a HOST_TICK busy-loop under `cat` floods).
    pub(crate) pty_drain_pending: bool,
    pub(crate) status_message: String,
    pub(crate) events: VecDeque<String>,
    pub(crate) viewport_width: u32,
    pub(crate) viewport_height: u32,
    pub(crate) renderer: ViewportRenderer,
    pub(crate) last_stats_at: Option<Instant>,
    pub(crate) viewport_dirty: bool,
    /// Last successful Viewport rasterize (host paint). Used to throttle dirty under flood.
    pub(crate) last_viewport_paint_at: Option<Instant>,
    /// Last `poll_pty` (ingest). Host must not busy-wake the reader within the paint interval.
    pub(crate) last_pty_poll_at: Option<Instant>,
    /// When true, the first tick auto-starts the active program.
    /// CLI launch only — never set for Project open / last-Project restore.
    pub(crate) auto_start_launch: bool,
    /// Terminal tab block-caret blink: visible when true; toggled on [`CARET_BLINK_PERIOD`].
    pub(crate) caret_blink_on: bool,
    pub(crate) caret_blink_at: Instant,
    /// Host viewport has keyboard focus — caret only blinks when true.
    pub(crate) viewport_focused: bool,
    /// FILTERS draft preview (UI-global): pattern text + compiled highlight.
    pub(crate) filter_draft_query: String,
    pub(crate) filter_draft_regex: bool,
    pub(crate) filter_draft_pattern: Option<SearchPattern>,
    /// Debounced persistence (issue #62): `projects.yaml` changed since last flush.
    pub(crate) projects_dirty: bool,
    /// Debounced persistence (issue #62): `config.yaml` changed since last flush.
    pub(crate) config_dirty: bool,
    /// Last persistence-relevant change; the deferred write lands after
    /// [`PERSIST_DEBOUNCE`] of quiet (or immediately at engine drop).
    pub(crate) persist_changed_at: Option<Instant>,
    /// Delay before the next persist retry; grows ×4 per consecutive failure
    /// up to [`PERSIST_RETRY_MAX`] so a permanently failing save (read-only
    /// config dir, full disk) does not push a status event every debounce
    /// period (issue #237). Reset on a successful save.
    pub(crate) persist_retry_delay: Duration,
    /// True once the current projects-store persist failure streak has
    /// surfaced a status event; later retries stay silent until that store
    /// saves successfully (issue #237, per-store tracking per issue #253).
    pub(crate) projects_failure_announced: bool,
    /// Same as [`Self::projects_failure_announced`] for `config.yaml`.
    pub(crate) config_failure_announced: bool,
    /// When true (tests only), persist saves report failure (issue #237 tests).
    #[cfg(test)]
    pub(crate) persist_fail_saves: bool,
    /// When true (tests only), do not write `projects.yaml` / `config.yaml`.
    #[cfg(test)]
    pub(crate) skip_projects_persist: bool,
    /// Set when the session config came from `--config` (issue #110): the
    /// debounced config flush is disabled so the user's config.yaml survives.
    pub(crate) config_persist_disabled: bool,
    /// Background spawn resolution + cache (issue #59): Start/Restart never
    /// probes PATH / the registry on the UI thread.
    pub(crate) spawn_resolver: SpawnResolver,
    /// Prewarm dedup: launch/shell keys already kicked for background
    /// resolution (hashed launch inputs + terminal id + shell pref).
    pub(crate) spawn_prewarm_seen: std::collections::HashSet<u64>,
    /// Completed background file I/O (window reads, match scans) posted by
    /// worker threads; drained at the head of [`Engine::tick`] (issue #55).
    pub(crate) file_io_done: Arc<std::sync::Mutex<Vec<FileIoDone>>>,
    /// Monotonic request id for background match-window reads (issue #55).
    pub(crate) match_window_req: u64,
    /// Test-only reduced match-offset cap: exercises the truncation path
    /// (issue #150) without generating 2M+ matching lines.
    #[cfg(test)]
    pub(crate) match_scan_cap_override: Option<usize>,
    /// Last external-change sweep over open file sessions (issue #151);
    /// throttled to one stat per session per [`FILE_WATCH_INTERVAL`].
    pub(crate) last_file_watch_at: Option<Instant>,
}

impl Engine {
    pub fn new() -> Self {
        let (pty_tx, pty_rx) = std::sync::mpsc::sync_channel(PTY_QUEUE_CAPACITY);
        let mut config = load_bundled_config();
        let (user_config, startup_warning) = load_user_config();
        let mut startup_status = startup_warning.unwrap_or_default();
        if let Some(user) = user_config {
            config = user;
        }
        config.max_scrollback_lines = clamp_max_scrollback_lines(config.max_scrollback_lines);
        config.viewport_font_size = clamp_viewport_font_size(config.viewport_font_size);
        let max_scrollback = config.max_scrollback_lines;
        let viewport_font_size = config.viewport_font_size;
        let preset_name = config.default_preset.clone();
        // A typo'd default_preset must be observable, not silently yield zero
        // filters (issue #239).
        if let Some(msg) = missing_default_preset_warning(&config) {
            if !startup_status.is_empty() {
                startup_status.push_str("; ");
            }
            startup_status.push_str(&msg);
        }
        let runtime = build_runtime_config(&config, Some(&preset_name));
        let formats = merge_formats(
            &crate::core::config::all_format_presets(&config),
            &HashMap::new(),
        );
        let format_id = runtime.format_id.clone();
        let default_format = formats
            .get(&format_id)
            .cloned()
            .unwrap_or_else(|| get_builtin_format("node-default"));

        let id = next_terminal_id(&[]);
        let terminal = TerminalState::new(
            id,
            LaunchConfig::default(),
            &runtime,
            &default_format,
            max_scrollback,
        );

        let (projects, projects_warning) = load_projects_store();
        if let Some(msg) = projects_warning {
            if !startup_status.is_empty() {
                startup_status.push_str("; ");
            }
            startup_status.push_str(&msg);
        }

        // Boot terminal is replaced by [`Self::finish_startup`] (CLI launch or
        // project restore). No PTY here so a leftover boot-shell Exit cannot
        // steal the session. Project restore leaves Programs stopped.
        let mut engine = Self {
            terminals: vec![terminal],
            active_terminal: 0,
            config,
            projects,
            active_project: None,
            preset_name,
            format_id,
            formats,
            ptys: HashMap::new(),
            pty_rx,
            pty_tx,
            pty_activity_wake: None,
            pty_hold: None,
            pty_drain_pending: false,
            status_message: startup_status.clone(),
            events: VecDeque::new(),
            viewport_width: 800,
            viewport_height: 600,
            renderer: ViewportRenderer::with_font_size(viewport_font_size),
            last_stats_at: None,
            viewport_dirty: true,
            last_viewport_paint_at: None,
            last_pty_poll_at: None,
            auto_start_launch: false,
            caret_blink_on: true,
            caret_blink_at: Instant::now(),
            viewport_focused: false,
            filter_draft_query: String::new(),
            filter_draft_regex: false,
            filter_draft_pattern: None,
            projects_dirty: false,
            config_dirty: false,
            persist_changed_at: None,
            persist_retry_delay: PERSIST_DEBOUNCE,
            projects_failure_announced: false,
            config_failure_announced: false,
            // Unit tests must never touch the developer's real projects/config.
            #[cfg(test)]
            skip_projects_persist: cfg!(test),
            #[cfg(test)]
            persist_fail_saves: false,
            config_persist_disabled: false,
            spawn_resolver: SpawnResolver::new(),
            spawn_prewarm_seen: std::collections::HashSet::new(),
            file_io_done: Arc::new(std::sync::Mutex::new(Vec::new())),
            match_window_req: 0,
            #[cfg(test)]
            match_scan_cap_override: None,
            last_file_watch_at: None,
        };
        // Warm the pwsh probe (PATH + registry scan) off the UI thread so the
        // first `auto` shell spawn never scans synchronously (issue #59).
        #[cfg(windows)]
        crate::spawn_resolve::prewarm_shell_probe();
        if !startup_status.is_empty() {
            engine.push_event(json!({"type":"status","message": startup_status}));
        }
        engine
    }

    pub(crate) fn has_active_terminal(&self) -> bool {
        !self.terminals.is_empty() && self.active_terminal < self.terminals.len()
    }

    pub(crate) fn ensure_valid_state(&mut self) {
        if self.terminals.is_empty() {
            let runtime = build_runtime_config(&self.config, Some(&self.preset_name));
            let format = self.current_format();
            let id = next_terminal_id(&[]);
            self.terminals.push(TerminalState::new(
                id,
                LaunchConfig::default(),
                &runtime,
                &format,
                self.config.max_scrollback_lines,
            ));
            self.active_terminal = 0;
        }
        self.active_terminal = self.active_terminal.min(self.terminals.len() - 1);
        let runtime = build_runtime_config(&self.config, Some(&self.preset_name));
        self.terminals[self.active_terminal].ensure_terminal_tab_view(&runtime);
    }

    pub(crate) fn active_terminal(&self) -> &TerminalState {
        let idx = self
            .active_terminal
            .min(self.terminals.len().saturating_sub(1));
        self.terminals
            .get(idx)
            .expect("active_terminal called with no terminals")
    }

    pub(crate) fn active_terminal_mut(&mut self) -> &mut TerminalState {
        let idx = self
            .active_terminal
            .min(self.terminals.len().saturating_sub(1));
        self.terminals
            .get_mut(idx)
            .expect("active_terminal_mut called with no terminals")
    }

    pub(crate) fn command_needs_active_terminal(cmd: &Command) -> bool {
        !matches!(
            cmd,
            Command::Resize { .. }
                | Command::TerminalAdd
                | Command::TerminalClose { .. }
                | Command::TerminalSwitch { .. }
                | Command::TerminalMove { .. }
                | Command::TerminalRename { .. }
                | Command::TerminalStart { .. }
                | Command::Stop { .. }
                | Command::ProjectOpen { .. }
                | Command::ProjectCreate { .. }
                | Command::ProjectRename { .. }
                | Command::ProjectDelete { .. }
                | Command::ProgramSetLaunch { .. }
                | Command::LoadFile { .. }
                | Command::ReloadFile { .. }
                | Command::SetSettings { .. }
                | Command::SetViewportFontSize { .. }
                | Command::SetViewportFocus { .. }
                | Command::FilterDraftSet { .. }
        )
    }

    pub fn tick(&mut self) {
        self.ensure_valid_state();
        self.flush_persist_if_due();
        self.surface_stdin_errors();
        if !self.has_active_terminal() {
            if self.status_message.is_empty() {
                self.status_message = "No terminal".to_string();
            }
            self.poll_pty();
            self.emit_stats();
            return;
        }
        if self.auto_start_launch {
            let start_action = {
                let terminal = self.active_terminal();
                if terminal.process_started {
                    None
                } else if let Some(path) = terminal.launch.log_file.clone() {
                    Some(StartAction::File(path))
                } else if terminal.launch.command.is_some() {
                    Some(StartAction::Launch)
                } else {
                    None
                }
            };
            if let Some(action) = start_action {
                self.active_terminal_mut().process_started = true;
                match action {
                    StartAction::File(path) => self.start_log_file_load(&path),
                    StartAction::Launch => self.start_launch_process(),
                }
            }
        }
        self.poll_pty();
        self.prewarm_spawns();
        self.apply_file_io_results();
        self.advance_file_load();
        self.advance_file_match_scan();
        self.maybe_prefetch_file_window();
        self.poll_file_changes();
        if self.rebuild_if_needed() {
            self.mark_viewport_dirty();
            if self.has_active_terminal() {
                let idx = self.active_terminal;
                self.snap_follow_scroll_after_ingest(idx);
            }
        }
        self.tick_caret_blink();
        self.emit_stats();
    }

    /// Surface stdin writer-thread failures as status events (issue #58).
    pub(crate) fn surface_stdin_errors(&mut self) {
        let errors: Vec<(String, String)> = self
            .ptys
            .iter_mut()
            .filter_map(|(id, pty)| pty.take_stdin_error().map(|e| (id.clone(), e)))
            .collect();
        for (id, err) in errors {
            self.status_message = format!("stdin ({id}): {err}");
            self.push_event(json!({"type":"status","message": self.status_message}));
        }
    }

    /// Register a host wake when PTY bytes or exit are posted (from the reader thread).
    pub fn set_pty_activity_wake(&mut self, wake: PtyActivityWake) {
        self.pty_activity_wake = Some(wake);
    }

    /// True when budgeted PTY ingest left work for a later tick (`pty_hold` or drain flag).
    pub fn pty_work_pending(&self) -> bool {
        self.pty_drain_pending || self.pty_hold.is_some()
    }

    /// True when the host should keep a fast tick cadence (PTY flood, file load/index,
    /// pending window jump, in-progress whole-file match scan, or a spawn
    /// waiting for background resolution).
    pub fn host_work_pending(&self) -> bool {
        if self.pty_work_pending() {
            return true;
        }
        if !self.has_active_terminal() {
            return false;
        }
        // Any terminal's file load keeps the fast cadence (background FILE
        // sessions must progress without being activated, issue #234) — but
        // a stalled load must not wedge the cadence forever (issue #253).
        if self.terminals.iter().any(|t| t.file_load_active()) {
            return true;
        }
        let terminal = self.active_terminal();
        if terminal.pending_file_window.is_some() {
            return true;
        }
        if terminal.pending_spawn.is_some() {
            return true;
        }
        terminal.active_view().match_scan_pos.is_some()
    }

    /// Kick background resolution for every stopped terminal's launch or
    /// default shell (issue #59), so Start/Restart and the first keystroke hit
    /// a warm [`SpawnResolver`] cache with zero probing on the UI thread.
    /// Runs every tick; each distinct launch is kicked at most once.
    pub(crate) fn prewarm_spawns(&mut self) {
        let mut kicks: Vec<(LaunchConfig, bool)> = Vec::new();
        for term in &self.terminals {
            if term.is_file_session() || term.running || term.pending_spawn.is_some() {
                continue;
            }
            let is_shell = term.launch.command.is_none();
            // The shell default cwd feeds the key (and the kick) without
            // cloning the whole launch config on this every-tick path.
            let effective_cwd = if is_shell {
                Some(term.launch.cwd.as_deref().unwrap_or(&term.cwd))
            } else {
                term.launch.cwd.as_deref()
            };
            let key = spawn_prewarm_key(
                &term.id,
                &term.launch,
                effective_cwd,
                is_shell,
                self.config.shell,
            );
            if self.spawn_prewarm_seen.insert(key) {
                let mut launch = term.launch.clone();
                if is_shell && launch.cwd.is_none() {
                    launch.cwd = Some(term.cwd.clone());
                }
                kicks.push((launch, is_shell));
            }
        }
        if kicks.is_empty() {
            return;
        }
        let resolver = self.spawn_resolver.clone();
        let shell_pref = self.config.shell;
        std::thread::spawn(move || {
            for (launch, is_shell) in kicks {
                // resolve_* is pure except the Auto-shell pwsh probe — the
                // whole point is to run that off the UI thread.
                let resolved = if is_shell {
                    resolve_interactive_shell(&launch, shell_pref)
                } else {
                    resolve_process_launch(&launch)
                };
                let Ok((command, args, cwd)) = resolved else {
                    continue;
                };
                let workdir = terminal_lifecycle::expand_spawn_cwd(cwd);
                let _ = resolver.request_fresh(&command, args, &workdir);
            }
        });
    }

    /// Clear and return whether a drain was requested after the last `poll_pty`.
    pub fn take_pty_drain_pending(&mut self) -> bool {
        let pending = self.pty_drain_pending || self.pty_hold.is_some();
        self.pty_drain_pending = false;
        pending
    }

    /// True when the PTY reader must **not** force an immediate UI tick: flood
    /// work is already queued and the last ingest (or paint) was within the
    /// display interval. The host timer (~33 ms) will poll. Echo (empty of
    /// pending flood) must still wake immediately.
    pub fn defer_pty_reader_wake(&self) -> bool {
        if !self.pty_work_pending() {
            return false;
        }
        let last = match self.last_pty_poll_at.or(self.last_viewport_paint_at) {
            Some(t) => t,
            None => return false,
        };
        last.elapsed() < VIEWPORT_PAINT_MIN_INTERVAL
    }

    /// Reset blink phase (host shows overlay immediately while typing / on focus).
    pub fn reset_caret_blink(&mut self) {
        self.caret_blink_on = true;
        self.caret_blink_at = Instant::now();
    }

    /// TUI hosts: set PTY + ingest geometry directly in **cell units**.
    /// [`Command::Resize`] is viewport-pixel based (bitmap hosts); a TUI
    /// terminal emulator hands us columns/rows, not pixels, and has no font
    /// metrics. Nothing else overrides this — bitmap hosts go through
    /// `render()`, which a TUI host never calls.
    pub fn set_terminal_grid(&mut self, cols: u16, rows: u16) {
        let size = PtySize {
            cols: cols.max(1),
            rows: rows.max(1),
            pixel_width: 8u32
                .saturating_mul(u32::from(cols.max(1)))
                .min(u16::MAX as u32) as u16,
            pixel_height: 16u32
                .saturating_mul(u32::from(rows.max(1)))
                .min(u16::MAX as u32) as u16,
        };
        for pty in self.ptys.values_mut() {
            let _ = pty.set_size(size);
        }
        if self.terminals.is_empty() {
            return;
        }
        let mut any = false;
        for term in &mut self.terminals {
            if term.ingest.size() != (size.cols as usize, size.rows as usize) {
                term.ingest.resize(
                    size.cols as usize,
                    size.rows as usize,
                    &mut term.buffer,
                    &mut term.parser,
                );
                any = true;
            }
        }
        if any {
            self.mark_all_views_dirty();
            self.mark_viewport_dirty();
        }
    }

    /// TUI hosts: true when the active terminal sits at the bottom of its
    /// scroll range (or has nothing to scroll). Wheel-down past this point
    /// should re-enter Follow, matching conventional terminal emulators.
    pub fn at_scroll_bottom(&self) -> bool {
        if !self.has_active_terminal() {
            return true;
        }
        // File/match sessions keep `scroll_offset_y` local to the resident
        // window (issue #195); `stats_scroll_y()` maps it into the same global
        // space as `max_scroll_offset()`, so both sides of the comparison stay
        // comparable for every session kind.
        (self.max_scroll_offset() - self.stats_scroll_y()).abs() < 1.0
    }

    /// TUI hosts: mouse selection in **cell units** — the TUI has no font
    /// metrics, so map cells to viewport pixels here. `col`/`row` are visible
    /// slice coordinates (0 = first visible row), matching
    /// [`Engine::visible_flat_lines`].
    pub fn select_at_cell(&mut self, col: u16, row: u16, extend: bool, click_count: u32) {
        let metrics = self.renderer.metrics();
        let x = f32::from(col) * metrics.cell_width as f32;
        let y = f32::from(row) * metrics.row_stride;
        self.selection_at(x, y, extend, click_count);
    }

    /// Cap for stdin buffered while a spawn is still resolving (issue #59
    /// review). Past the cap the parked spawn is treated as stuck and
    /// cancelled instead of growing the buffer without bound (dead PATH
    /// share / lost worker).
    const PENDING_STDIN_CAP: usize = 64 * 1024;

    /// Apply a typed host → engine command (same guards as JSON path).
    pub fn send_command(&mut self, cmd: Command) -> Result<(), String> {
        self.ensure_valid_state();
        if Self::command_needs_active_terminal(&cmd) && !self.has_active_terminal() {
            return Ok(());
        }
        self.apply_command(cmd)
    }

    /// Thin JSON → [`Command`] → [`Self::send_command`] (FFI / legacy hosts).
    pub fn send_command_json(&mut self, json: &str) -> Result<(), String> {
        let cmd: Command = serde_json::from_str(json).map_err(|e| e.to_string())?;
        self.send_command(cmd)
    }

    pub fn poll_event_json(&mut self) -> Option<String> {
        self.events.pop_front()
    }

    pub fn handle_key(&mut self, bytes: &[u8]) {
        if !self.has_active_terminal() {
            return;
        }
        // Terminal tab: auto-start an interactive shell so typing works without Start.
        // Programs with a saved command stay stopped after exit (no shell spawn).
        if !self.active_terminal().running {
            if self.active_terminal().active_view != 0 {
                return;
            }
            if self.active_terminal().is_file_session() {
                return;
            }
            if self.active_terminal().launch.command.is_some() {
                return;
            }
            self.start_interactive_shell();
            if !self.active_terminal().running {
                return;
            }
        }
        // Typing in the Terminal tab while scrolled up: Follow on and jump to
        // the live prompt (same as a conventional terminal emulator).
        if self.active_terminal().active_view == 0
            && !bytes.is_empty()
            && !self.active_view().auto_follow
        {
            self.scroll_to_end();
        }
        let id = self.active_terminal().id.clone();
        enum WriteOutcome {
            Written,
            Buffered,
            Failed(String),
        }
        let write_result = match self.ptys.get_mut(&id) {
            Some(pty) => match pty.write_bytes(bytes) {
                Ok(()) => WriteOutcome::Written,
                Err(err) => WriteOutcome::Failed(err),
            },
            None => {
                // Spawn still resolving on a worker thread (issue #59):
                // buffer keystrokes until the PTY exists instead of erroring.
                let term = self.active_terminal_mut();
                if term.pending_spawn.is_none() {
                    WriteOutcome::Failed("no pty for terminal".to_string())
                } else if term.pending_stdin.len().saturating_add(bytes.len())
                    > Self::PENDING_STDIN_CAP
                {
                    // A parked spawn that still has not started after this
                    // much input is stuck (dead PATH share, lost worker):
                    // cancel it instead of buffering without bound.
                    term.pending_spawn = None;
                    term.pending_stdin.clear();
                    term.running = false;
                    WriteOutcome::Failed(
                        "spawn is not starting — input buffer full, start cancelled".to_string(),
                    )
                } else {
                    term.pending_stdin.extend_from_slice(bytes);
                    WriteOutcome::Buffered
                }
            }
        };
        match write_result {
            WriteOutcome::Written | WriteOutcome::Buffered => {
                // Keep the caret visible while typing (same as a real terminal).
                self.reset_caret_blink();
            }
            WriteOutcome::Failed(err) => {
                self.push_event(json!({"type":"status","message": format!("stdin: {err}")}));
            }
        }
    }

    pub(crate) fn active_view(&self) -> &LogView {
        self.active_terminal().active_view()
    }

    pub(crate) fn active_view_mut(&mut self) -> &mut LogView {
        self.active_terminal_mut().active_view_mut()
    }

    pub(crate) fn current_format(&self) -> LogFormat {
        self.formats
            .get(&self.format_id)
            .cloned()
            .unwrap_or_else(|| get_builtin_format("node-default"))
    }

    pub(crate) fn push_event(&mut self, value: serde_json::Value) {
        if let Ok(text) = serde_json::to_string(&value) {
            self.events.push_back(text);
        }
    }

    pub(crate) fn rebuild_if_needed(&mut self) -> bool {
        if !self.has_active_terminal() {
            return false;
        }

        // File filter tabs with a match index rebuild via apply_match_window.
        if self.active_terminal().is_file_session() && self.active_view().uses_match_index() {
            if self.active_view().match_scan_pos.is_some() {
                return false;
            }
            if self.active_view().is_flat_lines_dirty() {
                self.apply_match_window();
            }
            let terminal = self.active_terminal_mut();
            let view = terminal.active_view_mut();
            let search_was_dirty = view.is_search_dirty();
            let scroll_row = view.refresh_search_if_dirty();
            if let Some(row) = scroll_row {
                // Search hit is within the match window's flat_lines.
                terminal.scroll_to_row = Some(row);
            }
            return search_was_dirty || scroll_row.is_some();
        }

        if self.paints_live_vt_grid() {
            return false;
        }

        let terminal = self.active_terminal_mut();
        let is_file = terminal.is_file_session();
        let active = terminal.active_view;
        let is_live_terminal_tab = active == 0 && !is_file;
        let records_len = terminal.buffer.records_len();
        let Some(view) = terminal.views.get_mut(active) else {
            return false;
        };
        let before_records = view.flat_lines_record_cursor;
        let before_lines = view.flat_lines.len();
        let search_was_dirty = view.is_search_dirty();
        let rebuild_committed =
            !is_file && (view.is_flat_lines_dirty() || view.flat_lines_record_cursor < records_len);
        if rebuild_committed {
            view.strip_live_overlay();
        }
        // Partial borrow: view + buffer are distinct fields.
        let scroll_row = {
            let TerminalState { views, buffer, .. } = terminal;
            let view = views.get_mut(active).expect("active view");
            view.rebuild(buffer)
        };
        // Live overlay is a snapshot at rebuild time, not a per-ingest patch.
        // Filter tabs: apply once after a committed rebuild so short `uname` is
        // visible, then leave the tail alone on overlay-only PTY frames.
        let overlay_changed = if is_file {
            false
        } else if is_live_terminal_tab {
            if rebuild_committed || terminal.views[0].overlay_len() == 0 {
                let overlay = terminal.ingest.overlay_flat_lines();
                terminal.views[0].set_live_overlay(overlay);
            }
            false
        } else if rebuild_committed {
            let overlay = terminal.ingest.overlay_flat_lines();
            terminal.views[active].set_filtered_live_overlay(overlay)
        } else {
            false
        };
        let view = terminal.views.get_mut(active).expect("active view");
        let changed = search_was_dirty
            || overlay_changed
            || view.flat_lines_record_cursor != before_records
            || view.flat_lines.len() != before_lines;
        if let Some(row) = scroll_row {
            terminal.scroll_to_row = Some(row);
        }
        changed
    }

    pub(crate) fn mark_all_views_dirty(&mut self) {
        let terminal = self.active_terminal_mut();
        for view in &mut terminal.views {
            view.mark_flat_lines_dirty();
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        // App exit: flush any debounced projects/config changes so a quit right
        // after a gesture burst cannot lose them (issue #62).
        self.flush_persist();
        // Events die with the engine — surface a failed final save on stderr
        // so it is at least visible somewhere (issue #109).
        if self.projects_dirty || self.config_dirty {
            eprintln!(
                "noviewlog: WARNING: pending projects/config changes could not be saved at exit"
            );
        }
    }
}

/// Dedup key for [`Engine::prewarm_spawns`]: terminal identity plus everything
/// that changes the future resolution input. Edited launches hash differently
/// and are re-kicked automatically on the next tick.
fn spawn_prewarm_key(
    terminal_id: &str,
    launch: &LaunchConfig,
    effective_cwd: Option<&str>,
    is_shell: bool,
    shell: crate::core::types::ShellPreference,
) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    terminal_id.hash(&mut hasher);
    launch.command.hash(&mut hasher);
    launch.args.hash(&mut hasher);
    effective_cwd.hash(&mut hasher);
    launch.wsl.hash(&mut hasher);
    launch.wsl_distro.hash(&mut hasher);
    is_shell.hash(&mut hasher);
    shell.hash(&mut hasher);
    hasher.finish()
}
