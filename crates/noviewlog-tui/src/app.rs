//! TUI application state: sessions, mouse/keyboard routing, paint tick.
//!
//! Free key/menu/clipboard helpers live in [`crate::input`].

use std::io::{stdout, Write};
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use crossterm::execute;

use noviewlog_core::core::types::{
    FilterType, FlatLine, LaunchConfig, ShellPreference, SshProfile,
};
use noviewlog_core::spawn_resolve::resolve_interactive_shell;
use noviewlog_core::{parse_engine_event, Command, Engine, EngineEvent, StatsSnapshot};

use crate::input::{
    caret_x, connect_sel_move, copy_to_clipboard, key_command, menu_hit, menu_items, paste_append,
    shell_key_bytes, text_in_cells, MENU_BOX_COLS, MENU_WIDTH,
};
use crate::render;
use crate::ssh::SessionChoice;

/// Double-click window for record expand/collapse.
const DOUBLE_CLICK: Duration = Duration::from_millis(350);

/// Right-click context menu rendered as a text overlay on the grid.
pub(crate) struct Menu {
    /// Screen row/col of the box top-left.
    pub(crate) row: u16,
    pub(crate) col: u16,
    /// Actions for the record under the cursor (if any).
    pub(crate) record_id: Option<u64>,
    /// Whole text of the record's first line (for "copy line").
    pub(crate) line_text: String,
    pub(crate) items: Vec<&'static str>,
}

pub(crate) struct App {
    pub(crate) engine: Engine,
    pub(crate) stats: Option<StatsSnapshot>,
    /// Filter input line focus: while open, keys edit the pattern.
    pub(crate) input_focus: bool,
    pub(crate) filter_buf: String,
    pub(crate) confirm_quit: bool,
    /// Saved SSH profiles (user config `tui_ssh_profiles`).
    pub(crate) profiles: Vec<SshProfile>,
    /// The running (or last) session; `r` reconnects with the same argv.
    pub(crate) session: Option<SessionChoice>,
    /// Session child exited: banner text; keys are gated (R/N/Q), never
    /// passed through to any shell (design D6).
    pub(crate) exited: Option<String>,
    /// SSH profile list overlay open (mouse-driven; Esc/outside = local).
    pub(crate) connect_open: bool,
    /// Keyboard selection index into the connect overlay items.
    pub(crate) connect_sel: usize,
    /// Label of the running (or last) SSH session, shown in the status bar.
    pub(crate) session_label: Option<String>,
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    /// Cursor into the visible slice (keyboard record toggle); None = none.
    pub(crate) cursor: Option<usize>,
    /// Hit-testing for mouse clicks: (start_col, len, tab_index) per tab-bar cell.
    pub(crate) tab_spans: Vec<(u16, u16, usize)>,
    /// Column span of the "+" (new filter tab) cell in the tab bar.
    pub(crate) tab_add_span: Option<(u16, u16)>,
    /// record_id painted per content row (row 0 = first content row).
    pub(crate) row_records: Vec<Option<u64>>,
    /// Text selection drag state in content-cell coords (row, col), row 0 =
    /// first visible content row, col 0 = after the 2-char prefix.
    pub(crate) sel_anchor: Option<(usize, usize)>,
    pub(crate) sel_current: Option<(usize, usize)>,
    /// Last left-click (cell + time) for double-click detection.
    pub(crate) last_click: Option<((u16, u16), Instant)>,
    /// Visible flat lines snapshot (selection text source, menu context).
    pub(crate) visible: Vec<FlatLine>,
    /// Open context menu, if any.
    pub(crate) menu: Option<Menu>,
    /// Whether the previous frame had the menu open.
    pub(crate) menu_was_open: bool,
    /// Whether the previous frame had the connect overlay open.
    pub(crate) connect_was_open: bool,
    /// UI state changed (selection/menu/input) — repaint next frame even if
    /// the engine considers its viewport clean.
    pub(crate) ui_dirty: bool,
    /// Rendered byte buffers of the previous frame (diff painting).
    pub(crate) frame_prev: Vec<Vec<u8>>,
    pub(crate) last_paint: Option<Instant>,
}

impl App {
    pub(crate) fn new(cols: u16, rows: u16, profiles: Vec<SshProfile>) -> Result<Self, String> {
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
    pub(crate) fn start_session(&mut self, choice: SessionChoice) -> Result<(), String> {
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

    pub(crate) fn cmd(&mut self, c: Command) {
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
    pub(crate) fn handle_paste(&mut self, text: &str) {
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

    pub(crate) fn sync_geometry(&mut self) {
        // Cell units, not pixels: Command::Resize is bitmap-host pixel space.
        self.engine
            .set_terminal_grid(self.cols.max(1), self.content_rows().max(1));
    }

    pub(crate) fn content_rows(&self) -> u16 {
        self.rows.saturating_sub(3)
    }

    pub(crate) fn drain_events(&mut self) {
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
    pub(crate) fn active_tab_index(&self) -> usize {
        self.stats.as_ref().map_or(0, |s| s.active_tab)
    }

    /// Number of tabs (Terminal + filter tabs); at least the Terminal tab.
    pub(crate) fn tab_count(&self) -> usize {
        self.stats.as_ref().map_or(1, |s| s.tabs.len().max(1))
    }

    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<()> {
        // Any key may alter UI state (input line, confirm); repaint cheaply.
        self.ui_dirty = true;
        // Act on Press only; some hosts synthesize Release/Repeat events.
        if key.kind != KeyEventKind::Press {
            return Some(());
        }
        // Quit: the first Ctrl+Q arms the confirm prompt, a second Ctrl+Q
        // quits. While armed, any other key only disarms it: the code
        // consumes that key and forwards nothing to the shell.
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
            self.copy_current_selection();
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
    pub(crate) fn connect_items(&self) -> Vec<String> {
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
    pub(crate) fn connect_choice(&self, idx: usize) -> Option<SessionChoice> {
        if idx < self.profiles.len() {
            let p = &self.profiles[idx];
            let label = format!("{} ({})", p.name, p.target);
            Some(SessionChoice::Ssh {
                label,
                argv: crate::ssh::argv_for_profile(p),
            })
        } else if idx < self.connect_items().len() {
            Some(SessionChoice::Local)
        } else {
            None
        }
    }

    /// Deterministic overlay geometry: centered box, 40 cols wide.
    /// (col, row, item_count) — same math as the painter in render.rs.
    pub(crate) fn connect_geo(&self) -> (u16, u16, usize) {
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
        MENU_WIDTH.min(cols.saturating_sub(col))
    }

    pub(crate) fn handle_mouse(&mut self, m: MouseEvent) {
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
                let col = m.column.min(self.cols.saturating_sub(MENU_BOX_COLS));
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
                // Priority: connect overlay, then context menu, then the tab
                // bar (row 0), then the content area.
                if self.connect_open {
                    self.connect_click(m);
                } else if self.menu.is_some() {
                    self.menu_click(m);
                } else if m.row == 0 {
                    self.tab_bar_click(m);
                } else {
                    self.content_click(m);
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
                // Click without drag: do not leave a one-cell highlight.
                if self.sel_anchor == self.sel_current {
                    self.sel_anchor = None;
                    self.sel_current = None;
                } else {
                    self.copy_current_selection();
                }
            }
            _ => {}
        }
    }

    /// Copy the current selection (if non-empty) to the clipboard. The
    /// highlight is kept; the text comes from the visible slice on our own
    /// (no engine coordinate math). Shared by mouse-up and Ctrl+Shift+C.
    fn copy_current_selection(&self) {
        if let (Some(a), Some(b)) = (self.sel_anchor, self.sel_current) {
            if a != b {
                let text = self.selected_text(a.min(b), a.max(b));
                if !text.is_empty() {
                    copy_to_clipboard(&text);
                }
            }
        }
    }

    /// Left click while the connect overlay is open: an item starts that
    /// session; outside the box falls back to the local shell (same as Esc).
    fn connect_click(&mut self, m: MouseEvent) {
        // Box geometry matches the painter: title row at orow+1, items start
        // at orow+2.
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
    }

    /// Left click while the context menu is open: run the clicked item (if
    /// any) and always close the menu.
    fn menu_click(&mut self, m: MouseEvent) {
        let Some(menu) = self.menu.take() else {
            return;
        };
        let Menu {
            row: mrow,
            col: mcol,
            record_id,
            line_text,
            items,
        } = menu;
        // Hit-test: item 0 = the row below the top border; the bottom border
        // maps to a no-op index.
        let in_box =
            m.column >= mcol && m.column < mcol.saturating_add(Self::menu_width(mcol, self.cols));
        let idx = if in_box {
            menu_hit(mrow, m.row, items.len())
        } else {
            None
        };
        let Some(idx) = idx else {
            return;
        };
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

    /// Left click on the tab bar (row 0): switch to a tab, or "+" adds a
    /// filter tab and opens the filter input.
    fn tab_bar_click(&mut self, m: MouseEvent) {
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
    }

    /// Left click below the tab bar: double-click toggles record
    /// expand/collapse, a single click starts a text selection, and a click
    /// outside the content area clears a stale selection.
    fn content_click(&mut self, m: MouseEvent) {
        let Some(cell) = self.cell_at(&m) else {
            // Outside the content area (input/status rows): dismiss a stale
            // selection instead of leaving a highlight no drag explains.
            self.sel_anchor = None;
            self.sel_current = None;
            return;
        };
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
    }

    /// Text of the selected span across the visible slice (inclusive of the
    /// end cell's column on its last row; missing cells are skipped).
    pub(crate) fn selected_text(&self, a: (usize, usize), b: (usize, usize)) -> String {
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
    pub(crate) fn cell_at(&self, m: &MouseEvent) -> Option<(usize, usize)> {
        if m.row == 0 || m.row as usize > self.content_rows() as usize {
            return None;
        }
        Some(((m.row - 1) as usize, m.column.saturating_sub(2) as usize))
    }

    pub(crate) fn paint(&mut self) {
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
