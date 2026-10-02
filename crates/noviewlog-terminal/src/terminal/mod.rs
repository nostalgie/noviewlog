//! Terminal SCREEN + SCROLLBACK emulation (**live VT layer**).
//!
//! # Dual ANSI stack (read this before changing color / escape handling)
//!
//! | Layer | Module | Owns |
//! |-------|--------|------|
//! | **Live VT** (this file) | `core::terminal` | `vte` grid + scrollback; cursor, erase, OSC 7 |
//! | **Line SGR** | [`crate::ansi`] | Parse/strip/overlay SGR on stored record lines |
//!
//! This module owns the live cell grid. Committed (scrolled-off) rows are
//! serialized to ANSI for the Record buffer. The live screen is exposed as
//! overlay [`FlatLine`]s built from cells (no Record round-trip). Filter/display
//! coloring of stored lines lives in `ansi.rs`.
//!
//! ora / listr2 / ink render progress by manipulating the terminal *screen*
//! (cursor up/down, erase line, carriage return, redraw a block of lines).
//! A line-oriented buffer with `\r`/CSI collapse heuristics cannot reproduce
//! this — it either glues frames together or loses finalized lines.
//!
//! This module models a real terminal: a fixed-height grid the cursor moves
//! around on, plus a scrollback of lines that have scrolled off the top. Lines
//! that scroll off are *committed* permanently to the log buffer; the active
//! on-screen region is rendered live and repaints in place, so spinners replace
//! correctly while every finalized line (tables, ✔ steps, banners) survives.

mod screen;
pub mod width;

use std::sync::Arc;

use vte::{Params, Parser, Perform};

use crate::buffer::RecordBuffer;
use crate::parser::RecordParser;
use crate::types::FlatLine;
use screen::{Pen, DEFAULT_COLS, DEFAULT_ROWS};

/// Caret position within [`TerminalEmulator::screen_lines`] (logical rows).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenCursor {
    /// Index into the logical screen-line list (joined wraps).
    pub line: usize,
    /// Cell column within that logical line (may past end of trimmed text).
    pub col: usize,
}

/// A terminal screen with scrollback commit + colour-preserving serialization.
pub struct TerminalEmulator {
    cols: usize,
    rows: usize,
    screen: Vec<screen::Row>,
    cursor_row: usize,
    cursor_col: usize,
    cursor_visible: bool,
    pen: Pen,
    /// Pending auto-wrap: next print wraps first (DEC autowrap).
    wrap_pending: bool,
    /// Finalized lines that scrolled off the top, ready to drain.
    committed: Vec<String>,
    /// Accumulator for a logical line whose rows are still auto-wrapping.
    wrap_accum: Option<String>,
    /// Latest OSC 7 working directory (file://…), if reported by the shell.
    pending_cwd: Option<String>,
}

impl Default for TerminalEmulator {
    fn default() -> Self {
        Self::new(DEFAULT_COLS, DEFAULT_ROWS)
    }
}

impl TerminalEmulator {
    pub fn new(cols: usize, rows: usize) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        TerminalEmulator {
            cols,
            rows,
            screen: vec![screen::Row::new(cols); rows],
            cursor_row: 0,
            cursor_col: 0,
            cursor_visible: true,
            pen: Pen::default(),
            wrap_pending: false,
            committed: Vec::new(),
            wrap_accum: None,
            pending_cwd: None,
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Drain lines that have scrolled off the screen (finalized).
    pub fn take_committed(&mut self) -> Vec<String> {
        std::mem::take(&mut self.committed)
    }

    pub fn take_pending_cwd(&mut self) -> Option<String> {
        self.pending_cwd.take()
    }

    pub fn cursor_visible(&self) -> bool {
        self.cursor_visible
    }

    /// Map the grid cursor into the logical screen-line coordinate system
    /// (auto-wrapped rows joined), matching [`Self::screen_lines`].
    pub fn screen_cursor(&self) -> ScreenCursor {
        let mut line = 0usize;
        let mut col_base = 0usize;
        for r in 0..self.cursor_row {
            if self.screen[r].wrapped {
                col_base += self.cols;
            } else {
                line += 1;
                col_base = 0;
            }
        }
        ScreenCursor {
            line,
            col: col_base + self.cursor_col.min(self.cols.saturating_sub(1)),
        }
    }
}

fn first_param(params: &Params) -> u16 {
    params
        .iter()
        .next()
        .and_then(|p| p.first().copied())
        .unwrap_or(0)
}

fn nth_param(params: &Params, n: usize) -> u16 {
    params
        .iter()
        .nth(n)
        .and_then(|p| p.first().copied())
        .unwrap_or(0)
}

impl Perform for TerminalEmulator {
    fn print(&mut self, c: char) {
        self.write_char(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => {
                self.wrap_pending = false;
                self.line_feed();
            }
            b'\r' => {
                self.wrap_pending = false;
                self.cursor_col = 0;
            }
            b'\t' => {
                let next = ((self.cursor_col / 8) + 1) * 8;
                self.cursor_col = next.min(self.cols - 1);
                self.wrap_pending = false;
            }
            0x08 => {
                self.cursor_col = self.cursor_col.saturating_sub(1);
                self.wrap_pending = false;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // Most DEC private modes (bracketed paste, etc.) are ignored; cursor
        // visibility (`?25`) is tracked so the viewport caret can hide.
        let private = intermediates.contains(&b'?');
        match action {
            'm' if !private => {
                let mut codes: Vec<u16> = Vec::new();
                for p in params.iter() {
                    if p.is_empty() {
                        codes.push(0);
                    } else {
                        codes.extend_from_slice(p);
                    }
                }
                self.apply_sgr(&codes);
            }
            'h' if private => {
                if first_param(params) == 25 {
                    self.cursor_visible = true;
                }
            }
            'l' if private => {
                if first_param(params) == 25 {
                    self.cursor_visible = false;
                }
            }
            'A' => {
                let n = first_param(params).max(1) as usize;
                self.cursor_row = self.cursor_row.saturating_sub(n);
                self.wrap_pending = false;
            }
            'B' | 'e' => {
                let n = first_param(params).max(1) as usize;
                self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
                self.wrap_pending = false;
            }
            'C' | 'a' => {
                let n = first_param(params).max(1) as usize;
                self.cursor_col = (self.cursor_col + n).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'D' => {
                let n = first_param(params).max(1) as usize;
                self.cursor_col = self.cursor_col.saturating_sub(n);
                self.wrap_pending = false;
            }
            'E' => {
                let n = first_param(params).max(1) as usize;
                self.cursor_row = (self.cursor_row + n).min(self.rows - 1);
                self.cursor_col = 0;
                self.wrap_pending = false;
            }
            'F' => {
                let n = first_param(params).max(1) as usize;
                self.cursor_row = self.cursor_row.saturating_sub(n);
                self.cursor_col = 0;
                self.wrap_pending = false;
            }
            'G' | '`' => {
                let n = first_param(params).max(1) as usize;
                self.cursor_col = (n - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'd' => {
                let n = first_param(params).max(1) as usize;
                self.cursor_row = (n - 1).min(self.rows - 1);
                self.wrap_pending = false;
            }
            'H' | 'f' => {
                let r = nth_param(params, 0).max(1) as usize;
                let c = nth_param(params, 1).max(1) as usize;
                self.cursor_row = (r - 1).min(self.rows - 1);
                self.cursor_col = (c - 1).min(self.cols - 1);
                self.wrap_pending = false;
            }
            'J' => self.erase_display(first_param(params)),
            'K' => self.erase_line(first_param(params)),
            // Character edit — required for readline mid-line Delete (CSI 3~ → DCH).
            'P' if !private => {
                let n = first_param(params).max(1) as usize;
                self.delete_chars(n);
            }
            '@' if !private => {
                let n = first_param(params).max(1) as usize;
                self.insert_chars(n);
            }
            'X' if !private => {
                let n = first_param(params).max(1) as usize;
                self.erase_chars(n);
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, byte: u8) {
        match byte {
            b'D' => {
                self.wrap_pending = false;
                self.line_feed();
            }
            b'M' => {
                // Reverse index: cursor up, scroll down at the top row (DEC RI).
                self.wrap_pending = false;
                if self.cursor_row == 0 {
                    self.scroll_down();
                } else {
                    self.cursor_row -= 1;
                }
            }
            b'E' => {
                self.wrap_pending = false;
                self.cursor_col = 0;
                self.line_feed();
            }
            b'c' => {
                // RIS full reset.
                self.flush_all();
                self.pen = Pen::default();
                for r in 0..self.rows {
                    self.screen[r].clear();
                }
            }
            _ => {}
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 7 ; <uri> — shell reports current working directory.
        if params.first().is_some_and(|p| p == b"7") {
            if let Some(uri) = params.get(1) {
                if let Ok(s) = std::str::from_utf8(uri) {
                    if let Some(path) = parse_osc7_cwd(s) {
                        self.pending_cwd = Some(path);
                    }
                }
            }
            return;
        }
        // OSC 8 ; params ; uri — hyperlink start; empty uri closes it.
        // URIs may contain ';', so rejoin everything after the params field.
        if params.first().is_some_and(|p| p == b"8") && params.len() >= 2 {
            let mut uri = String::new();
            for (i, part) in params.iter().enumerate().skip(2) {
                if i > 2 {
                    uri.push(';');
                }
                if let Ok(s) = std::str::from_utf8(part) {
                    uri.push_str(s);
                }
            }
            self.pen.link = if uri.is_empty() {
                None
            } else {
                Some(Arc::from(uri))
            };
        }
    }
}

/// Parse OSC 7 payloads like `file://hostname/path` or plain paths into a local path.
pub fn parse_osc7_cwd(payload: &str) -> Option<String> {
    let payload = payload.trim();
    if payload.is_empty() {
        return None;
    }
    if let Some(rest) = payload.strip_prefix("file://") {
        let path = if let Some(slash) = rest.find('/') {
            &rest[slash..]
        } else {
            rest
        };
        let decoded = percent_decode_basic(path);
        if decoded.is_empty() {
            return None;
        }
        return Some(decoded);
    }
    if payload.starts_with('/') || payload.starts_with('~') {
        return Some(payload.to_string());
    }
    None
}

fn percent_decode_basic(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (from_hex(bytes[i + 1]), from_hex(bytes[i + 2])) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn from_hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Drives the [`TerminalEmulator`] into a [`RecordBuffer`] + [`RecordParser`].
///
/// Committed (scrolled-off) rows flow through the parser so multiline records
/// (stack traces) still group and filters apply. The live on-screen region
/// stays in the emulator grid. Follow paints that grid directly; overlay
/// [`FlatLine`]s are built only when the Terminal tab must show scrollback
/// (scrolled up, search, process exit).
pub struct TerminalIngest {
    parser: Parser,
    emu: TerminalEmulator,
}

impl Default for TerminalIngest {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalIngest {
    pub fn new() -> Self {
        TerminalIngest {
            parser: Parser::new(),
            emu: TerminalEmulator::default(),
        }
    }

    pub fn new_with_size(cols: usize, rows: usize) -> Self {
        TerminalIngest {
            parser: Parser::new(),
            emu: TerminalEmulator::new(cols, rows),
        }
    }

    /// Match emulator geometry to the viewport / PTY. May commit rows that scroll off.
    pub fn resize(
        &mut self,
        cols: usize,
        rows: usize,
        buffer: &mut RecordBuffer,
        parser: &mut RecordParser,
    ) {
        if cols == self.emu.cols() && rows == self.emu.rows() {
            return;
        }
        parser.begin_chunk();
        self.emu.resize(cols, rows);
        let _ = self.commit_available(buffer, parser);
    }

    fn commit_available(
        &mut self,
        buffer: &mut RecordBuffer,
        parser: &mut RecordParser,
    ) -> (usize, usize) {
        let mut shifted_records = 0usize;
        let mut shifted_lines = 0usize;
        let mut committed = false;
        for line in self.emu.take_committed() {
            committed = true;
            for record in parser.push_line(line) {
                let (records, lines) = buffer.add_counting(record);
                shifted_records += records;
                shifted_lines += lines;
            }
        }
        // Scrolled-off lines that are still "open" in the RecordParser would
        // otherwise sit invisible in pending until the next line or idle_flush
        // (~120ms) — Follow sees them vanish and reappear. Flush immediately.
        // Only when rows actually scrolled off: flushing on every chunk would
        // split a multiline record arriving across successive PTY reads
        // (the normal case for slow output) into one record per chunk.
        if committed {
            if let Some(rec) = parser.flush_pending() {
                let (records, lines) = buffer.add_counting(rec);
                shifted_records += records;
                shifted_lines += lines;
            }
        }
        (shifted_records, shifted_lines)
    }

    /// Feed a raw PTY byte chunk. Returns `(records, raw lines)` dropped from
    /// the ring (scrollback trim). A multiline record dropped by the ring
    /// removes one record but several flat lines — callers patching the
    /// flat-line prefix vs the record cursor need each count in its own space.
    pub fn feed(
        &mut self,
        bytes: &[u8],
        buffer: &mut RecordBuffer,
        parser: &mut RecordParser,
    ) -> (usize, usize) {
        parser.begin_chunk();
        self.parser.advance(&mut self.emu, bytes);
        self.commit_available(buffer, parser)
    }

    pub fn take_cwd_update(&mut self) -> Option<String> {
        self.emu.take_pending_cwd()
    }

    /// Flush a still-pending parser record. Called on idle so the
    /// most-recently-scrolled-off line is not stuck pending.
    pub fn idle_flush(&mut self, buffer: &mut RecordBuffer, parser: &mut RecordParser) -> bool {
        if !parser.has_pending() {
            return false;
        }
        parser.begin_chunk();
        if let Some(rec) = parser.flush_pending() {
            buffer.add(rec);
        }
        true
    }

    /// Finalize at process exit: commit the whole screen permanently.
    pub fn finish(&mut self, buffer: &mut RecordBuffer, parser: &mut RecordParser) {
        parser.begin_chunk();
        self.emu.flush_all();
        let _ = self.commit_available(buffer, parser);
        if let Some(rec) = parser.flush_pending() {
            buffer.add(rec);
        }
    }

    pub fn reset(&mut self) {
        self.reset_with_size(DEFAULT_COLS, DEFAULT_ROWS);
    }

    pub fn reset_with_size(&mut self, cols: usize, rows: usize) {
        self.parser = Parser::new();
        self.emu = TerminalEmulator::new(cols, rows);
    }

    pub fn size(&self) -> (usize, usize) {
        (self.emu.cols(), self.emu.rows())
    }

    /// Current VT screen as painted lines (Follow live grid), including the
    /// in-progress row. Used by tests that must not wait for Record commit.
    pub fn live_screen_lines(&self) -> Vec<String> {
        self.emu.screen_lines()
    }

    /// Ensure the live screen exists even before the first PTY byte (empty
    /// grid with a visible caret at 0,0). Does not write Records.
    pub fn ensure_live_screen(&mut self, _buffer: &mut RecordBuffer) {}

    /// Number of live overlay lines (not stored in the Record buffer).
    pub fn volatile_count(&self) -> usize {
        self.emu.overlay_flat_lines().len()
    }

    /// Live overlay `FlatLine`s for scrollback composition (not the Follow paint path).
    pub fn overlay_flat_lines(&self) -> Vec<FlatLine> {
        self.emu.overlay_flat_lines()
    }

    /// Physical VT grid as FlatLines — Follow Viewport paint.
    pub fn grid_flat_lines(&self) -> Vec<FlatLine> {
        self.emu.grid_flat_lines()
    }

    /// Caret cell on the physical grid (`row`, `col`).
    pub fn grid_caret(&self) -> Option<(usize, usize)> {
        if !self.emu.cursor_visible() {
            return None;
        }
        Some(self.emu.grid_cursor())
    }

    /// Caret for scrolled-up overlay mapping (logical screen lines).
    /// `None` when the cursor is hidden (`CSI ?25l`).
    pub fn viewport_caret(&self) -> Option<ScreenCursor> {
        if !self.emu.cursor_visible() {
            return None;
        }
        Some(self.emu.screen_cursor())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::RecordBuffer;
    use crate::formats::get_builtin_format;
    use crate::parser::RecordParser;

    fn feed(emu: &mut TerminalEmulator, bytes: &[u8]) {
        let mut parser = Parser::new();
        parser.advance(emu, bytes);
    }

    #[test]
    fn cursor_advances_on_print() {
        let mut emu = TerminalEmulator::new(80, 24);
        feed(&mut emu, b"hi");
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 2 });
        assert_eq!(emu.screen_lines(), vec!["hi".to_string()]);
    }

    #[test]
    fn cursor_moves_with_csi_and_backspace() {
        let mut emu = TerminalEmulator::new(80, 24);
        feed(&mut emu, b"abc\x08\x08");
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 1 });
        feed(&mut emu, b"\x1b[C\x1b[C");
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 3 });
        feed(&mut emu, b"\x1b[5;5H");
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 4, col: 4 });
    }

    #[test]
    fn screen_lines_keep_blank_row_for_cursor() {
        let mut emu = TerminalEmulator::new(80, 24);
        feed(&mut emu, b"one\r\n\x1b[2;1H");
        let lines = emu.screen_lines();
        assert!(lines.len() >= 2, "expected cursor row kept: {lines:?}");
        assert_eq!(lines[0], "one");
        assert_eq!(emu.screen_cursor().line, 1);
    }

    #[test]
    fn cursor_visibility_dec_mode() {
        let mut emu = TerminalEmulator::new(80, 24);
        assert!(emu.cursor_visible());
        feed(&mut emu, b"\x1b[?25l");
        assert!(!emu.cursor_visible());
        feed(&mut emu, b"\x1b[?25h");
        assert!(emu.cursor_visible());
    }

    #[test]
    fn reverse_index_at_top_scrolls_down() {
        // Issue #236: DEC RI with the cursor on the top row must scroll the
        // grid down one line, like index at the bottom scrolls up.
        let mut emu = TerminalEmulator::new(80, 4);
        feed(&mut emu, b"top\r\nbottom");
        feed(&mut emu, b"\x1b[1;1H\x1bM");
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 0 });
        let lines = emu.screen_lines();
        assert_eq!(lines[0], "");
        assert_eq!(lines[1], "top");
        assert_eq!(lines[2], "bottom");
    }

    #[test]
    fn reverse_index_below_top_moves_cursor_up() {
        let mut emu = TerminalEmulator::new(80, 4);
        feed(&mut emu, b"top\r\nbottom");
        feed(&mut emu, b"\x1bM");
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 6 });
    }

    #[test]
    fn dch_shifts_line_left_like_bash_delete() {
        // Bash mid-line Delete emits CSI 1 P, then may reprint the shifted
        // glyph and BS (`\x1b[1Pt\x08`). Without DCH that becomes `tett`.
        let mut emu = TerminalEmulator::new(80, 24);
        feed(&mut emu, b"test");
        feed(&mut emu, b"\x08\x08");
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 2 });
        feed(&mut emu, b"\x1b[1Pt\x08");
        assert_eq!(emu.screen_lines(), vec!["tet".to_string()]);
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 2 });
    }

    #[test]
    fn ich_and_ech_edit_cells() {
        let mut emu = TerminalEmulator::new(80, 24);
        feed(&mut emu, b"abcd\x08\x08\x08"); // cursor on 'b'
        feed(&mut emu, b"\x1b[@");
        assert_eq!(emu.screen_lines(), vec!["a bcd".to_string()]);
        feed(&mut emu, b"\x1b[P");
        assert_eq!(emu.screen_lines(), vec!["abcd".to_string()]);

        let mut emu = TerminalEmulator::new(80, 24);
        feed(&mut emu, b"abc\x08\x08"); // cursor on 'b'
        feed(&mut emu, b"\x1b[2X");
        assert_eq!(emu.screen_lines(), vec!["a".to_string()]);
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 1 });
    }

    #[test]
    fn osc8_grid_roundtrip_through_serialization() {
        let mut emu = TerminalEmulator::new(80, 24);
        feed(
            &mut emu,
            b"\x1b]8;;https://example.com\x07visit\x1b]8;;\x07 ok",
        );
        // Serialize → line parser must restore the link style on "visit" only.
        let segs = crate::ansi::parse_ansi_line(&emu.screen_lines()[0]);
        assert_eq!(
            segs.iter().map(|s| s.text.as_str()).collect::<String>(),
            "visit ok"
        );
        let linked: Vec<&str> = segs
            .iter()
            .filter(|s| s.style.as_ref().is_some_and(|st| st.link.is_some()))
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(linked, vec!["visit"]);
        // Overlay/grid FlatLines carry the link in segment styles.
        let flat = emu.overlay_flat_lines();
        assert!(flat[0].segments.iter().any(|s| {
            s.text == "visit" && s.style.as_ref().is_some_and(|st| st.link.is_some())
        }));
    }

    #[test]
    fn osc8_reopen_replaces_uri() {
        let mut emu = TerminalEmulator::new(80, 24);
        feed(
            &mut emu,
            b"\x1b]8;;https://a\x07A\x1b]8;;https://b\x07B\x1b]8;;\x07",
        );
        let segs = crate::ansi::parse_ansi_line(&emu.screen_lines()[0]);
        let a = segs.iter().find(|s| s.text == "A").unwrap();
        let b = segs.iter().find(|s| s.text == "B").unwrap();
        assert_eq!(a.style.as_ref().unwrap().link.as_deref(), Some("https://a"));
        assert_eq!(b.style.as_ref().unwrap().link.as_deref(), Some("https://b"));
    }

    #[test]
    fn ingest_exposes_viewport_caret() {
        let mut ingest = TerminalIngest::new();
        let mut buffer = RecordBuffer::new(1000);
        let mut parser = RecordParser::new(get_builtin_format("node-default"));
        ingest.feed(b"$ hello", &mut buffer, &mut parser);
        let caret = ingest.viewport_caret().expect("visible");
        assert_eq!(caret, ScreenCursor { line: 0, col: 7 });
        assert_eq!(ingest.volatile_count(), 1);
    }

    #[test]
    fn resize_widens_autowrap_column() {
        let mut emu = TerminalEmulator::new(10, 5);
        feed(&mut emu, b"abcdefghijXYZ");
        // At 10 cols the "XYZ" starts a wrapped continuation row.
        assert!(emu.screen_lines()[0].len() >= 10);
        emu.resize(40, 5);
        assert_eq!(emu.cols(), 40);
        assert_eq!(emu.rows(), 5);
        // Reflow must keep one logical line (not fragment at the old wrap).
        assert_eq!(
            emu.screen_lines(),
            vec!["abcdefghijXYZ".to_string()],
            "column resize should reflow auto-wrapped content"
        );
        // After widen, new prints should not wrap at the old 10-col boundary.
        feed(&mut emu, b"\r\n");
        feed(&mut emu, b"12345678901234567890".as_ref());
        let lines = emu.screen_lines();
        assert!(
            lines.iter().any(|l| l.contains("12345678901234567890")),
            "expected full 20-char line without 10-col hard wrap: {lines:?}"
        );
    }

    #[test]
    fn resize_narrows_reflows_logical_line() {
        let mut emu = TerminalEmulator::new(40, 5);
        feed(&mut emu, b"abcdefghijklmnopqrstuvwxyz");
        assert_eq!(emu.screen_lines().len(), 1);
        emu.resize(10, 8);
        let lines = emu.screen_lines();
        assert_eq!(
            lines.len(),
            1,
            "still one logical line after narrow: {lines:?}"
        );
        assert_eq!(lines[0], "abcdefghijklmnopqrstuvwxyz");
    }

    #[test]
    fn ingest_resize_updates_size() {
        let mut ingest = TerminalIngest::new_with_size(80, 24);
        let mut buffer = RecordBuffer::new(1000);
        let mut parser = RecordParser::new(get_builtin_format("node-default"));
        assert_eq!(ingest.size(), (80, 24));
        ingest.resize(160, 48, &mut buffer, &mut parser);
        assert_eq!(ingest.size(), (160, 48));
    }

    #[test]
    fn scrolled_off_line_is_committed_immediately() {
        // Tiny grid so the next LF scrolls the top line off. That line must
        // land in the buffer in the same feed() — not sit invisible in parser
        // pending until idle_flush (which caused disappear/reappear jumps).
        let mut ingest = TerminalIngest::new_with_size(40, 2);
        let mut buffer = RecordBuffer::new(1000);
        let mut parser = RecordParser::new(get_builtin_format("node-default"));

        ingest.feed(b"[LOG] first\r\n[LOG] second\r\n", &mut buffer, &mut parser);
        // Third line scrolls "first" off the 2-row screen.
        ingest.feed(b"[LOG] third\r\n", &mut buffer, &mut parser);

        assert!(
            !parser.has_pending(),
            "scrolled-off lines must not remain only in parser pending"
        );
        let committed: Vec<String> = buffer
            .records()
            .iter()
            .flat_map(|r| r.lines.iter().cloned())
            .map(|l| crate::ansi::strip_ansi(&l))
            .collect();
        assert!(
            committed.iter().any(|l| l.contains("[LOG] first")),
            "scrolled-off line must be in buffer immediately, got {committed:?}"
        );
        assert!(
            committed.iter().any(|l| l.contains("[LOG] second")),
            "expected second line committed, got {committed:?}"
        );
        assert!(
            !committed.iter().any(|l| l.contains("[LOG] third")),
            "live screen line must not be a Record, got {committed:?}"
        );
        assert_eq!(ingest.volatile_count(), ingest.overlay_flat_lines().len());
        assert_eq!(ingest.grid_flat_lines().len(), ingest.size().1);
    }

    #[test]
    fn resize_preserves_cell_colors() {
        // Issue #79: a column resize must not strip SGR from the live grid.
        let mut emu = TerminalEmulator::new(80, 24);
        feed(&mut emu, b"plain \x1b[31mred\x1b[0m plain\r\nsecond");
        assert_eq!(emu.plain_screen_lines()[0], "plain red plain");

        emu.resize(120, 24);

        // The reflowed grid keeps the colored run: cells under "red" carry
        // the red pen, surrounding text stays default.
        let (segments, _) = TerminalEmulator::row_to_segments(&emu.screen[0]);
        let red = segments
            .iter()
            .find(|s| s.text.contains("red"))
            .expect("red segment survives resize");
        assert_eq!(
            red.style.as_ref().and_then(|s| s.fg),
            Some((248, 81, 73)),
            "red fg preserved: {:?}",
            red.style
        );
    }

    #[test]
    fn colon_sgr_subparameters_match_semicolon_form() {
        // Issue #77: `38:5:n` / `38:2:r:g:b` (colon sub-params) must color
        // identically to the semicolon form; `4:3` underlines.
        let mut emu = TerminalEmulator::new(80, 24);
        feed(
            &mut emu,
            b"\x1b[38:5:196midx\x1b[0m \x1b[38:2:10:200:30mMrgb\x1b[0m \x1b[4:3mu\x1b[0m",
        );
        let (segments, _) = TerminalEmulator::row_to_segments(&emu.screen[0]);
        let find = |needle: &str| {
            segments
                .iter()
                .find(|s| s.text == needle)
                .unwrap_or_else(|| panic!("segment {needle} in {segments:?}"))
        };
        let idx = find("idx");
        let rgb = find("Mrgb").style.as_ref().and_then(|s| s.fg);
        let underline = find("u").style.as_ref().map(|s| s.underline);
        assert_eq!(
            idx.style.as_ref().and_then(|s| s.fg),
            Some((255, 0, 0)),
            "256-color colon form (196 → basic red 9): {:?}",
            idx.style
        );
        assert_eq!(rgb, Some((10, 200, 30)), "truecolor colon form");
        assert_eq!(underline, Some(true), "4:3 underlines");
    }

    #[test]
    fn echo_does_not_create_records() {
        let mut ingest = TerminalIngest::new_with_size(80, 24);
        let mut buffer = RecordBuffer::new(1000);
        let mut parser = RecordParser::new(get_builtin_format("node-default"));
        ingest.feed(b"$ hello", &mut buffer, &mut parser);
        assert_eq!(
            buffer.records_len(),
            0,
            "live prompt must stay out of RecordBuffer"
        );
        assert!(ingest.volatile_count() >= 1);
        assert!(ingest.overlay_flat_lines()[0].raw.contains("hello"));
    }

    #[test]
    fn cjk_readline_overwrite_does_not_drift() {
        // End-to-end: bash-style CJK editing keeps the grid caret where the
        // child's cursor is (2 columns per CJK char, DCH shifts cleanly).
        let mut emu = TerminalEmulator::new(80, 24);
        feed(&mut emu, "echo 中文".as_bytes());
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 9 });
        // Backspace twice (over the 文 pair), then Delete (DCH).
        feed(&mut emu, b"\x08\x08");
        assert_eq!(emu.screen_cursor(), ScreenCursor { line: 0, col: 7 });
        feed(&mut emu, "\x1b[1P".as_bytes()); // DCH removes half a pair...
        let line = emu.screen_lines().remove(0);
        // ...the leftover continuation must not render the leader's glyph.
        assert_eq!(line, "echo 中");
    }
}
