//! Real-ConPTY e2e tests for TUI mouse selection (slow tier, `#[ignore]` at
//! birth). Run on a Windows dev host:
//!
//! ```text
//! cargo test -p noviewlog-tui --test conpty_mouse_selection -- --ignored --test-threads=1
//! ```
//!
//! Spawns the real `noviewlog-tui` binary under a portable-pty ConPTY pair,
//! injects SGR mouse sequences exactly as Windows Terminal sends them (the
//! same conhost decoder sits between the terminal and the TUI), and asserts
//! the painted selection highlight and clipboard contents on the decoded
//! screen grid. Reproduction + regression lock for the "selection end snaps
//! to end of line" bug (user report 2026-09-26).
//!
//! Known pre-existing quirk (documented here, deliberately not fixed): the
//! painted highlight end is exclusive while the copied text end is inclusive
//! — one extra char is copied vs highlighted.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use noviewlog_terminal::buffer::RecordBuffer;
use noviewlog_terminal::formats::get_builtin_format;
use noviewlog_terminal::parser::RecordParser;
use noviewlog_terminal::terminal::TerminalIngest;
use noviewlog_terminal::types::FlatLine;
use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

const COLS: u16 = 100;
const ROWS: u16 = 30;
/// TUI content area height (`rows - 3`: tab bar + input + status).
const CONTENT_ROWS: usize = (ROWS - 3) as usize;
/// Selection bg as the emulator decodes crossterm `Color::DarkBlue` (SGR 44):
/// `ansi_basic_color(4, false)` from noviewlog-terminal/src/ansi.rs.
const SELECTION_BG: (u8, u8, u8) = (88, 166, 255);

struct Tui {
    child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    raw: Arc<Mutex<Vec<u8>>>,
    consumed: usize,
    ingest: TerminalIngest,
    buffer: RecordBuffer,
    parser: RecordParser,
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Tui {
    fn spawn() -> Self {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_noviewlog-tui"));
        let child = pair.slave.spawn_command(cmd).expect("spawn noviewlog-tui");
        let mut reader = pair.master.try_clone_reader().expect("pty reader");
        let raw = Arc::new(Mutex::new(Vec::new()));
        let sink = raw.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 8192];
            loop {
                match reader.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => sink.lock().unwrap().extend_from_slice(&chunk[..n]),
                }
            }
        });
        let writer = pair.master.take_writer().expect("pty writer");
        Self {
            child,
            writer,
            raw,
            consumed: 0,
            ingest: TerminalIngest::new_with_size(COLS as usize, ROWS as usize),
            buffer: RecordBuffer::new(10),
            parser: RecordParser::new(get_builtin_format("generic")),
        }
    }

    fn write(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("pty write");
        self.writer.flush().expect("pty flush");
    }

    fn raw_bytes(&self) -> Vec<u8> {
        self.raw.lock().unwrap().clone()
    }

    /// Feed everything the TUI produced since the last call, then decode the
    /// physical screen grid.
    fn grid(&mut self) -> Vec<FlatLine> {
        let (fresh, len) = {
            let buf = self.raw.lock().unwrap();
            (buf[self.consumed..].to_vec(), buf.len())
        };
        self.consumed = len;
        if !fresh.is_empty() {
            self.ingest.feed(&fresh, &mut self.buffer, &mut self.parser);
        }
        self.ingest.grid_flat_lines()
    }

    fn pump(&mut self, mut done: impl FnMut(&[FlatLine]) -> bool, secs: u64) -> bool {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let grid = self.grid();
            if done(&grid) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

struct Scene {
    tui: Tui,
    sentinel: String,
    /// Screen row / column of the sentinel output's first cell.
    row: usize,
    col: usize,
}

/// Spawn the TUI, wait for the running shell, echo a unique sentinel and
/// locate its output row on the grid.
fn scene_with_echoed_sentinel() -> Scene {
    let mut tui = Tui::spawn();
    let running = tui.pump(
        |grid| {
            grid.iter()
                .any(|l| row_text(l).to_lowercase().contains("running"))
        },
        15,
    );
    assert!(running, "TUI must reach the running state");
    let sentinel = format!(
        "NVL{}{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_millis()
    );
    tui.write(format!("echo {sentinel}\r").as_bytes());
    let appeared = tui.pump(|grid| find_sentinel_row(grid, &sentinel).is_some(), 15);
    assert!(appeared, "sentinel output row must appear on the grid");
    let (row, col) = find_sentinel_row(&tui.grid(), &sentinel).expect("sentinel row");
    Scene {
        tui,
        sentinel,
        row,
        col,
    }
}

fn row_text(line: &FlatLine) -> String {
    line.segments.iter().map(|s| s.text.as_str()).collect()
}

/// First content row holding the sentinel output (not the echoed command
/// line): (screen row index, sentinel start screen column).
fn find_sentinel_row(grid: &[FlatLine], sentinel: &str) -> Option<(usize, usize)> {
    for (i, line) in grid.iter().enumerate().skip(1).take(CONTENT_ROWS) {
        let text = row_text(line);
        if text.contains(sentinel) && !text.contains("echo") {
            return Some((i, text.find(sentinel).expect("sentinel substring")));
        }
    }
    None
}

/// Background color of every display cell that starts a character (the
/// emulator skips wide-glyph continuation cells; ASCII rows are 1:1).
fn cell_bgs(line: &FlatLine) -> Vec<Option<(u8, u8, u8)>> {
    let mut out = Vec::new();
    for seg in &line.segments {
        for ch in seg.text.chars() {
            out.push(seg.style.as_ref().and_then(|st| st.bg));
            for _ in 1..noviewlog_terminal::terminal::width::char_width(ch) {
                out.push(None);
            }
        }
    }
    out
}

/// SGR mouse bytes for a TUI content-cell target (row, col), mirroring what
/// Windows Terminal emits: x = col + 3 (1-based + 2 prefix cells),
/// y = row + 2 (1-based + tab bar).
fn mouse_down(row: usize, col: usize) -> Vec<u8> {
    format!("\x1b[<0;{};{}M", col + 3, row + 2).into_bytes()
}

fn mouse_drag(row: usize, col: usize) -> Vec<u8> {
    format!("\x1b[<32;{};{}M", col + 3, row + 2).into_bytes()
}

fn mouse_up(row: usize, col: usize) -> Vec<u8> {
    format!("\x1b[<0;{};{}m", col + 3, row + 2).into_bytes()
}

/// Drag from the sentinel start to `k` cells past it (screen coords) and
/// release, via ≥3 intermediate drag positions.
fn drag_select(tui: &mut Tui, row: usize, col: usize, k: usize) {
    let (r, c) = (row - 1, col - 2);
    tui.write(&mouse_down(r, c));
    for mid in [c + 1, c + 2, c + 4, c + k / 2] {
        tui.write(&mouse_drag(r, mid));
    }
    tui.write(&mouse_up(r, c + k));
}

/// Cells `[col, col + k)` of `row` must carry the selection bg, every other
/// cell of the row must not.
fn assert_highlight_extent(grid: &[FlatLine], row: usize, col: usize, k: usize) {
    let line = &grid[row];
    let text = row_text(line);
    let bgs = cell_bgs(line);
    assert!(
        bgs.len() >= col + k,
        "row {row} too short for the selection: {text:?}"
    );
    for (i, bg) in bgs.iter().enumerate() {
        let want = if (col..col + k).contains(&i) {
            Some(SELECTION_BG)
        } else {
            None
        };
        assert_eq!(*bg, want, "row {row} cell {i} bg (row text: {text:?})");
    }
}

fn base64(data: &[u8]) -> String {
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

/// OSC 52 payload the TUI emits as clipboard fallback for `text`.
fn osc52_marker(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn raw_contains_osc52(raw: &[u8], text: &str) -> bool {
    let needle = osc52_marker(text);
    raw.len() >= needle.len() && raw.windows(needle.len()).any(|w| w == needle.as_bytes())
}

fn read_clipboard_now() -> Option<String> {
    let mut cb = arboard::Clipboard::new().ok()?;
    cb.get_text().ok()
}

fn set_clipboard(text: &str) -> bool {
    match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(text.to_owned()).is_ok(),
        Err(_) => false,
    }
}

/// Wait until the clipboard holds `expected` exactly (arboard), falling back
/// to the TUI's OSC 52 output when arboard is unavailable in this environment.
fn wait_clipboard(tui: &Tui, expected: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if read_clipboard_now().as_deref() == Some(expected) {
            return true;
        }
        if raw_contains_osc52(&tui.raw_bytes(), expected) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore = "slow tier: real ConPTY spawn; run with -- --ignored --test-threads=1"]
fn drag_selects_exact_span() {
    let mut scene = scene_with_echoed_sentinel();
    let k = scene.sentinel.chars().count() / 2;
    let (row, col) = (scene.row, scene.col);
    drag_select(&mut scene.tui, row, col, k);
    // Settle: the injected events are queued synchronously; wait until the
    // TUI has processed the release so the final (not mid-drag) frame is
    // asserted. Live drag frames may still show the un-fixed EOL stretch.
    std::thread::sleep(Duration::from_millis(500));
    let grid = scene.tui.grid();
    assert!(
        cell_bgs(&grid[row]).contains(&Some(SELECTION_BG)),
        "no selection highlight appeared at all (row {row} text: {:?})",
        row_text(&grid[row])
    );
    assert_highlight_extent(&grid, row, col, k);
}

#[test]
#[ignore = "slow tier: real ConPTY spawn; run with -- --ignored --test-threads=1"]
fn release_copies_selected_text() {
    let mut scene = scene_with_echoed_sentinel();
    let len = scene.sentinel.chars().count();
    let k = len / 2;
    let (row, col) = (scene.row, scene.col);
    drag_select(&mut scene.tui, row, col, k);
    // Copied text is end-inclusive (known quirk): k + 1 chars.
    let expected: String = scene.sentinel.chars().take(k + 1).collect();
    assert!(
        wait_clipboard(&scene.tui, &expected),
        "clipboard must hold {expected:?} after mouse-up (got {:?})",
        read_clipboard_now()
    );
}

#[test]
#[ignore = "slow tier: real ConPTY spawn; run with -- --ignored --test-threads=1"]
fn click_without_drag_copies_nothing() {
    let mut scene = scene_with_echoed_sentinel();
    // The would-be product copy on a wrongful click: the release-point span
    // prefix (end-inclusive quirk). Asserting "clipboard != this exact
    // value" — not "clipboard == what we set" — keeps the test immune to
    // external clipboard writers (the terminal itself syncs the shared
    // system clipboard), which produced a one-off false failure (#268).
    let len = scene.sentinel.chars().count();
    let wrong_copy: String = scene.sentinel.chars().take(len / 2 + 1).collect();
    let (row, col) = (scene.row, scene.col);
    let (r, c) = (row - 1, col - 2);
    // Make sure the would-be copy differs from whatever is on the clipboard
    // right now, so the assertion below has teeth.
    if read_clipboard_now().as_deref() == Some(wrong_copy.as_str()) {
        assert!(set_clipboard(&format!("{wrong_copy}#")), "set clipboard");
    }
    scene.tui.write(&mouse_down(r, c));
    scene.tui.write(&mouse_up(r, c));
    // Give a (wrong) copy or highlight time to land before asserting.
    std::thread::sleep(Duration::from_millis(500));
    let grid = scene.tui.grid();
    for (i, line) in grid.iter().enumerate() {
        assert!(
            !cell_bgs(line).contains(&Some(SELECTION_BG)),
            "single click must not paint a selection (row {i})"
        );
    }
    let copied = match read_clipboard_now() {
        Some(text) => text == wrong_copy,
        // Clipboard read failed (locked by another app): fall back to the
        // TUI's OSC 52 output as the copy signal.
        None => raw_contains_osc52(&scene.tui.raw_bytes(), &scene.sentinel),
    };
    assert!(
        !copied,
        "single click must not copy the selection prefix ({wrong_copy:?})"
    );
}
