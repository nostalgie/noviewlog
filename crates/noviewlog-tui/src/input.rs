//! Pure key/menu/clipboard helpers for the TUI host (unit-tested without a PTY).

use std::io::{stdout, Write};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use noviewlog_core::Command;

/// Inner width of the context menu box (see [`crate::App::menu_width`]).
pub(crate) const MENU_WIDTH: u16 = 24;
/// Full menu box width: inner width plus the left/right border columns.
/// The click position clamps to `cols - MENU_BOX_COLS` so the box fits.
pub(crate) const MENU_BOX_COLS: u16 = MENU_WIDTH + 2;

/// Part of `text` covering display-cell range `[start, end]` (end cell
/// inclusive). Selection coordinates are cells, not char indices: wide CJK
/// glyphs take two cells and zero-width marks take none, so chars are
/// included by the cell their glyph starts at (#254). Pure-ASCII lines keep
/// the exact span the old char-index slice produced.
pub(crate) fn text_in_cells(text: &str, start: usize, end: usize) -> String {
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
pub(crate) fn caret_x(c: usize, cols: u16) -> u16 {
    (c.min(usize::from(cols.saturating_sub(3))) as u16 + 2).min(cols.saturating_sub(1))
}

/// Paste cap for the filter buffer: a multi-megabyte clipboard paste must
/// not balloon the input line (the frame paints the whole buffer).
pub(crate) const FILTER_BUF_CAP: usize = 64 * 1024;

/// Append pasted text to the filter buffer up to [`FILTER_BUF_CAP`],
/// stopping on a char boundary (never a partial UTF-8 tail).
pub(crate) fn paste_append(buf: &mut String, text: &str) {
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
pub(crate) fn menu_items(collapsed: bool) -> Vec<&'static str> {
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
pub(crate) fn menu_hit(menu_row: u16, row: u16, items: usize) -> Option<usize> {
    if row <= menu_row {
        return None;
    }
    let idx = (row - menu_row - 1) as usize;
    (idx <= items).then_some(idx)
}

/// Move the connect overlay selection by `delta` (clamped to `[0, count)`),
/// staying put on an empty list or an out-of-range start.
pub(crate) fn connect_sel_move(sel: usize, delta: i32, count: usize) -> usize {
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
pub(crate) fn key_command(key: &KeyEvent, tab_count: usize, active_tab: usize) -> Option<Command> {
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
pub(crate) fn shell_key_bytes(key: &KeyEvent) -> Vec<u8> {
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
pub(crate) fn copy_to_clipboard(text: &str) {
    let mut out = stdout();
    copy_with_fallback(&mut out, text, CLIPBOARD_WAIT, spawn_arboard_worker);
}

/// Spawn the native clipboard attempt on a throwaway thread; the receiver
/// yields `true` on success, `false` on failure.
pub(crate) fn spawn_arboard_worker(text: &str) -> std::sync::mpsc::Receiver<bool> {
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
pub(crate) fn copy_with_fallback<W: Write, F>(
    out: &mut W,
    text: &str,
    wait: Duration,
    spawn_worker: F,
) -> bool
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

pub(crate) fn base64_encode(data: &[u8]) -> String {
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
