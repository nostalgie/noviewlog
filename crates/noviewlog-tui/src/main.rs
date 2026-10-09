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

mod app;
mod input;
mod render;
mod ssh;

use std::io::{self, stdout};
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

use noviewlog_core::core::config::load_user_config;
use noviewlog_core::core::types::SshProfile;

pub(crate) use app::App;
use ssh::SessionChoice;

/// Frame budget: at most one paint per interval (flood-safe rendering).
const FRAME_BUDGET: Duration = Duration::from_millis(16);

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
#[derive(Debug)]
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
    parse_cli_from(&args)
}

fn parse_cli_from(args: &[String]) -> Result<CliChoice, String> {
    let mut iter = args.iter();
    let mut choice = CliChoice::Local;
    // `--port` / `--ssh-arg` accumulate and apply to the `--ssh` target
    // regardless of flag order (issue #348): the target's argv is built
    // after the scan. `--port` keeps its last value, `--ssh-arg` collects
    // all occurrences; a repeated `--ssh` means the last target wins.
    let mut port: u16 = 0;
    let mut extra: Vec<String> = Vec::new();
    let mut ssh: Option<String> = None;
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--ssh" => {
                let target = iter.next().ok_or("--ssh requires a target (user@host)")?;
                ssh::probe_ssh_client()?;
                ssh = Some(target.clone());
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
                ssh = None;
                choice = CliChoice::Ssh(SessionChoice::Ssh {
                    label: format!("{} ({})", p.name, p.target),
                    argv: ssh::argv_for_profile(p),
                });
            }
            "--connect" => {
                ssh = None;
                choice = CliChoice::Connect;
            }
            other => {
                return Err(format!(
                    "unknown argument `{other}` (use --ssh, --profile, --connect)"
                ))
            }
        }
    }
    if let Some(target) = ssh {
        choice = CliChoice::Ssh(SessionChoice::Ssh {
            label: target.clone(),
            argv: ssh::argv_from_cli_flags(&target, port, &extra),
        });
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
    use std::time::{Duration, Instant};

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

    use noviewlog_core::core::types::FlatLine;
    use noviewlog_core::Command;

    use crate::input::*;
    use crate::ssh::SessionChoice;

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

    #[test]
    fn selected_text_includes_release_cell_like_highlight() {
        // Drag anchor→current must copy the release cell too (end-inclusive),
        // matching the painted highlight (#350).
        let app = app_with_visible_lines(&["hello"]);
        assert_eq!(app.selected_text((0, 0), (0, 4)), "hello");
        assert_eq!(app.selected_text((0, 1), (0, 3)), "ell");
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

#[cfg(test)]
mod cli_tests {
    use super::*;

    fn ssh_argv(args: &[&str]) -> Vec<String> {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match parse_cli_from(&owned).expect("parse ok") {
            CliChoice::Ssh(SessionChoice::Ssh { argv, .. }) => argv,
            other => panic!("expected ssh choice, got {other:?}"),
        }
    }

    #[test]
    fn ssh_flags_after_target_apply() {
        // Issue #348: the natural order connected to port 22 silently.
        assert!(
            ssh_argv(&["--ssh", "web01", "--port", "2222"])
                .windows(2)
                .any(|w| w == ["-p", "2222"]),
            "--port after --ssh must reach the argv"
        );
    }

    #[test]
    fn ssh_flags_before_target_apply() {
        assert!(
            ssh_argv(&["--port", "2222", "--ssh", "web01"])
                .windows(2)
                .any(|w| w == ["-p", "2222"]),
            "--port before --ssh must reach the argv"
        );
    }

    #[test]
    fn ssh_args_apply_to_last_target() {
        let argv = ssh_argv(&["--ssh-arg", "-v", "--ssh", "web01", "--ssh-arg", "-o"]);
        assert!(argv.contains(&"-o".to_string()), "post-target --ssh-arg");
        assert!(argv.contains(&"-v".to_string()), "pre-target --ssh-arg");
    }

    #[test]
    fn later_ssh_resets_and_wins() {
        let argv = ssh_argv(&["--ssh", "a", "--port", "1", "--ssh", "b", "--port", "2"]);
        assert!(argv.windows(2).any(|w| w == ["-p", "2"]));
        assert!(!argv.windows(2).any(|w| w == ["-p", "1"]));
    }

    #[test]
    fn port_without_ssh_stays_local() {
        let owned: Vec<String> = ["--port", "2222"].iter().map(|s| s.to_string()).collect();
        assert!(matches!(parse_cli_from(&owned), Ok(CliChoice::Local)));
    }
}
