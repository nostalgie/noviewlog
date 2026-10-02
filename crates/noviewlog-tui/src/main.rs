//! NoViewLog TUI host: the existing engine rendered as ANSI text on the
//! alternate screen, so the terminal + filters run inside any ANSI emulator
//! (VS Code / IDEA terminal panels, Windows Terminal, tmux).
//!
//! Interaction model (transparent terminal):
//! - Keys always go to the wrapped shell, like a normal terminal.
//! - Mouse: wheel scrolls our output; drag selects text (copy on release);
//!   tab-bar clicks switch/create filter tabs; clicking a collapsed record
//!   twice expands it.
//! - The only modal thing is the filter input line (Enter applies, Esc
//!   closes). Ctrl+Q quits with a y/N confirm.

mod render;
mod ssh;

use std::io::{self, stdout, Write};
use std::time::{Duration, Instant};

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

use noviewlog_core::core::config::load_user_config;
use noviewlog_core::core::types::{
    FilterType, FlatLine, LaunchConfig, ShellPreference, SshProfile,
};
use noviewlog_core::spawn_resolve::resolve_interactive_shell;
use noviewlog_core::{parse_engine_event, Command, Engine, EngineEvent, StatsSnapshot};

use ssh::SessionChoice;

/// Frame budget: at most one paint per interval (flood-safe rendering).
const FRAME_BUDGET: Duration = Duration::from_millis(16);
/// Double-click window for record expand/collapse.
const DOUBLE_CLICK: Duration = Duration::from_millis(350);

/// Right-click context menu rendered as a text overlay on the grid.
struct Menu {
    /// Screen row/col of the box top-left.
    row: u16,
    col: u16,
    /// Actions for the record under the cursor (if any).
    record_id: Option<u64>,
    /// Whole text of the record's first line (for "copy line").
    line_text: String,
    items: Vec<&'static str>,
}

struct App {
    engine: Engine,
    stats: Option<StatsSnapshot>,
    /// Filter input line focus: while open, keys edit the pattern.
    input_focus: bool,
    filter_buf: String,
    confirm_quit: bool,
    /// Saved SSH profiles (user config `tui_ssh_profiles`).
    profiles: Vec<SshProfile>,
    /// The running (or last) session; `r` reconnects with the same argv.
    session: Option<SessionChoice>,
    /// Session child exited: banner text; keys are gated (R/N/Q), never
    /// passed through to any shell (design D6).
    exited: Option<String>,
    /// SSH profile list overlay open (mouse-driven; Esc/outside = local).
    connect_open: bool,
    /// Keyboard selection index into the connect overlay items.
    connect_sel: usize,
    /// Label of the running (or last) SSH session, shown in the status bar.
    session_label: Option<String>,
    cols: u16,
    rows: u16,
    /// Cursor into the visible slice (keyboard record toggle); None = none.
    cursor: Option<usize>,
    /// Hit-testing for mouse clicks: (start_col, len, tab_index) per tab-bar cell.
    tab_spans: Vec<(u16, u16, usize)>,
    /// Column span of the "+" (new filter tab) cell in the tab bar.
    tab_add_span: Option<(u16, u16)>,
    /// record_id painted per content row (row 0 = first content row).
    row_records: Vec<Option<u64>>,
    /// Text selection drag state in content-cell coords (row, col), row 0 =
    /// first visible content row, col 0 = after the 2-char prefix.
    sel_anchor: Option<(usize, usize)>,
    sel_current: Option<(usize, usize)>,
    /// Last left-click (cell + time) for double-click detection.
    last_click: Option<((u16, u16), Instant)>,
    /// Visible flat lines snapshot (selection text source, menu context).
    visible: Vec<FlatLine>,
    /// Open context menu, if any.
    menu: Option<Menu>,
    /// Whether the previous frame had the menu open.
    menu_was_open: bool,
    /// Whether the previous frame had the connect overlay open.
    connect_was_open: bool,
    /// UI state changed (selection/menu/input) — repaint next frame even if
    /// the engine considers its viewport clean.
    ui_dirty: bool,
    /// Rendered byte buffers of the previous frame (diff painting).
    frame_prev: Vec<Vec<u8>>,
    last_paint: Option<Instant>,
}

impl App {
    fn new(cols: u16, rows: u16, profiles: Vec<SshProfile>) -> Result<Self, String> {
        let mut app = Self {
            engine: Engine::new(),
            stats: None,
            input_focus: false,
            filter_buf: String::new(),
            confirm_quit: false,
            profiles,
            session: None,
            exited: None,
            connect_open: false,
            connect_sel: 0,
            session_label: None,
            cols,
            rows,
            cursor: None,
            tab_spans: Vec::new(),
            tab_add_span: None,
            row_records: Vec::new(),
            sel_anchor: None,
            sel_current: None,
            last_click: None,
            visible: Vec::new(),
            menu: None,
            menu_was_open: false,
            connect_was_open: false,
            ui_dirty: true,
            frame_prev: Vec::new(),
            last_paint: None,
        };
        app.engine.finish_startup(LaunchConfig::default());
        // Whole flat lines are the TUI render unit (no font metrics here).
        app.engine
            .send_command(Command::SetWrapLines { wrap: false })?;
        Ok(app)
    }

    /// Start a session (local shell or ssh) on the engine PTY. Called from
    /// startup, the connect overlay, and `r` reconnect.
    fn start_session(&mut self, choice: SessionChoice) -> Result<(), String> {
        match choice.clone() {
            SessionChoice::Local => {
                let (shell, args, cwd) =
                    resolve_interactive_shell(&LaunchConfig::default(), ShellPreference::Auto)?;
                self.engine.send_command(Command::Start {
                    command: shell,
                    args,
                    cwd,
                })?;
                self.session_label = None;
            }
            SessionChoice::Ssh { label, argv } => {
                // No panic path on an empty argv (would currently be
                // unreachable, but build errors, not panics, surface it).
                let (command, rest) = argv
                    .split_first()
                    .ok_or_else(|| format!("ssh profile `{label}` has an empty command"))?;
                let args = rest.to_vec();
                self.engine.send_command(Command::Start {
                    command: command.clone(),
                    args,
                    cwd: None,
                })?;
                self.session_label = Some(label); // shown in the status bar
            }
        }
        self.session = Some(choice);
        self.exited = None;
        self.connect_open = false;
        Ok(())
    }

    fn cmd(&mut self, c: Command) {
        if let Err(e) = self.engine.send_command(c) {
            // Surface engine rejections on the status line instead of crashing.
            if let Some(s) = self.stats.as_mut() {
                s.status = format!("cmd error: {e}");
            }
        }
    }

    /// Route a terminal paste: into the filter buffer (capped) while the
    /// input line is focused; otherwise to the wrapped shell as plain stdin.
    /// A dead session has no shell to receive it — drop silently instead of
    /// surfacing a cmd error banner (#241).
    fn handle_paste(&mut self, text: &str) {
        // Gate order mirrors handle_key: a dead session drops the paste
        // before the filter buffer is touched (#254).
        if self.exited.is_some() {
            return;
        }
        if self.input_focus {
            paste_append(&mut self.filter_buf, text);
        } else {
            self.cmd(Command::Stdin {
                text: String::new(),
                bytes: Some(text.as_bytes().to_vec()),
            });
        }
    }

    fn sync_geometry(&mut self) {
        // Cell units, not pixels: Command::Resize is bitmap-host pixel space.
        self.engine
            .set_terminal_grid(self.cols.max(1), self.content_rows().max(1));
    }

    fn content_rows(&self) -> u16 {
        self.rows.saturating_sub(3)
    }

    fn drain_events(&mut self) {
        while let Some(json) = self.engine.poll_event_json() {
            match parse_engine_event(&json) {
                Some(EngineEvent::Stats(s)) => self.stats = Some(s),
                Some(EngineEvent::Exit { code, message }) => {
                    let what = match self.session.as_ref() {
                        Some(SessionChoice::Ssh { label, .. }) => format!("ssh {label}"),
                        Some(SessionChoice::Local) => "shell".to_string(),
                        None => "session".to_string(),
                    };
                    let detail = if message.is_empty() {
                        format!("{what} exited (code {code})")
                    } else {
                        format!("{what} exited (code {code}): {message}")
                    };
                    self.exited = Some(detail);
                    self.ui_dirty = true;
                }
                Some(EngineEvent::Status { message }) => {
                    if let Some(s) = self.stats.as_mut() {
                        s.status = message;
                    }
                }
                Some(EngineEvent::Unknown) | None => {}
            }
        }
    }

    fn leave_follow(&mut self) {
        if self.stats.as_ref().is_some_and(|s| s.auto_follow) {
            self.cmd(Command::SetFollow { follow: false });
        }
    }

    /// Index of the active tab per the last stats snapshot (0 when unknown).
    fn active_tab_index(&self) -> usize {
        self.stats.as_ref().map_or(0, |s| s.active_tab)
    }

    /// Number of tabs (Terminal + filter tabs); at least the Terminal tab.
    fn tab_count(&self) -> usize {
        self.stats.as_ref().map_or(1, |s| s.tabs.len().max(1))
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<()> {
        // Any key may alter UI state (input line, confirm); repaint cheaply.
        self.ui_dirty = true;
        // Act on Press only; some hosts synthesize Release/Repeat events.
        if key.kind != KeyEventKind::Press {
            return Some(());
        }
        // Quit: Ctrl+Q with y/N confirm (typed text still reaches the shell).
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q'))
        {
            if self.confirm_quit {
                return None;
            }
            self.confirm_quit = true;
            return Some(());
        }
        if self.confirm_quit {
            self.confirm_quit = false;
            return Some(());
        }
        // Connect overlay: Up/Down move the selection (clamped), Enter
        // activates it, Esc dismisses (local shell when nothing runs yet,
        // back to the banner otherwise).
        if self.connect_open {
            let items = self.connect_items().len();
            match key.code {
                KeyCode::Esc => {
                    self.connect_open = false;
                    if self.session.is_none() {
                        let _ = self.start_session(SessionChoice::Local);
                    }
                }
                KeyCode::Up => self.connect_sel = connect_sel_move(self.connect_sel, -1, items),
                KeyCode::Down => self.connect_sel = connect_sel_move(self.connect_sel, 1, items),
                KeyCode::Enter => {
                    let choice = self.connect_choice(self.connect_sel);
                    self.connect_open = false;
                    match choice {
                        Some(c) => {
                            let _ = self.start_session(c);
                        }
                        None if self.session.is_none() => {
                            let _ = self.start_session(SessionChoice::Local);
                        }
                        _ => {}
                    }
                }
                _ => {}
            }
            return Some(());
        }
        // Session exited: keys are session controls only — nothing reaches a
        // shell until the user explicitly reconnects or starts a local one
        // (a disconnected SSH session must not leak keystrokes locally).
        if self.exited.is_some() {
            match key.code {
                KeyCode::Char('r' | 'R') => {
                    if let Some(choice) = self.session.clone() {
                        let _ = self.start_session(choice);
                    }
                }
                KeyCode::Char('n' | 'N') => {
                    self.connect_open = true;
                    self.connect_sel = 0;
                    self.ui_dirty = true;
                }
                KeyCode::Char('q' | 'Q') => return None,
                _ => {}
            }
            return Some(());
        }
        // The filter input line is the only modal element.
        if self.input_focus {
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Enter => {
                    let pattern = std::mem::take(&mut self.filter_buf);
                    if !pattern.is_empty() {
                        self.cmd(Command::FilterAdd {
                            filter_type: FilterType::Include,
                            pattern,
                            regex: false,
                        });
                    }
                    self.input_focus = false;
                }
                KeyCode::Esc => {
                    self.filter_buf.clear();
                    self.input_focus = false;
                }
                KeyCode::Backspace => {
                    self.filter_buf.pop();
                }
                // Ignore Ctrl-chords in the input line (Ctrl+A/E/U are edit
                // motions, not text) instead of typing the literal letter.
                KeyCode::Char(c) if !ctrl => self.filter_buf.push(c),
                _ => {}
            }
            return Some(());
        }
        // Chord shortcuts (tab/filter management) while the input line is
        // not focused. Ctrl+F opens the filter input (same state the "+"
        // tab-bar click sets).
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(key.code, KeyCode::Char('f' | 'F')) {
            self.filter_buf.clear();
            self.input_focus = true;
            return Some(());
        }
        if let Some(command) = key_command(&key, self.tab_count(), self.active_tab_index()) {
            self.cmd(command);
            return Some(());
        }
        // Ctrl+Shift+C copies the current selection (terminal convention);
        // consumed here so it never reaches the shell as a Ctrl+C.
        if ctrl && key.modifiers.contains(KeyModifiers::SHIFT) {
            if let (Some(a), Some(b)) = (self.sel_anchor, self.sel_current) {
                if a != b {
                    let text = self.selected_text(a.min(b), a.max(b));
                    if !text.is_empty() {
                        copy_to_clipboard(&text);
                    }
                }
            }
            return Some(());
        }
        // Transparent terminal: everything goes to the wrapped shell.
        let bytes = shell_key_bytes(&key);
        if !bytes.is_empty() {
            self.cmd(Command::Stdin {
                text: String::new(),
                bytes: Some(bytes),
            });
        }
        Some(())
    }

    /// Items of the connect overlay: profiles then the local shell entry.
    fn connect_items(&self) -> Vec<String> {
        let mut items: Vec<String> = self
            .profiles
            .iter()
            .map(|p| format!("ssh {}", p.target))
            .collect();
        items.push("local shell".to_string());
        items
    }

    /// Session choice for connect overlay item `idx` (profiles then the
    /// local shell entry); None for an out-of-range index.
    fn connect_choice(&self, idx: usize) -> Option<SessionChoice> {
        if idx < self.profiles.len() {
            let p = &self.profiles[idx];
            let label = format!("{} ({})", p.name, p.target);
            Some(SessionChoice::Ssh {
                label,
                argv: ssh::argv_for_profile(p),
            })
        } else if idx < self.connect_items().len() {
            Some(SessionChoice::Local)
        } else {
            None
        }
    }

    /// Deterministic overlay geometry: centered box, 40 cols wide.
    /// (col, row, item_count) — same math as the painter in render.rs.
    fn connect_geo(&self) -> (u16, u16, usize) {
        let items = self.connect_items().len();
        let col = self.cols.saturating_sub(40) / 2;
        let row = self.rows.saturating_sub(items as u16 + 2) / 2;
        (col, row, items)
    }

    /// Inner width of the connect overlay box at left column `col` — the
    /// same clamp the painter uses, so hit-testing only maps to visibly
    /// drawn cells on narrow terminals (#241).
    pub(crate) fn connect_box_width(col: u16, cols: u16) -> u16 {
        38u16.min(cols.saturating_sub(col).saturating_sub(2))
    }

    /// Drawn width of the context menu box at left column `col` — the same
    /// clamp the painter uses, so the hit region only covers visibly drawn
    /// cells at any terminal width (#254).
    pub(crate) fn menu_width(col: u16, cols: u16) -> u16 {
        24u16.min(cols.saturating_sub(col))
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        // Selection/menu highlight must follow the pointer in real time.
        self.ui_dirty = true;
        // Wheel and right-click are inert over the connect overlay or the
        // exited-session banner: nothing scrollable is shown, and a right
        // click on the banner rows would misread row 0 as the tab bar.
        let gated = self.connect_open || self.exited.is_some();
        match m.kind {
            MouseEventKind::ScrollUp if !gated => {
                self.leave_follow();
                self.cursor = None;
                self.cmd(Command::ScrollLines { delta: -3 });
            }
            MouseEventKind::ScrollDown if !gated => {
                // Windows Terminal behavior: reaching the bottom re-enters
                // follow so new output pins the view again.
                if self.engine.at_scroll_bottom() {
                    self.cmd(Command::SetFollow { follow: true });
                    self.cursor = None;
                } else {
                    self.leave_follow();
                    self.cursor = None;
                    self.cmd(Command::ScrollLines { delta: 3 });
                }
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Middle) => {
                // Middle-click on a tab-bar tab closes it (tab 0 pinned by
                // the engine, and TabClose on 0 is rejected there anyway).
                if m.row == 0 {
                    if let Some(&(_, _, index)) = self
                        .tab_spans
                        .iter()
                        .find(|&&(s, l, _)| m.column >= s && m.column < s.saturating_add(l))
                    {
                        if index != 0 {
                            self.cmd(Command::TabClose { index });
                        }
                    }
                }
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Right) if !gated => {
                if self.menu.is_some() {
                    self.menu = None;
                    return;
                }
                // Row 0 is the tab bar: no record context, no menu.
                if m.row == 0 {
                    return;
                }
                // Open a text menu over the grid at the click point.
                let content_row = m.row.saturating_sub(1) as usize;
                let collapsed = self.visible.get(content_row).is_some_and(|l| l.collapsed);
                let items = menu_items(collapsed);
                // The menu draws items+2 rows (top/bottom border); clamp so
                // the whole box fits — derived from the item count, not a
                // hardcoded row budget (P3-12).
                let menu_rows = items.len() as u16 + 2;
                let row = m.row.min(self.rows.saturating_sub(menu_rows));
                let col = m.column.min(self.cols.saturating_sub(26));
                let content_row = m.row.saturating_sub(1) as usize;
                let ctx = self.row_records.get(content_row).copied().flatten();
                let line_text = self
                    .visible
                    .get(content_row)
                    .map(|l| {
                        l.segments
                            .iter()
                            .map(|s| s.text.as_str())
                            .collect::<String>()
                            .trim()
                            .to_string()
                    })
                    .unwrap_or_default();
                self.menu = Some(Menu {
                    row,
                    col,
                    record_id: ctx,
                    line_text,
                    items,
                });
            }
            MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                // Connect overlay: click on an item starts that session;
                // Esc/outside falls back to the local shell.
                if self.connect_open {
                    // Box geometry matches the painter: title row at orow+1,
                    // items start at orow+2.
                    let (ocol, orow, items) = self.connect_geo();
                    let box_cols = Self::connect_box_width(ocol, self.cols);
                    let idx = if m.column >= ocol
                        && m.column < ocol.saturating_add(box_cols).saturating_add(2)
                        && m.row >= orow + 2
                        && ((m.row - orow - 2) as usize) < items
                    {
                        Some((m.row - orow - 2) as usize)
                    } else {
                        None
                    };
                    self.connect_sel = idx.unwrap_or(0);
                    self.connect_open = false;
                    let choice = self.connect_choice(idx.unwrap_or(usize::MAX));
                    match choice {
                        Some(c) => {
                            let _ = self.start_session(c);
                        }
                        None if self.session.is_none() => {
                            // Dismissed with no session yet: default to local.
                            let _ = self.start_session(SessionChoice::Local);
                        }
                        _ => {}
                    }
                    return;
                }
                if let Some(menu) = &self.menu {
                    let items = menu.items.clone();
                    let record_id = menu.record_id;
                    let line_text = menu.line_text.clone();
                    let (mrow, mcol) = (menu.row, menu.col);
                    self.menu = None;
                    // Hit-test: item 0 = the row below the top border; the
                    // bottom border maps to a no-op index.
                    let in_box = m.column >= mcol
                        && m.column < mcol.saturating_add(Self::menu_width(mcol, self.cols));
                    let idx = if in_box {
                        menu_hit(mrow, m.row, items.len())
                    } else {
                        None
                    };
                    if let Some(idx) = idx {
                        match idx {
                            0 if record_id.is_some() => {
                                self.cmd(Command::RecordCollapseToggle {
                                    record_id: record_id.unwrap(),
                                });
                            }
                            1 => copy_to_clipboard(&line_text),
                            2 if !line_text.is_empty() => {
                                self.cmd(Command::FilterAdd {
                                    filter_type: FilterType::Include,
                                    pattern: line_text,
                                    regex: false,
                                });
                            }
                            3 => self.cmd(Command::FilterClear),
                            _ => {}
                        }
                    }
                    return;
                }
                if m.row == 0 {
                    if let Some(&(_, _, index)) = self
                        .tab_spans
                        .iter()
                        .find(|&&(s, l, _)| m.column >= s && m.column < s.saturating_add(l))
                    {
                        self.cmd(Command::TabSwitch { index });
                    } else if self
                        .tab_add_span
                        .is_some_and(|(s, l)| m.column >= s && m.column < s.saturating_add(l))
                    {
                        self.cmd(Command::TabAdd);
                        self.filter_buf.clear();
                        self.input_focus = true;
                    }
                    return;
                }
                if let Some(cell) = self.cell_at(&m) {
                    // Double-click toggles record expand/collapse; single
                    // clicks start a text selection.
                    let now = Instant::now();
                    let dbl = self.last_click.is_some_and(|((r, c), t)| {
                        (r, c) == (m.row, m.column) && now.duration_since(t) <= DOUBLE_CLICK
                    });
                    self.last_click = Some(((m.row, m.column), now));
                    if dbl {
                        if let Some(Some(id)) = self.row_records.get(cell.0) {
                            self.cmd(Command::RecordCollapseToggle { record_id: *id });
                        }
                        self.sel_anchor = None;
                        self.sel_current = None;
                        return;
                    }
                    self.sel_anchor = Some(cell);
                    self.sel_current = Some(cell);
                } else {
                    // A click outside the content area (tab bar handled above,
                    // input/status rows) dismisses a stale selection instead
                    // of leaving a highlight no drag explains.
                    self.sel_anchor = None;
                    self.sel_current = None;
                }
            }
            MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                if let Some(cell) = self.cell_at(&m) {
                    self.sel_current = Some(cell);
                }
            }
            MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                // ConPTY/WT decode drag motion with an unreliable X (it snaps
                // to end of line), while press/release decode faithfully — so
                // the release point is the authoritative selection end, for
                // both the kept highlight and the copied text.
                if let Some(cell) = self.cell_at(&m) {
                    self.sel_current = Some(cell);
                }
                // Keep the highlight; copy the selected text from the
                // visible slice on our own (no engine coordinate math).
                if let (Some(a), Some(b)) = (self.sel_anchor, self.sel_current) {
                    if a != b {
                        let text = self.selected_text(a.min(b), a.max(b));
                        if !text.is_empty() {
                            copy_to_clipboard(&text);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    /// Text of the selected span across the visible slice (inclusive of the
    /// end cell's column on its last row; missing cells are skipped).
    fn selected_text(&self, a: (usize, usize), b: (usize, usize)) -> String {
        // Empty visible slice: `len().saturating_sub(1)` would saturate to
        // usize::MAX and the first index panics (#199).
        if self.visible.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for row in a.0..=b.0.min(self.visible.len().saturating_sub(1)) {
            let line = &self.visible[row];
            let text: String = line.segments.iter().map(|s| s.text.as_str()).collect();
            let start = if row == a.0 { a.1 } else { 0 };
            let end = if row == b.0 { b.1 } else { usize::MAX };
            let slice = text_in_cells(&text, start, end);
            if !slice.is_empty() {
                out.push_str(&slice);
            }
            if row != b.0 {
                out.push('\n');
            }
        }
        out
    }

    /// Content-cell coords of a mouse event: (row, col), row 0 = first
    /// visible content row, col 0 = after the 2-char prefix. None outside
    /// the content area.
    fn cell_at(&self, m: &MouseEvent) -> Option<(usize, usize)> {
        if m.row == 0 || m.row as usize > self.content_rows() as usize {
            return None;
        }
        Some(((m.row - 1) as usize, m.column.saturating_sub(2) as usize))
    }

    fn paint(&mut self) {
        self.tab_spans.clear();
        self.row_records.clear();
        let content = self.content_rows() as usize;
        let lines = self.engine.visible_flat_lines(content);
        self.visible = lines.clone();
        // Menu open/close transitions get one full repaint (overlay rows are
        // not part of the steady diff layout); same for the connect overlay.
        let full = (self.menu.is_some() != self.menu_was_open)
            || (self.connect_open != self.connect_was_open);
        self.menu_was_open = self.menu.is_some();
        self.connect_was_open = self.connect_open;
        let mut out = stdout();
        let _ = render::frame(&mut out, self, &lines, full);
        // Park the emulator caret where the wrapped shell's caret is, so
        // echoed/editing output lands where the user expects. While the
        // filter input is open the caret stays hidden (input line is ours).
        match self.engine.caret_visible_pos(content) {
            Some((r, c))
                if !self.input_focus
                    && self.exited.is_none()
                    && (r as u16) < self.content_rows() =>
            {
                let x = caret_x(c, self.cols);
                let _ = execute!(
                    out,
                    crossterm::cursor::Show,
                    crossterm::cursor::MoveTo(x, r as u16 + 1)
                );
            }
            _ => {
                let _ = execute!(out, crossterm::cursor::Hide);
            }
        }
        let _ = out.flush();
        self.ui_dirty = false;
        self.last_paint = Some(Instant::now());
    }
}

/// Part of `text` covering display-cell range `[start, end]` (end cell
/// inclusive). Selection coordinates are cells, not char indices: wide CJK
/// glyphs take two cells and zero-width marks take none, so chars are
/// included by the cell their glyph starts at (#254). Pure-ASCII lines keep
/// the exact span the old char-index slice produced.
fn text_in_cells(text: &str, start: usize, end: usize) -> String {
    let mut out = String::new();
    let mut cell = 0usize;
    for ch in text.chars() {
        if cell >= start && cell <= end {
            out.push(ch);
        }
        cell = cell.saturating_add(noviewlog_terminal::terminal::width::char_width(ch));
    }
    out
}

/// Screen x of the engine caret in column `c` (after the 2-char prefix):
/// clamp in usize first, then cast — clamping after `as u16` could truncate
/// to a wrong position instead of the right edge.
fn caret_x(c: usize, cols: u16) -> u16 {
    (c.min(usize::from(cols.saturating_sub(3))) as u16 + 2).min(cols.saturating_sub(1))
}

/// Paste cap for the filter buffer: a multi-megabyte clipboard paste must
/// not balloon the input line (the frame paints the whole buffer).
const FILTER_BUF_CAP: usize = 64 * 1024;

/// Append pasted text to the filter buffer up to [`FILTER_BUF_CAP`],
/// stopping on a char boundary (never a partial UTF-8 tail).
fn paste_append(buf: &mut String, text: &str) {
    for ch in text.chars() {
        if buf.len() + ch.len_utf8() > FILTER_BUF_CAP {
            break;
        }
        buf.push(ch);
    }
}

/// Items of the right-click context menu in draw and click order. The box
/// is `items.len() + 2` rows tall (top/bottom border) — the on-screen row
/// clamp derives from this count so the menu always fits (P3-12). Index 3
/// ("Clear filters") sends `Command::FilterClear` for the active tab.
fn menu_items(collapsed: bool) -> Vec<&'static str> {
    vec![
        if collapsed {
            "Expand record"
        } else {
            "Collapse record"
        },
        "Copy line",
        "Filter include line",
        "Clear filters",
        "Cancel",
    ]
}

/// Index of the menu item under a click at screen `row` for a menu whose box
/// top border is `menu_row` with `items` entries (item 0 is the row right
/// below the border). The bottom border row maps to index `items` (a no-op);
/// anything above or below the box is None.
fn menu_hit(menu_row: u16, row: u16, items: usize) -> Option<usize> {
    if row <= menu_row {
        return None;
    }
    let idx = (row - menu_row - 1) as usize;
    (idx <= items).then_some(idx)
}

/// Move the connect overlay selection by `delta` (clamped to `[0, count)`),
/// staying put on an empty list or an out-of-range start.
fn connect_sel_move(sel: usize, delta: i32, count: usize) -> usize {
    if count == 0 {
        return 0;
    }
    let sel = sel.min(count - 1);
    if delta < 0 {
        sel.saturating_sub(delta.unsigned_abs() as usize)
    } else {
        (sel + delta as usize).min(count - 1)
    }
}

/// Engine command triggered by a chord key when the filter input line is not
/// focused. Pure so the mapping is unit-testable without a terminal.
///
/// - Ctrl+L: clear the filters of the active tab (engine no-ops on the
///   Terminal tab).
/// - Ctrl+W: close the active filter tab (never the pinned Terminal tab 0).
/// - Ctrl+Tab: cycle to the next tab, wrapping.
/// - Alt+1..9: switch to tab N-1 when it exists.
fn key_command(key: &KeyEvent, tab_count: usize, active_tab: usize) -> Option<Command> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    match key.code {
        KeyCode::Char('l' | 'L') if ctrl => Some(Command::FilterClear),
        KeyCode::Char('w' | 'W') if ctrl => {
            (active_tab != 0).then(|| Command::TabClose { index: active_tab })
        }
        KeyCode::Tab if ctrl => (tab_count > 1).then(|| Command::TabSwitch {
            index: (active_tab + 1) % tab_count,
        }),
        KeyCode::Char(c @ '1'..='9') if alt && !ctrl => {
            let index = usize::from(c as u8 - b'1');
            (index < tab_count).then(|| Command::TabSwitch { index })
        }
        _ => None,
    }
}

/// Encode a key event as PTY bytes (POSIX terminal encoding, valid for the
/// wrapped shell on both platforms).
fn shell_key_bytes(key: &KeyEvent) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('c') if ctrl => b"\x03".to_vec(),
        KeyCode::Char('d') if ctrl => b"\x04".to_vec(),
        KeyCode::Char(c) if ctrl => {
            // Ctrl + non-ASCII (e.g. Ctrl+ü) has no control-byte encoding —
            // send nothing instead of garbage ('ü' & 0x1f = FS) (#199).
            let upper = c.to_ascii_uppercase();
            if upper.is_ascii_alphabetic() {
                vec![upper as u8 & 0x1f]
            } else {
                Vec::new()
            }
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Backspace => b"\x7f".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::Esc => b"\x1b".to_vec(),
        KeyCode::Up => b"\x1b[A".to_vec(),
        KeyCode::Down => b"\x1b[B".to_vec(),
        KeyCode::Right => b"\x1b[C".to_vec(),
        KeyCode::Left => b"\x1b[D".to_vec(),
        KeyCode::Home => b"\x1b[H".to_vec(),
        KeyCode::End => b"\x1b[F".to_vec(),
        KeyCode::PageUp => b"\x1b[5~".to_vec(),
        KeyCode::PageDown => b"\x1b[6~".to_vec(),
        KeyCode::Char(c) => {
            let mut buf = [0u8; 4];
            c.encode_utf8(&mut buf).as_bytes().to_vec()
        }
        _ => Vec::new(),
    }
}

/// How long the UI thread waits for the clipboard worker before falling back
/// to OSC 52. Generous against normal contention, short enough that a hang
/// never reaches the user.
const CLIPBOARD_WAIT: Duration = Duration::from_millis(150);

/// Copy to the real clipboard: arboard first (native), OSC 52 fallback
/// (modern terminals sync their clipboard from it).
///
/// The win32 clipboard is a global resource other processes (clipboard
/// managers, the terminal syncing its own clipboard) hold open
/// intermittently; arboard's `OpenClipboard` then retries with sleeps. On
/// the event-loop thread that froze the whole terminal the moment a
/// selection copy ran — so the native attempt runs on a throwaway thread
/// and the UI thread only waits briefly before falling back to OSC 52.
fn copy_to_clipboard(text: &str) {
    let mut out = stdout();
    copy_with_fallback(&mut out, text, CLIPBOARD_WAIT, spawn_arboard_worker);
}

/// Spawn the native clipboard attempt on a throwaway thread; the receiver
/// yields `true` on success, `false` on failure.
fn spawn_arboard_worker(text: &str) -> std::sync::mpsc::Receiver<bool> {
    let (tx, rx) = std::sync::mpsc::channel();
    let owned = text.to_owned();
    std::thread::spawn(move || {
        let ok = arboard::Clipboard::new()
            .and_then(|mut cb| cb.set_text(owned))
            .is_ok();
        let _ = tx.send(ok);
    });
    rx
}

/// Wait `wait` for the native worker; on timeout/failure emit the OSC 52
/// fallback instead. Returns `true` when the native copy won.
///
/// The whole point (PR #266 regression): a hung or slow worker must never
/// hold the UI thread beyond `wait`.
fn copy_with_fallback<W: Write, F>(out: &mut W, text: &str, wait: Duration, spawn_worker: F) -> bool
where
    F: FnOnce(&str) -> std::sync::mpsc::Receiver<bool>,
{
    let rx = spawn_worker(text);
    if rx.recv_timeout(wait) == Ok(true) {
        return true;
    }
    let _ = write!(out, "\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let _ = out.flush();
    false
}

fn base64_encode(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn main() {
    if let Err(e) = run() {
        let _ = restore_terminal();
        eprintln!("noviewlog-tui: {e}");
        std::process::exit(1);
    }
}

/// CLI: `--ssh <target>` (connect now, with `--port <n>` and repeatable
/// `--ssh-arg <arg>`), `--profile <name>` (saved profile), `--connect`
/// (profile list overlay), no args = local shell (unchanged).
enum CliChoice {
    Local,
    Ssh(SessionChoice),
    Connect,
}

/// Human-readable user config path for error / overlay messages.
pub(crate) fn config_path_label() -> String {
    noviewlog_core::core::config::user_config_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "~/.config/noviewlog/config.yaml".into())
}

fn parse_cli() -> Result<CliChoice, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut iter = args.iter();
    let mut choice = CliChoice::Local;
    // `--port` / `--ssh-arg` accumulate and apply to the `--ssh` target
    // (defaults keep the plain `ssh -t <target>` argv).
    let mut port: u16 = 0;
    let mut extra: Vec<String> = Vec::new();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--ssh" => {
                let target = iter.next().ok_or("--ssh requires a target (user@host)")?;
                ssh::probe_ssh_client()?;
                choice = CliChoice::Ssh(SessionChoice::Ssh {
                    label: target.clone(),
                    argv: ssh::argv_from_cli_flags(target, port, &extra),
                });
                port = 0;
                extra.clear();
            }
            "--port" => {
                let v = iter.next().ok_or("--port requires a port number")?;
                port = v
                    .parse()
                    .map_err(|_| format!("--port expects a number 0-65535, got `{v}`"))?;
            }
            "--ssh-arg" => {
                let v = iter.next().ok_or("--ssh-arg requires an argument")?;
                extra.push(v.clone());
            }
            "--profile" => {
                let name = iter.next().ok_or("--profile requires a profile name")?;
                let profiles = load_profiles()?;
                let p = profiles.iter().find(|p| p.name == *name).ok_or_else(|| {
                    format!(
                        "no ssh profile `{name}` — add it to {} under tui_ssh_profiles",
                        config_path_label()
                    )
                })?;
                ssh::probe_ssh_client()?;
                choice = CliChoice::Ssh(SessionChoice::Ssh {
                    label: format!("{} ({})", p.name, p.target),
                    argv: ssh::argv_for_profile(p),
                });
            }
            "--connect" => choice = CliChoice::Connect,
            other => {
                return Err(format!(
                    "unknown argument `{other}` (use --ssh, --profile, --connect)"
                ))
            }
        }
    }
    Ok(choice)
}

fn load_profiles() -> Result<Vec<SshProfile>, String> {
    let (config, warning) = load_user_config();
    if let Some(w) = warning {
        eprintln!("noviewlog-tui: config warning: {w}");
    }
    Ok(config.map(|c| c.tui_ssh_profiles).unwrap_or_default())
}

fn run() -> Result<(), String> {
    let cli = parse_cli()?;
    enable_raw_mode().map_err(|e| e.to_string())?;
    let mut out = stdout();
    // Mouse capture is always on: the wheel scrolls our output like a
    // terminal buffer (on the alternate screen the host would otherwise
    // translate it to arrow keys = shell history). Selection is our own.
    // Bracketed paste: pastes arrive as Event::Paste (filter input) or are
    // forwarded to the shell as plain stdin instead of being replayed as a
    // stream of fake keystrokes (#199).
    execute!(
        out,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )
    .map_err(|e| e.to_string())?;
    // Any panic (and normal exit) must restore the user's terminal.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = restore_terminal();
        prev_hook(info);
    }));

    let (cols, rows) = crossterm::terminal::size().map_err(|e| e.to_string())?;
    let mut app = App::new(cols, rows, load_profiles()?)?;
    app.sync_geometry();
    match cli {
        CliChoice::Local => app.start_session(SessionChoice::Local)?,
        CliChoice::Ssh(choice) => app.start_session(choice)?,
        CliChoice::Connect => {
            app.connect_open = true;
            app.connect_sel = 0;
        }
    }
    app.ui_dirty = true;
    let _ = execute!(out, crossterm::cursor::Hide);

    let result = event_loop(&mut app);

    let _ = restore_terminal();
    result
}

fn event_loop(app: &mut App) -> Result<(), String> {
    loop {
        // Drain input (16ms poll doubles as the frame tick).
        if crossterm::event::poll(FRAME_BUDGET).map_err(|e| e.to_string())? {
            match crossterm::event::read().map_err(|e| e.to_string())? {
                Event::Key(k) => {
                    if app.handle_key(k).is_none() {
                        return Ok(());
                    }
                }
                Event::Mouse(m) => app.handle_mouse(m),
                Event::Resize(cols, rows) => {
                    app.cols = cols;
                    app.rows = rows;
                    app.sync_geometry();
                }
                Event::Paste(text) => app.handle_paste(&text),
                Event::FocusGained | Event::FocusLost => {}
            }
        }
        app.engine.tick();
        app.drain_events();
        let due = app.last_paint.is_none_or(|t| t.elapsed() >= FRAME_BUDGET)
            && (app.engine.needs_render() || app.ui_dirty);
        if due {
            app.paint();
            app.engine.note_viewport_painted();
        }
    }
}

fn restore_terminal() -> io::Result<()> {
    let mut out = stdout();
    disable_raw_mode()?;
    execute!(
        out,
        crossterm::cursor::Show,
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caret_x_clamps_before_cast() {
        // Clamp happens in usize: an extreme engine column saturates to the
        // right edge instead of truncating through u16 to a bogus x (#241).
        assert_eq!(caret_x(usize::MAX, 80), 79);
        assert_eq!(caret_x(5_000, 80), 79);
        assert_eq!(caret_x(usize::MAX, 40), 39);
        // In-range columns keep the +2 prefix offset.
        assert_eq!(caret_x(0, 80), 2);
        assert_eq!(caret_x(10, 80), 12);
    }

    #[test]
    fn paste_append_caps_filter_buffer() {
        let mut buf = String::new();
        let chunk = "a".repeat(FILTER_BUF_CAP);
        paste_append(&mut buf, &chunk);
        paste_append(&mut buf, &chunk);
        assert_eq!(buf.len(), FILTER_BUF_CAP);
        assert_eq!(buf.chars().count(), FILTER_BUF_CAP);
    }

    #[test]
    fn paste_append_stops_on_char_boundary() {
        // Two-byte chars: the cap must never split a UTF-8 sequence.
        let mut buf = String::new();
        let wide = "é".repeat(FILTER_BUF_CAP);
        paste_append(&mut buf, &wide);
        assert!(buf.chars().all(|c| c == 'é'));
        assert!(buf.len() <= FILTER_BUF_CAP);
        assert!(std::str::from_utf8(buf.as_bytes()).is_ok());
    }

    // --- Clipboard copy must never hold the UI thread (PR #266). The freeze
    // bug: arboard ran synchronously in the mouse-up handler and win32
    // clipboard contention froze the whole event loop. ---

    /// Worker that answers after `delay` with `result`.
    fn delayed_worker(
        delay: Duration,
        result: bool,
    ) -> impl FnOnce(&str) -> std::sync::mpsc::Receiver<bool> {
        move |_text: &str| {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                std::thread::sleep(delay);
                let _ = tx.send(result);
            });
            rx
        }
    }

    #[test]
    fn hung_clipboard_worker_cannot_block_ui_and_falls_back() {
        // Regression for the reported freeze: a worker stuck on clipboard
        // contention must release the UI after the wait budget, and the copy
        // must still happen via the OSC 52 fallback.
        let mut out: Vec<u8> = Vec::new();
        let started = Instant::now();
        let copied = copy_with_fallback(
            &mut out,
            "selection",
            Duration::from_millis(50),
            delayed_worker(Duration::from_secs(5), true),
        );
        let elapsed = started.elapsed();
        assert!(!copied, "timeout worker must lose to the fallback");
        assert!(
            elapsed < Duration::from_secs(2),
            "UI thread blocked {elapsed:?} — the old sync freeze is back"
        );
        let s = String::from_utf8(out).unwrap();
        assert_eq!(
            s,
            format!("\x1b]52;c;{}\x07", base64_encode(b"selection")),
            "OSC 52 fallback with exact payload"
        );
    }

    #[test]
    fn fast_clipboard_success_skips_fallback() {
        let mut out: Vec<u8> = Vec::new();
        let copied = copy_with_fallback(
            &mut out,
            "text",
            Duration::from_millis(200),
            delayed_worker(Duration::from_millis(10), true),
        );
        assert!(copied);
        assert!(out.is_empty(), "no OSC 52 when the native copy won");
    }

    #[test]
    fn failing_clipboard_worker_falls_back_to_osc52() {
        let mut out: Vec<u8> = Vec::new();
        let copied = copy_with_fallback(
            &mut out,
            "abc",
            Duration::from_millis(200),
            delayed_worker(Duration::from_millis(10), false),
        );
        assert!(!copied);
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!("\x1b]52;c;{}\x07", base64_encode(b"abc"))
        );
    }

    #[test]
    fn connect_box_width_matches_draw_clamp() {
        // 40-col terminal: full box fits, inner width 38 (draw and hit test
        // must agree so phantom clicks cannot select unseen items).
        assert_eq!(App::connect_box_width(0, 40), 38);
        // 20-col terminal: the box clamps to the terminal width.
        assert_eq!(App::connect_box_width(0, 20), 18);
    }

    #[test]
    fn menu_width_matches_draw_clamp() {
        // Wide terminal: fixed 24-col menu.
        assert_eq!(App::menu_width(0, 80), 24);
        // cols=20 with the menu at col=10: only the 10 drawn cells are
        // clickable (draw and hit test share this helper, #254).
        assert_eq!(App::menu_width(10, 20), 10);
        assert_eq!(App::menu_width(20, 20), 0);
        // No overflow at the terminal edge.
        assert_eq!(App::menu_width(u16::MAX, 80), 0);
    }

    #[test]
    fn text_in_cells_slices_ascii_like_char_indices() {
        // Pure ASCII: identical to the old chars[start..end] slice.
        assert_eq!(text_in_cells("hello world", 0, 4), "hello");
        assert_eq!(text_in_cells("hello", 1, 3), "ell");
        assert_eq!(text_in_cells("abc", 5, 9), "");
    }

    #[test]
    fn text_in_cells_maps_wide_chars_to_cells() {
        // "日志ab": 日 = cells 0..1, 志 = cells 2..3, a = 4, b = 5.
        // Cell selection 0..=3 must copy exactly the two CJK glyphs — the
        // old char-index slice returned "日日" (4 chars for 4 "indices").
        assert_eq!(text_in_cells("日志ab", 0, 3), "日志");
        assert_eq!(text_in_cells("日志ab", 2, 4), "志a");
        assert_eq!(text_in_cells("日志ab", 4, 5), "ab");
        assert_eq!(text_in_cells("日志", 1, 2), "志");
    }

    #[test]
    fn selected_text_uses_cell_coords_on_mixed_lines() {
        // End-to-end through selected_text: one CJK+ASCII line, drag over a
        // known cell range; the copied substring must be exact.
        let app = app_with_visible_lines(&["日志ab"]);
        assert_eq!(app.selected_text((0, 0), (0, 3)), "日志");
        assert_eq!(app.selected_text((0, 2), (0, 4)), "志a");
        assert_eq!(app.selected_text((0, 4), (0, 5)), "ab");
    }

    /// Minimal App for selection tests: engine built off-session, one
    /// pre-populated visible slice (no terminal, no PTY).
    fn app_with_visible_lines(lines: &[&str]) -> App {
        use noviewlog_core::core::types::{LogLevel, TextSegment};
        let mut app = App::new(80, 24, Vec::new()).expect("app");
        app.visible = lines
            .iter()
            .map(|l| FlatLine {
                record_id: 0,
                line_index: 0,
                raw: (*l).to_string(),
                hidden_line_count: 0,
                collapsible: false,
                collapsed: false,
                level: Some(LogLevel::Info),
                segments: vec![TextSegment {
                    text: (*l).to_string(),
                    style: None,
                }],
            })
            .collect();
        app
    }

    #[test]
    fn handle_paste_drops_before_filter_buf_when_exited() {
        // Gate order: an exited session swallows the paste even with the
        // filter input focused (#254).
        let mut app = App::new(80, 24, Vec::new()).expect("app");
        app.input_focus = true;
        app.exited = Some("ssh x exited (code 0)".to_string());
        app.handle_paste("leak");
        assert!(app.filter_buf.is_empty());
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn ctrl_l_clears_filters() {
        assert!(matches!(
            key_command(&key(KeyCode::Char('l'), KeyModifiers::CONTROL), 3, 2),
            Some(Command::FilterClear)
        ));
        assert!(matches!(
            key_command(&key(KeyCode::Char('L'), KeyModifiers::CONTROL), 1, 0),
            Some(Command::FilterClear)
        ));
    }

    #[test]
    fn ctrl_w_closes_active_filter_tab_never_tab0() {
        assert!(matches!(
            key_command(&key(KeyCode::Char('w'), KeyModifiers::CONTROL), 3, 2),
            Some(Command::TabClose { index: 2 })
        ));
        // Terminal tab 0 is pinned (engine rejects close on it).
        assert!(key_command(&key(KeyCode::Char('w'), KeyModifiers::CONTROL), 3, 0).is_none());
    }

    #[test]
    fn ctrl_tab_cycles_to_next_tab_with_wrap() {
        assert!(matches!(
            key_command(&key(KeyCode::Tab, KeyModifiers::CONTROL), 3, 0),
            Some(Command::TabSwitch { index: 1 })
        ));
        // Wrap at the last tab back to the Terminal tab.
        assert!(matches!(
            key_command(&key(KeyCode::Tab, KeyModifiers::CONTROL), 3, 2),
            Some(Command::TabSwitch { index: 0 })
        ));
        // A single tab has nothing to cycle to.
        assert!(key_command(&key(KeyCode::Tab, KeyModifiers::CONTROL), 1, 0).is_none());
    }

    #[test]
    fn alt_digits_switch_to_existing_tabs_only() {
        let alt = KeyModifiers::ALT;
        assert!(matches!(
            key_command(&key(KeyCode::Char('1'), alt), 3, 0),
            Some(Command::TabSwitch { index: 0 })
        ));
        assert!(matches!(
            key_command(&key(KeyCode::Char('3'), alt), 3, 0),
            Some(Command::TabSwitch { index: 2 })
        ));
        // Tab 4 does not exist with 3 tabs.
        assert!(key_command(&key(KeyCode::Char('4'), alt), 3, 0).is_none());
        // Alt+9 with 9+ tabs maps to index 8.
        assert!(matches!(
            key_command(&key(KeyCode::Char('9'), alt), 10, 0),
            Some(Command::TabSwitch { index: 8 })
        ));
        // Unmodified digits type into the shell, never switch tabs.
        assert!(key_command(&key(KeyCode::Char('3'), KeyModifiers::NONE), 3, 0).is_none());
    }

    #[test]
    fn plain_keys_map_to_no_command() {
        // Transparent-terminal keys stay transparent.
        for k in [
            key(KeyCode::Char('x'), KeyModifiers::NONE),
            key(KeyCode::Enter, KeyModifiers::NONE),
            key(KeyCode::Tab, KeyModifiers::NONE),
            key(KeyCode::Char('c'), KeyModifiers::CONTROL),
        ] {
            assert!(key_command(&k, 3, 1).is_none());
        }
    }

    #[test]
    fn menu_items_include_clear_filters_and_count_drives_geometry() {
        let items = menu_items(false);
        assert_eq!(items.len(), 5);
        assert_eq!(items[3], "Clear filters");
        assert_eq!(items[4], "Cancel");
        // Collapsed variant only swaps the first item.
        assert_eq!(menu_items(true)[0], "Expand record");
        assert_eq!(menu_items(true).len(), items.len());
        // The click-position clamp must reserve items+2 rows (borders), not
        // a hardcoded budget (P3-12).
        let menu_rows = items.len() as u16 + 2;
        assert_eq!(menu_rows, 7);
    }

    #[test]
    fn menu_hit_matches_drawn_item_rows() {
        // Menu box top at row 2, 5 items: borders at rows 2 and 8, items at
        // rows 3..=7. Draw (render.rs) paints item i at menu.row + i + 1;
        // the hit test must agree exactly.
        assert_eq!(menu_hit(2, 2, 5), None, "top border");
        for i in 0..5u16 {
            assert_eq!(menu_hit(2, 2 + i + 1, 5), Some(i as usize));
        }
        assert_eq!(menu_hit(2, 8, 5), Some(5), "bottom border = no-op index");
        assert_eq!(menu_hit(2, 9, 5), None, "below the box");
        // The no-op index is not one of the real item labels.
        assert!(menu_hit(2, 8, 5).unwrap() < menu_items(false).len() + 1);
    }

    #[test]
    fn app_reports_active_tab_and_count_from_stats() {
        let app = App::new(80, 24, Vec::new()).expect("app");
        // No stats yet: Terminal tab only.
        assert_eq!(app.active_tab_index(), 0);
        assert_eq!(app.tab_count(), 1);
    }

    #[test]
    fn connect_sel_move_clamps_at_both_ends() {
        // Clamp, no wrap: Up from the first item stays there, Down from the
        // last stays there.
        assert_eq!(connect_sel_move(0, -1, 4), 0);
        assert_eq!(connect_sel_move(3, 1, 4), 3);
        assert_eq!(connect_sel_move(1, 1, 4), 2);
        assert_eq!(connect_sel_move(1, -1, 4), 0);
        // An out-of-range start is pulled back into range.
        assert_eq!(connect_sel_move(9, 0, 4), 3);
        // An empty list never yields a live index.
        assert_eq!(connect_sel_move(0, 1, 0), 0);
    }

    fn mouse(kind: MouseEventKind, row: u16, column: u16) -> MouseEvent {
        MouseEvent {
            kind,
            row,
            column,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn right_click_builds_menu_over_content_rows() {
        let mut app = App::new(80, 24, Vec::new()).expect("app");
        app.handle_mouse(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Right),
            1,
            10,
        ));
        assert!(app.menu.is_some());
    }

    #[test]
    fn right_click_on_tab_bar_row_builds_no_menu() {
        let mut app = App::new(80, 24, Vec::new()).expect("app");
        app.handle_mouse(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Right),
            0,
            10,
        ));
        assert!(app.menu.is_none(), "row 0 is the tab bar, not a record");
    }

    #[test]
    fn right_click_gated_while_connect_overlay_open() {
        let mut app = App::new(80, 24, Vec::new()).expect("app");
        app.connect_open = true;
        app.handle_mouse(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Right),
            1,
            10,
        ));
        assert!(app.menu.is_none());
    }

    #[test]
    fn right_click_gated_while_session_exited() {
        let mut app = App::new(80, 24, Vec::new()).expect("app");
        app.exited = Some("ssh x exited (code 0)".to_string());
        app.handle_mouse(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Right),
            1,
            10,
        ));
        assert!(app.menu.is_none());
    }

    #[test]
    fn connect_choice_maps_items_and_rejects_out_of_range() {
        let app = App::new(80, 24, Vec::new()).expect("app");
        // No profiles: item 0 is the local shell, item 1 is out of range.
        assert!(matches!(app.connect_choice(0), Some(SessionChoice::Local)));
        assert!(app.connect_choice(1).is_none());
        assert!(app.connect_choice(usize::MAX).is_none());
    }

    #[test]
    fn left_click_outside_content_clears_stale_selection() {
        // A selection kept from an earlier drag must not survive a click on
        // the status row (cell_at = None there).
        let mut app = app_with_visible_lines(&["line one"]);
        app.sel_anchor = Some((0, 0));
        app.sel_current = Some((0, 3));
        let status_row = 24 - 1;
        app.handle_mouse(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            status_row,
            5,
        ));
        assert!(app.sel_anchor.is_none());
        assert!(app.sel_current.is_none());
    }

    #[test]
    fn ctrl_shift_c_returns_after_copy_attempt_without_selection() {
        // Ctrl+Shift+C is consumed by the handler (never forwarded to the
        // shell); with no selection it just copies nothing.
        let mut app = App::new(80, 24, Vec::new()).expect("app");
        assert!(app
            .handle_key(key(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT
            ))
            .is_some());
    }
}
