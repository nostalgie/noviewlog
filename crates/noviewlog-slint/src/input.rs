//! Keyboard, clipboard, and pointer input: helpers + UI wiring.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use noviewlog_core::core::types::{
    DEFAULT_VIEWPORT_FONT_SIZE, MAX_VIEWPORT_FONT_SIZE, MIN_VIEWPORT_FONT_SIZE,
};
use noviewlog_core::{Command, Engine};
use slint::ComponentHandle;

use crate::app_state::ClickTracker;
use crate::ctx::Ctx;
use crate::engine_bridge::bump_fast_timer;
use crate::viewport::apply_zoom;
use noviewlog_slint::ui::AppWindow;

pub(crate) fn is_zoom_in_key(text: &str) -> bool {
    text == "=" || text == "+"
}

pub(crate) fn clipboard_has_text() -> bool {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return false;
    };
    clipboard.get_text().map(|t| !t.is_empty()).unwrap_or(false)
}

pub(crate) fn is_key_char(text: &str, want: char) -> bool {
    text.chars()
        .next()
        .map(|c| c.eq_ignore_ascii_case(&want))
        .unwrap_or(false)
}

pub(crate) fn copy_selection_to_clipboard(engine: &Engine) -> bool {
    let Some(text) = engine.selection_text() else {
        return false;
    };
    if text.is_empty() {
        return false;
    }
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return false;
    };
    clipboard.set_text(text).is_ok()
}

/// Bytes to write to the PTY for a pasted clipboard payload (issue #70).
///
/// Terminals expect `\r` for line endings — the same byte Enter sends. Bare
/// `\n` leaves CR-expecting ConPTY readers (cmd-style prompts) hanging in the
/// middle of a multi-line paste.
pub(crate) fn paste_bytes(text: &str) -> Vec<u8> {
    text.replace("\r\n", "\r").replace('\n', "\r").into_bytes()
}

pub(crate) fn paste_clipboard_to_terminal(engine: &mut Engine) {
    let Ok(mut clipboard) = arboard::Clipboard::new() else {
        return;
    };
    let Ok(text) = clipboard.get_text() else {
        return;
    };
    if text.is_empty() {
        return;
    }
    let bytes = paste_bytes(&text);
    engine.handle_key(&bytes);
}

pub(crate) fn handle_key_event(engine: &mut Engine, text: &str, ctrl_or_meta: bool) -> bool {
    if text.is_empty() {
        return false;
    }

    if ctrl_or_meta {
        let ch = text.chars().next().unwrap_or('\0');
        let lower = ch.to_ascii_lowercase();
        if lower.is_ascii_lowercase() {
            let byte = (lower as u8) - b'a' + 1;
            engine.handle_key(&[byte]);
            return true;
        }
        return false;
    }

    if let Some(bytes) = map_special_key(text) {
        engine.handle_key(bytes);
        return true;
    }

    if is_terminal_control_text(text) {
        return true;
    }

    if is_private_use_key(text) {
        // Unmapped special key: swallow it rather than send the private-use
        // codepoint into the shell as UTF-8 mojibake (issue #319).
        return true;
    }

    engine.handle_key(text.as_bytes());
    true
}

pub(crate) fn is_terminal_control_text(text: &str) -> bool {
    text.chars()
        .all(|ch| matches!(ch, '\r' | '\n' | '\t' | '\u{7f}' | '\u{8}') || ch < ' ')
}

/// Slint encodes non-text keys (arrows, Insert, F1–F12) as private-use
/// codepoints. Any of them that `map_special_key` does not map must never
/// reach the PTY — the shell would receive their UTF-8 bytes as garbage.
pub(crate) fn is_private_use_key(text: &str) -> bool {
    text.chars()
        .next()
        .is_some_and(|c| ('\u{f700}'..='\u{f7ff}').contains(&c))
}

pub(crate) fn map_special_key(text: &str) -> Option<&'static [u8]> {
    const BACKSPACE: &str = "\u{8}";
    const TAB: &str = "\u{9}";
    const RETURN: &str = "\n";
    const ESCAPE: &str = "\u{1b}";
    const DELETE: &str = "\u{7f}";
    const UP: &str = "\u{f700}";
    const DOWN: &str = "\u{f701}";
    const LEFT: &str = "\u{f702}";
    const RIGHT: &str = "\u{f703}";
    const INSERT: &str = "\u{f727}";
    const HOME: &str = "\u{f729}";
    const END: &str = "\u{f72b}";
    const PAGE_UP: &str = "\u{f72c}";
    const PAGE_DOWN: &str = "\u{f72d}";
    const F1: &str = "\u{f704}";
    const F2: &str = "\u{f705}";
    const F3: &str = "\u{f706}";
    const F4: &str = "\u{f707}";
    const F5: &str = "\u{f708}";
    const F6: &str = "\u{f709}";
    const F7: &str = "\u{f70a}";
    const F8: &str = "\u{f70b}";
    const F9: &str = "\u{f70c}";
    const F10: &str = "\u{f70d}";
    const F11: &str = "\u{f70e}";
    const F12: &str = "\u{f70f}";

    match text {
        RETURN | "\r" => Some(b"\r"),
        BACKSPACE => Some(&[0x7f]),
        DELETE => Some(b"\x1b[3~"),
        TAB => Some(&[0x09]),
        ESCAPE => Some(&[0x1b]),
        UP => Some(b"\x1b[A"),
        DOWN => Some(b"\x1b[B"),
        RIGHT => Some(b"\x1b[C"),
        LEFT => Some(b"\x1b[D"),
        HOME => Some(b"\x1b[H"),
        END => Some(b"\x1b[F"),
        PAGE_UP => Some(b"\x1b[5~"),
        PAGE_DOWN => Some(b"\x1b[6~"),
        // xterm sequences: SS3 for F1–F4, CSI ~ for the rest (issue #319).
        F1 => Some(b"\x1bOP"),
        F2 => Some(b"\x1bOQ"),
        F3 => Some(b"\x1bOR"),
        F4 => Some(b"\x1bOS"),
        F5 => Some(b"\x1b[15~"),
        F6 => Some(b"\x1b[17~"),
        F7 => Some(b"\x1b[18~"),
        F8 => Some(b"\x1b[19~"),
        F9 => Some(b"\x1b[20~"),
        F10 => Some(b"\x1b[21~"),
        F11 => Some(b"\x1b[23~"),
        F12 => Some(b"\x1b[24~"),
        INSERT => Some(b"\x1b[2~"),
        _ => None,
    }
}

pub(crate) fn install_pointer(
    ui: &AppWindow,
    ctx: &Ctx,
    terminal_tab_active: Rc<Cell<bool>>,
    has_selection: Rc<Cell<bool>>,
    selecting: Rc<Cell<bool>>,
    click_tracker: Rc<RefCell<ClickTracker>>,
) {
    let engine = ctx.engine.clone();
    let force_render = ctx.force_render.clone();
    let timer = ctx.timer.clone();
    let timer_fast = ctx.timer_fast.clone();
    let ui_ptr = ui.as_weak();
    ui.on_viewport_pointer(move |x, y, button, kind| {
        let Some(ui) = ui_ptr.upgrade() else {
            return;
        };
        let scale = ui.window().scale_factor().max(0.5) as f32;
        let px = x * scale;
        let py = y * scale;

        // kind: 0=down, 1=up, 2=move; button: 0=left, 1=middle, 2=right
        if kind == 0 && button == 1 {
            // Middle-click paste (Terminal tab only).
            if terminal_tab_active.get() {
                paste_clipboard_to_terminal(&mut engine.borrow_mut());
                force_render.set(true);
                bump_fast_timer(&timer, &timer_fast);
            }
            return;
        }

        if button != 0 {
            return;
        }

        match kind {
            0 => {
                selecting.set(true);
                let click_count = click_tracker.borrow_mut().on_press(px, py);
                let _ = engine.borrow_mut().send_command(Command::SelectionAt {
                    x: px,
                    y: py,
                    extend: false,
                    click_count,
                });
                let selected = engine.borrow().selection_text().is_some();
                has_selection.set(selected);
                ui.set_can_copy(selected);
                force_render.set(true);
                bump_fast_timer(&timer, &timer_fast);
            }
            2 => {
                if !selecting.get() {
                    return;
                }
                let _ = engine.borrow_mut().send_command(Command::SelectionAt {
                    x: px,
                    y: py,
                    extend: true,
                    click_count: 1,
                });
                let selected = engine.borrow().selection_text().is_some();
                has_selection.set(selected);
                ui.set_can_copy(selected);
                force_render.set(true);
                bump_fast_timer(&timer, &timer_fast);
            }
            1 => {
                selecting.set(false);
                // Click (no drag) on an OSC 8 hyperlink opens it.
                if engine.borrow().selection_text().is_none() {
                    let _ = engine
                        .borrow_mut()
                        .send_command(Command::OpenLinkAt { x: px, y: py });
                }
            }
            _ => {}
        }
    });
}

pub(crate) fn install_context_menu(
    ui: &AppWindow,
    ctx: &Ctx,
    terminal_tab_active: Rc<Cell<bool>>,
    has_selection: Rc<Cell<bool>>,
    pty_running: Rc<Cell<bool>>,
) {
    let engine = ctx.engine.clone();
    let ui_ptr = ui.as_weak();
    ui.on_viewport_context_opening(move || {
        let Some(ui) = ui_ptr.upgrade() else {
            return;
        };
        // Viewport context menu: Copy from selection,
        // Paste when Terminal tab + running + clipboard has text.
        let selected = has_selection.get()
            || engine
                .borrow()
                .selection_text()
                .is_some_and(|t| !t.is_empty());
        has_selection.set(selected);
        ui.set_can_copy(selected);
        let can_paste = terminal_tab_active.get() && pty_running.get() && clipboard_has_text();
        ui.set_can_paste(can_paste);
    });
}

pub(crate) fn install_copy_paste(ui: &AppWindow, ctx: &Ctx, terminal_tab_active: Rc<Cell<bool>>) {
    {
        let engine = ctx.engine.clone();
        let ctx = ctx.clone();
        ui.on_viewport_copy(move || {
            if copy_selection_to_clipboard(&engine.borrow()) {
                ctx.refresh();
            }
        });
    }
    {
        let engine = ctx.engine.clone();
        let ctx = ctx.clone();
        let terminal_tab_active = terminal_tab_active.clone();
        ui.on_viewport_paste(move || {
            if !terminal_tab_active.get() {
                return;
            }
            paste_clipboard_to_terminal(&mut engine.borrow_mut());
            ctx.refresh();
        });
    }
}

pub(crate) fn install_key_event(
    ui: &AppWindow,
    ctx: &Ctx,
    terminal_tab_active: Rc<Cell<bool>>,
    has_selection: Rc<Cell<bool>>,
    viewport_font_size: Rc<Cell<f32>>,
    find_resync: Rc<Cell<bool>>,
) {
    let engine = ctx.engine.clone();
    let ctx = ctx.clone();
    let ui_find_key = ui.as_weak();
    ui.on_key_event(move |text, control, meta, _alt, shift| {
        let ctrl = control || meta;
        // Ctrl/Cmd+F → open/focus find (never send 0x06 to PTY).
        if ctrl && is_key_char(&text, 'f') {
            if let Some(ui) = ui_find_key.upgrade() {
                let was_open = ui.get_find_open();
                ui.set_find_open(true);
                ui.set_find_focus_request(true);
                // Resync from engine only when opening — not on every re-focus,
                // or a pending typed query can be overwritten by a stale empty search.
                if !was_open {
                    find_resync.set(true);
                    // Closing Find clears engine search; re-apply the last query
                    // so highlights return with the bar.
                    let query = ui.get_find_query();
                    if !query.is_empty() {
                        let _ = engine.borrow_mut().send_command(Command::SearchSet {
                            query: query.to_string(),
                            regex: ui.get_find_regex(),
                            case_sensitive: ui.get_find_case_sensitive(),
                            whole_word: ui.get_find_whole_word(),
                        });
                    }
                }
            }
            ctx.refresh();
            return true;
        }
        // Escape closes find when open and clears engine search.
        if text == "\u{1b}" {
            if let Some(ui) = ui_find_key.upgrade() {
                if ui.get_find_open() {
                    ui.set_find_open(false);
                    ui.invoke_find_closed();
                    ctx.refresh();
                    return true;
                }
            }
        }
        if ctrl {
            if is_zoom_in_key(&text) {
                let next = (viewport_font_size.get() + 1.0)
                    .clamp(MIN_VIEWPORT_FONT_SIZE, MAX_VIEWPORT_FONT_SIZE);
                apply_zoom(&ctx, &viewport_font_size, next);
                return true;
            }
            if text == "-" {
                let next = (viewport_font_size.get() - 1.0)
                    .clamp(MIN_VIEWPORT_FONT_SIZE, MAX_VIEWPORT_FONT_SIZE);
                apply_zoom(&ctx, &viewport_font_size, next);
                return true;
            }
            if text == "0" {
                apply_zoom(&ctx, &viewport_font_size, DEFAULT_VIEWPORT_FONT_SIZE);
                return true;
            }
        }
        // Copy: Ctrl/Meta+C with an active selection (do not send SIGINT).
        if ctrl
            && is_key_char(&text, 'c')
            && has_selection.get()
            && copy_selection_to_clipboard(&engine.borrow())
        {
            ctx.refresh();
            return true;
        }
        // Paste: Ctrl/Meta+V or Shift+Insert (Terminal tab only).
        let insert = text == "\u{f727}";
        if terminal_tab_active.get() && ((ctrl && is_key_char(&text, 'v')) || (shift && insert)) {
            paste_clipboard_to_terminal(&mut engine.borrow_mut());
            ctx.refresh();
            return true;
        }
        if !terminal_tab_active.get() {
            // Navigation keys scroll the filter/file viewport (issue #83)
            // instead of being swallowed.
            let nav = match text.as_str() {
                "\u{f72c}" => Some(Command::ScrollPage { direction: -1 }), // PageUp
                "\u{f72d}" => Some(Command::ScrollPage { direction: 1 }),  // PageDown
                "\u{f700}" => Some(Command::ScrollLines { delta: -1 }),    // Up
                "\u{f701}" => Some(Command::ScrollLines { delta: 1 }),     // Down
                "\u{f702}" => Some(Command::ScrollHorizontal { delta: -40.0 }), // Left
                "\u{f703}" => Some(Command::ScrollHorizontal { delta: 40.0 }), // Right
                "\u{f729}" => Some(Command::ScrollTo {
                    pos: "start".into(),
                }), // Home
                "\u{f72b}" => Some(Command::ScrollTo { pos: "end".into() }), // End
                _ => None,
            };
            if let Some(cmd) = nav {
                let _ = engine.borrow_mut().send_command(cmd);
                ctx.refresh();
            }
            return true;
        }
        // Do not force_render before echo — paint when PTY content dirties.
        bump_fast_timer(&ctx.timer, &ctx.timer_fast);
        if let Some(ui) = ui_find_key.upgrade() {
            ui.set_caret_blink_on(true);
        }
        handle_key_event(&mut engine.borrow_mut(), &text, ctrl)
    });
}

#[cfg(test)]
mod tests {
    use super::{is_private_use_key, map_special_key, paste_bytes};

    #[test]
    fn paste_translates_line_endings_to_cr() {
        // Issue #70: every pasted line ending must reach the PTY as `\r`.
        assert_eq!(paste_bytes("one\r\ntwo\nthree"), b"one\rtwo\rthree");
        assert_eq!(paste_bytes("lone\rtext"), b"lone\rtext");
        assert_eq!(paste_bytes("no endings"), b"no endings");
        assert_eq!(paste_bytes(""), b"");
    }

    #[test]
    fn special_keys_map_to_xterm_sequences() {
        // Issue #319: Insert and F1–F12 previously fell through the map and
        // their private-use codepoints were written into the PTY as UTF-8
        // mojibake (Insert -> EF 9C A7).
        assert_eq!(map_special_key("\u{f727}"), Some(&b"\x1b[2~"[..]));
        assert_eq!(map_special_key("\u{f704}"), Some(&b"\x1bOP"[..]));
        assert_eq!(map_special_key("\u{f707}"), Some(&b"\x1bOS"[..]));
        assert_eq!(map_special_key("\u{f708}"), Some(&b"\x1b[15~"[..]));
        assert_eq!(map_special_key("\u{f70f}"), Some(&b"\x1b[24~"[..]));
    }

    #[test]
    fn every_private_use_key_is_mapped_or_swallowed() {
        // The guard contract: no private-use key text can reach the
        // `engine.handle_key(text.as_bytes())` fall-through.
        for code in [0xf700u32, 0xf704, 0xf710, 0xf727, 0xf730, 0xf7ff] {
            let text = char::from_u32(code).unwrap().to_string();
            assert!(
                map_special_key(&text).is_some() || is_private_use_key(&text),
                "PUA key U+{code:04X} would leak into the PTY"
            );
        }
        assert!(!is_private_use_key("a"));
        assert!(!is_private_use_key("\u{e000}")); // another PUA block, not Slint keys
    }
}
