//! Screen-cell state for [`TerminalEmulator`]: grid types, pens/colors, and
//! the screen/scrollback manipulation methods of the emulator (pure code
//! motion from the former single-file `terminal.rs`).

use std::sync::Arc;

use super::TerminalEmulator;
use crate::ansi::{ansi_256_color, ansi_basic_color};
use crate::types::{FlatLine, TextSegment, TextStyle};
use vte::Parser;

pub(super) const DEFAULT_COLS: usize = 120;
pub(super) const DEFAULT_ROWS: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    /// Basic SGR code (30-37 / 90-97 for fg, 40-47 / 100-107 for bg).
    Basic(u16),
    /// 256-colour palette index.
    Ext(u16),
    Rgb(u8, u8, u8),
}

#[derive(Clone, PartialEq, Eq, Default)]
pub(super) struct Pen {
    fg: Option<Color>,
    bg: Option<Color>,
    bold: bool,
    dim: bool,
    underline: bool,
    /// Active OSC 8 hyperlink (not an SGR property; separate lifecycle).
    pub(super) link: Option<Arc<str>>,
}

impl Pen {
    fn is_default(&self) -> bool {
        *self == Pen::default()
    }

    /// Emit the SGR parameter body (without `ESC[` / `m`) for this pen.
    /// Always leads with `0` so any previously-active style is reset first.
    fn sgr_body(&self) -> String {
        let mut parts: Vec<String> = vec!["0".to_string()];
        if self.bold {
            parts.push("1".to_string());
        }
        if self.dim {
            parts.push("2".to_string());
        }
        if self.underline {
            parts.push("4".to_string());
        }
        if let Some(c) = self.fg {
            parts.push(color_sgr(c, true));
        }
        if let Some(c) = self.bg {
            parts.push(color_sgr(c, false));
        }
        parts.join(";")
    }
}

fn color_sgr(c: Color, _fg: bool) -> String {
    match c {
        // Basic codes already encode fg vs bg (30.. vs 40..).
        Color::Basic(code) => code.to_string(),
        Color::Ext(n) => {
            if _fg {
                format!("38;5;{n}")
            } else {
                format!("48;5;{n}")
            }
        }
        Color::Rgb(r, g, b) => {
            if _fg {
                format!("38;2;{r};{g};{b}")
            } else {
                format!("48;2;{r};{g};{b}")
            }
        }
    }
}

#[derive(Clone)]
struct Cell {
    ch: char,
    pen: Pen,
    /// Leader cell of a double-width (wcwidth 2) character.
    wide: bool,
    /// Follower (continuation) cell of a double-width leader. Holds a blank
    /// glyph and is skipped by all projection paths; the wide char's glyph is
    /// emitted once and paints over the second column (documented overflow).
    cont: bool,
}

impl Default for Cell {
    fn default() -> Self {
        Cell {
            ch: ' ',
            pen: Pen::default(),
            wide: false,
            cont: false,
        }
    }
}

impl Cell {
    fn is_blank(&self) -> bool {
        self.ch == ' ' && self.pen.is_default()
    }

    /// Follower cell for a double-width leader (same pen, blank glyph).
    fn continuation(pen: Pen) -> Self {
        Cell {
            ch: ' ',
            pen,
            wide: false,
            cont: true,
        }
    }
}

#[derive(Clone)]
pub(super) struct Row {
    cells: Vec<Cell>,
    /// True when the row overflowed into the next one via auto-wrap (no explicit
    /// newline). Used to re-join wrapped rows into one logical log line.
    pub(super) wrapped: bool,
}

impl Row {
    pub(super) fn new(cols: usize) -> Self {
        Row {
            cells: vec![Cell::default(); cols],
            wrapped: false,
        }
    }

    pub(super) fn clear(&mut self) {
        for c in &mut self.cells {
            *c = Cell::default();
        }
        self.wrapped = false;
    }

    /// If `col` holds either half of a double-width pair, blank both halves so
    /// no stale half of the pair can render as garbage.
    pub(super) fn blank_wide_pair(&mut self, col: usize) {
        if col >= self.cells.len() {
            return;
        }
        if self.cells[col].wide {
            self.cells[col] = Cell::default();
            if col + 1 < self.cells.len() && self.cells[col + 1].cont {
                self.cells[col + 1] = Cell::default();
            }
        } else if self.cells[col].cont {
            self.cells[col] = Cell::default();
            if col > 0 && self.cells[col - 1].wide {
                self.cells[col - 1] = Cell::default();
            }
        }
    }

    /// Blank continuation cells whose leader was removed by an edit or shift
    /// (e.g. the partner left the row via DCH/ICH or a range erase).
    pub(super) fn fix_orphan_conts(&mut self) {
        for i in 0..self.cells.len() {
            if self.cells[i].cont && (i == 0 || !self.cells[i - 1].wide) {
                self.cells[i] = Cell::default();
            }
        }
    }

    /// Serialize to an ANSI string. Trailing blanks are trimmed only when the
    /// row is a true line end; auto-wrapped rows keep their full width so a
    /// space that landed on the wrap column is not lost when re-joining.
    fn serialize(&self) -> String {
        let last = if self.wrapped {
            self.cells.len()
        } else {
            self.cells
                .iter()
                .rposition(|c| !c.is_blank())
                .map(|i| i + 1)
                .unwrap_or(0)
        };
        let mut out = String::new();
        let mut cur = Pen::default();
        let mut cur_link: Option<Arc<str>> = None;
        for cell in &self.cells[..last] {
            if cell.pen != cur {
                out.push_str("\x1b[");
                out.push_str(&cell.pen.sgr_body());
                out.push('m');
                cur = cell.pen.clone();
            }
            if cell.pen.link != cur_link {
                if cur_link.is_some() {
                    out.push_str("\x1b]8;;\x07");
                }
                if let Some(uri) = &cell.pen.link {
                    out.push_str("\x1b]8;;");
                    out.push_str(uri);
                    out.push('\x07');
                }
                cur_link = cell.pen.link.clone();
            }
            if cell.cont {
                continue;
            }
            out.push(cell.ch);
        }
        if !cur.is_default() {
            out.push_str("\x1b[0m");
        }
        if cur_link.is_some() {
            out.push_str("\x1b]8;;\x07");
        }
        out
    }
}

fn color_rgb(c: &Color) -> (u8, u8, u8) {
    match *c {
        Color::Basic(n) => {
            let n = n as u32;
            if (30..=37).contains(&n) {
                ansi_basic_color(n - 30, false)
            } else if (90..=97).contains(&n) {
                ansi_basic_color(n - 90, true)
            } else if (40..=47).contains(&n) {
                ansi_basic_color(n - 40, false)
            } else if (100..=107).contains(&n) {
                ansi_basic_color(n - 100, true)
            } else {
                (230, 237, 243)
            }
        }
        Color::Ext(n) => ansi_256_color(n as u32),
        Color::Rgb(r, g, b) => (r, g, b),
    }
}

fn pen_to_style(pen: &Pen) -> Option<TextStyle> {
    if pen.is_default() {
        return None;
    }
    Some(TextStyle {
        fg: pen.fg.as_ref().map(color_rgb),
        bg: pen.bg.as_ref().map(color_rgb),
        bold: pen.bold,
        dim: pen.dim,
        underline: pen.underline,
        search: false,
        search_current: false,
        selected: false,
        link: pen.link.clone(),
    })
}

fn overlay_id(line: usize) -> u64 {
    (u64::MAX / 2).wrapping_add(line as u64)
}

fn push_overlay_line(out: &mut Vec<FlatLine>, segments: Vec<TextSegment>, raw: String) {
    out.push(FlatLine {
        record_id: overlay_id(out.len()),
        line_index: 0,
        segments,
        raw,
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    });
}

impl TerminalEmulator {
    /// Resize the live screen grid to match the viewport / PTY winsize.
    ///
    /// Column changes reflow auto-wrapped logical lines onto the new grid so
    /// soft-wrap / horizontal scroll still see one long line instead of
    /// fragmented hard-wrap slices. Already-committed scrollback is untouched.
    /// Height shrink commits rows that scroll off the top; growth pads blanks.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        if cols == self.cols && rows == self.rows {
            return;
        }

        if cols != self.cols {
            // Serialize the live grid back to ANSI (SGR runs preserved) and
            // replay through the normal VT path so colors survive the reflow
            // (issue #79; the previous plain-text replay stripped SGR). OSC 8
            // link state is not re-emitted — same as the segment serializer.
            let replay = self.styled_screen_ansi();
            let cursor = self.screen_cursor();
            let cursor_visible = self.cursor_visible;
            let pen = self.pen.clone();

            self.cols = cols;
            self.rows = rows;
            self.screen = vec![Row::new(cols); rows];
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.wrap_pending = false;
            self.wrap_accum = None;
            self.pen = Pen::default();

            let mut parser = Parser::new();
            parser.advance(self, &replay);

            // Best-effort caret restore within the reflowed logical lines.
            self.cursor_row = 0;
            self.cursor_col = 0;
            self.wrap_pending = false;
            for _ in 0..cursor.line {
                if self.cursor_row + 1 < self.rows {
                    self.cursor_row += 1;
                }
            }
            // Walk to the target column, following auto-wrap like write_char.
            let mut remaining = cursor.col;
            while remaining > 0 {
                if self.cursor_col + 1 >= self.cols {
                    self.screen[self.cursor_row].wrapped = true;
                    self.cursor_col = 0;
                    self.line_feed();
                } else {
                    self.cursor_col += 1;
                }
                remaining -= 1;
            }
            self.cursor_visible = cursor_visible;
            self.pen = pen;
            return;
        }

        while self.screen.len() > rows {
            if self.cursor_row + 1 > rows {
                let row = self.screen.remove(0);
                self.commit_row(&row);
                self.cursor_row = self.cursor_row.saturating_sub(1);
            } else {
                self.screen.pop();
            }
        }
        while self.screen.len() < rows {
            self.screen.push(Row::new(self.cols));
        }
        self.rows = rows;
        self.cursor_col = self.cursor_col.min(self.cols.saturating_sub(1));
        self.cursor_row = self.cursor_row.min(self.rows.saturating_sub(1));
        self.wrap_pending = false;
    }

    /// Serialize the live grid back to VT bytes: cell runs with SGR transitions
    /// (`ESC[…m` only when the pen changes), `\r\n` between logical lines
    /// (wrapped rows continue). Line selection/trailing-trim matches
    /// [`Self::plain_screen_lines`] exactly, so the replay scrolls exactly as
    /// much as the old plain-text one did (issue #79).
    fn styled_screen_ansi(&self) -> Vec<u8> {
        // Logical lines: wrapped rows joined; per-row trailing blanks trimmed
        // the same way plain_screen_lines does.
        let mut lines: Vec<String> = Vec::new();
        let mut cur = String::new();
        let mut cur_pen = Pen::default();
        for row in &self.screen {
            let last = if row.wrapped {
                row.cells.len()
            } else {
                row.cells
                    .iter()
                    .rposition(|c| !c.is_blank())
                    .map(|i| i + 1)
                    .unwrap_or(0)
            };
            for cell in &row.cells[..last] {
                if cell.cont {
                    continue;
                }
                if cell.pen != cur_pen {
                    cur_pen = cell.pen.clone();
                    if cur_pen.is_default() {
                        cur.push_str("\x1b[0m");
                    } else {
                        cur.push_str("\x1b[");
                        cur.push_str(&cur_pen.sgr_body());
                        cur.push('m');
                    }
                }
                cur.push(cell.ch);
            }
            if !row.wrapped {
                lines.push(std::mem::take(&mut cur));
            }
        }
        if !cur.is_empty() {
            lines.push(cur);
        }

        // Same trailing-trim / cursor-line padding as plain_screen_lines.
        let keep_through = self.screen_cursor().line;
        while lines.len() <= keep_through {
            lines.push(String::new());
        }
        while lines.len() > keep_through + 1 && lines.last().map(|s| s.is_empty()).unwrap_or(false)
        {
            lines.pop();
        }
        lines.join("\r\n").into_bytes()
    }

    /// Like [`Self::screen_lines`], but plain text from cells (no SGR) — used
    /// when reflowing the live grid on a column resize.
    #[cfg(test)]
    pub(super) fn plain_screen_lines(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut acc = String::new();
        for row in &self.screen {
            let last = if row.wrapped {
                row.cells.len()
            } else {
                row.cells
                    .iter()
                    .rposition(|c| !c.is_blank())
                    .map(|i| i + 1)
                    .unwrap_or(0)
            };
            for cell in &row.cells[..last] {
                if cell.cont {
                    continue;
                }
                acc.push(cell.ch);
            }
            if !row.wrapped {
                out.push(std::mem::take(&mut acc));
            }
        }
        if !acc.is_empty() {
            out.push(acc);
        }
        let keep_through = self.screen_cursor().line;
        while out.len() <= keep_through {
            out.push(String::new());
        }
        while out.len() > keep_through + 1 && out.last().map(|s| s.is_empty()).unwrap_or(false) {
            out.pop();
        }
        out
    }

    /// The current live on-screen rows (auto-wrapped rows re-joined).
    /// Trailing blank lines are trimmed, except those needed so the caret
    /// row remains present (empty prompt line, cursor below content, etc.).
    pub fn screen_lines(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut acc = String::new();
        for row in &self.screen {
            acc.push_str(&row.serialize());
            if !row.wrapped {
                out.push(std::mem::take(&mut acc));
            }
        }
        if !acc.is_empty() {
            out.push(acc);
        }
        let keep_through = self.screen_cursor().line;
        while out.len() <= keep_through {
            out.push(String::new());
        }
        while out.len() > keep_through + 1 && out.last().map(|s| s.is_empty()).unwrap_or(false) {
            out.pop();
        }
        out
    }

    pub(super) fn row_to_segments(row: &Row) -> (Vec<TextSegment>, String) {
        let last = if row.wrapped {
            row.cells.len()
        } else {
            row.cells
                .iter()
                .rposition(|c| !c.is_blank())
                .map(|i| i + 1)
                .unwrap_or(0)
        };
        let mut segments: Vec<TextSegment> = Vec::new();
        let mut raw = String::new();
        let mut cur_pen = Pen::default();
        let mut cur_text = String::new();
        let flush_seg = |segments: &mut Vec<TextSegment>, cur_pen: &Pen, cur_text: &mut String| {
            if cur_text.is_empty() {
                return;
            }
            segments.push(TextSegment {
                text: std::mem::take(cur_text),
                style: pen_to_style(cur_pen),
            });
        };
        for cell in &row.cells[..last] {
            if cell.cont {
                continue;
            }
            if cell.pen != cur_pen {
                flush_seg(&mut segments, &cur_pen, &mut cur_text);
                cur_pen = cell.pen.clone();
            }
            cur_text.push(cell.ch);
            raw.push(cell.ch);
        }
        flush_seg(&mut segments, &cur_pen, &mut cur_text);
        if segments.is_empty() {
            segments.push(TextSegment {
                text: String::new(),
                style: None,
            });
        }
        (segments, raw)
    }

    /// Physical screen rows as FlatLines (one per grid row, no wrap-join).
    /// Used to paint Follow like a native terminal: the viewport *is* the grid.
    pub fn grid_flat_lines(&self) -> Vec<FlatLine> {
        let mut out: Vec<FlatLine> = Vec::with_capacity(self.rows);
        for row in &self.screen {
            let (segments, raw) = Self::row_to_segments(row);
            push_overlay_line(&mut out, segments, raw);
        }
        out
    }

    pub fn grid_cursor(&self) -> (usize, usize) {
        (
            self.cursor_row,
            self.cursor_col.min(self.cols.saturating_sub(1)),
        )
    }

    /// Live screen as Terminal tab overlay lines (cells → segments, no Records).
    pub fn overlay_flat_lines(&self) -> Vec<FlatLine> {
        let mut out: Vec<FlatLine> = Vec::new();
        let mut segments: Vec<TextSegment> = Vec::new();
        let mut raw = String::new();
        let mut cur_pen = Pen::default();
        let mut cur_text = String::new();

        let flush_seg = |segments: &mut Vec<TextSegment>, cur_pen: &Pen, cur_text: &mut String| {
            if cur_text.is_empty() {
                return;
            }
            segments.push(TextSegment {
                text: std::mem::take(cur_text),
                style: pen_to_style(cur_pen),
            });
        };

        for row in &self.screen {
            let last = if row.wrapped {
                row.cells.len()
            } else {
                row.cells
                    .iter()
                    .rposition(|c| !c.is_blank())
                    .map(|i| i + 1)
                    .unwrap_or(0)
            };
            for cell in &row.cells[..last] {
                if cell.cont {
                    continue;
                }
                if cell.pen != cur_pen {
                    flush_seg(&mut segments, &cur_pen, &mut cur_text);
                    cur_pen = cell.pen.clone();
                }
                cur_text.push(cell.ch);
                raw.push(cell.ch);
            }
            if !row.wrapped {
                flush_seg(&mut segments, &cur_pen, &mut cur_text);
                if segments.is_empty() {
                    segments.push(TextSegment {
                        text: String::new(),
                        style: None,
                    });
                }
                push_overlay_line(
                    &mut out,
                    std::mem::take(&mut segments),
                    std::mem::take(&mut raw),
                );
                cur_pen = Pen::default();
            }
        }
        if !raw.is_empty() || !cur_text.is_empty() || !segments.is_empty() {
            flush_seg(&mut segments, &cur_pen, &mut cur_text);
            if segments.is_empty() {
                segments.push(TextSegment {
                    text: String::new(),
                    style: None,
                });
            }
            push_overlay_line(&mut out, segments, raw);
        }
        let keep_through = self.screen_cursor().line;
        while out.len() <= keep_through {
            push_overlay_line(
                &mut out,
                vec![TextSegment {
                    text: String::new(),
                    style: None,
                }],
                String::new(),
            );
        }
        while out.len() > keep_through + 1 && out.last().map(|l| l.raw.is_empty()).unwrap_or(false)
        {
            out.pop();
        }
        out
    }

    /// Flush the screen into `committed` (called at process exit / EOF), dropping
    /// the trailing unused blank rows of the grid.
    pub fn flush_all(&mut self) {
        let last = (0..self.rows).rev().find(|&i| !self.row_is_empty(i));
        if let Some(last) = last {
            for i in 0..=last {
                let row = std::mem::replace(&mut self.screen[i], Row::new(self.cols));
                self.commit_row(&row);
            }
        }
        if let Some(rem) = self.wrap_accum.take() {
            self.committed.push(rem);
        }
        self.cursor_row = 0;
        self.cursor_col = 0;
        self.cursor_visible = true;
    }

    fn row_is_empty(&self, i: usize) -> bool {
        let row = &self.screen[i];
        !row.wrapped && row.cells.iter().all(Cell::is_blank)
    }

    // ---- internal grid ops ----

    fn commit_row(&mut self, row: &Row) {
        let s = row.serialize();
        let joined = match self.wrap_accum.take() {
            Some(prev) => prev + &s,
            None => s,
        };
        if row.wrapped {
            self.wrap_accum = Some(joined);
        } else {
            self.committed.push(joined);
        }
    }

    fn scroll_up(&mut self) {
        let row = std::mem::replace(&mut self.screen[0], Row::new(self.cols));
        self.commit_row(&row);
        self.screen.remove(0);
        self.screen.push(Row::new(self.cols));
    }

    /// Reverse index at the top row: insert a blank line at the top, drop the
    /// bottom row of the grid. Nothing is committed to scrollback (content
    /// moves down, off-screen rows appear at the bottom).
    pub(super) fn scroll_down(&mut self) {
        self.screen.pop();
        self.screen.insert(0, Row::new(self.cols));
    }

    pub(super) fn line_feed(&mut self) {
        if self.cursor_row + 1 < self.rows {
            self.cursor_row += 1;
        } else {
            self.scroll_up();
        }
    }

    pub(super) fn write_char(&mut self, ch: char) {
        if self.wrap_pending {
            // Finish the auto-wrap deferred from the previous cell.
            self.screen[self.cursor_row].wrapped = true;
            self.wrap_pending = false;
            self.cursor_col = 0;
            self.line_feed();
        }
        // Double-width (wcwidth) semantics: a wide char occupies two cells
        // (leader + blank continuation), zero-width chars attach to the
        // previous cell (dropped here; cells store one char each).
        let w = super::width::char_width(ch);
        if w == 0 {
            return;
        }
        let wide = w == 2;
        let col = self.cursor_col.min(self.cols - 1);
        if wide && col + 1 >= self.cols {
            // A wide char does not fit in the last column: fill it with a
            // blank and defer the wrap like a narrow char (standard behavior).
            let row = &mut self.screen[self.cursor_row];
            row.blank_wide_pair(col);
            row.cells[col] = Cell {
                ch: ' ',
                pen: self.pen.clone(),
                wide: false,
                cont: false,
            };
            self.wrap_pending = true;
            return;
        }
        {
            let row = &mut self.screen[self.cursor_row];
            // Overwriting either half of a wide pair blanks both halves.
            row.blank_wide_pair(col);
            if wide {
                row.blank_wide_pair(col + 1);
            }
            row.cells[col] = Cell {
                ch,
                pen: self.pen.clone(),
                wide,
                cont: false,
            };
            if wide {
                row.cells[col + 1] = Cell::continuation(self.pen.clone());
            }
        }
        if self.cursor_col + w >= self.cols {
            if wide {
                // Wide char ends exactly at the last column: the next glyph
                // wraps; park the cursor on the last column like narrow chars.
                self.cursor_col = self.cols - 1;
            }
            self.wrap_pending = true;
        } else {
            self.cursor_col += w;
        }
    }

    pub(super) fn erase_line(&mut self, mode: u16) {
        self.wrap_pending = false;
        let col = self.cursor_col.min(self.cols - 1);
        let row = &mut self.screen[self.cursor_row];
        row.blank_wide_pair(col);
        match mode {
            1 => {
                for c in &mut row.cells[..=col] {
                    *c = Cell::default();
                }
            }
            2 => {
                row.clear();
            }
            _ => {
                for c in &mut row.cells[col..] {
                    *c = Cell::default();
                }
            }
        }
        row.fix_orphan_conts();
    }

    /// CSI `P` — Delete Character (DCH): shift cells left from the cursor.
    pub(super) fn delete_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        let col = self.cursor_col.min(self.cols - 1);
        let n = count.min(self.cols - col);
        if n == 0 {
            return;
        }
        let row = &mut self.screen[self.cursor_row];
        row.blank_wide_pair(col);
        for i in col..(self.cols - n) {
            row.cells[i] = row.cells[i + n].clone();
        }
        for c in &mut row.cells[(self.cols - n)..] {
            *c = Cell::default();
        }
        row.fix_orphan_conts();
    }

    /// CSI `@` — Insert Character (ICH): shift cells right from the cursor.
    pub(super) fn insert_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        let col = self.cursor_col.min(self.cols - 1);
        let n = count.min(self.cols - col);
        if n == 0 {
            return;
        }
        let row = &mut self.screen[self.cursor_row];
        row.blank_wide_pair(col);
        for i in ((col + n)..self.cols).rev() {
            row.cells[i] = row.cells[i - n].clone();
        }
        for c in &mut row.cells[col..(col + n)] {
            *c = Cell::default();
        }
        row.fix_orphan_conts();
    }

    /// CSI `X` — Erase Character (ECH): blank cells without shifting.
    pub(super) fn erase_chars(&mut self, count: usize) {
        self.wrap_pending = false;
        let col = self.cursor_col.min(self.cols - 1);
        let end = (col + count).min(self.cols);
        let row = &mut self.screen[self.cursor_row];
        row.blank_wide_pair(col);
        for c in &mut row.cells[col..end] {
            *c = Cell::default();
        }
        row.fix_orphan_conts();
    }

    pub(super) fn erase_display(&mut self, mode: u16) {
        self.wrap_pending = false;
        match mode {
            1 => {
                for r in 0..self.cursor_row {
                    self.screen[r].clear();
                }
                self.erase_line(1);
            }
            2 | 3 => {
                for r in 0..self.rows {
                    self.screen[r].clear();
                }
                self.cursor_row = 0;
                self.cursor_col = 0;
            }
            _ => {
                self.erase_line(0);
                for r in (self.cursor_row + 1)..self.rows {
                    self.screen[r].clear();
                }
            }
        }
    }

    pub(super) fn apply_sgr(&mut self, codes: &[u16]) {
        if codes.is_empty() {
            self.pen = Pen::default();
            return;
        }
        let mut i = 0;
        while i < codes.len() {
            match codes[i] {
                0 => self.pen = Pen::default(),
                1 => self.pen.bold = true,
                2 => self.pen.dim = true,
                4 => self.pen.underline = true,
                22 => {
                    self.pen.bold = false;
                    self.pen.dim = false;
                }
                24 => self.pen.underline = false,
                39 => self.pen.fg = None,
                49 => self.pen.bg = None,
                n @ (30..=37 | 90..=97) => self.pen.fg = Some(Color::Basic(n)),
                n @ (40..=47 | 100..=107) => self.pen.bg = Some(Color::Basic(n)),
                38 | 48 => {
                    let is_fg = codes[i] == 38;
                    let color = match codes.get(i + 1).copied() {
                        Some(5) => {
                            let n = codes.get(i + 2).copied().unwrap_or(0);
                            i += 2;
                            Some(Color::Ext(n))
                        }
                        Some(2) => {
                            let r = codes.get(i + 2).copied().unwrap_or(0) as u8;
                            let g = codes.get(i + 3).copied().unwrap_or(0) as u8;
                            let b = codes.get(i + 4).copied().unwrap_or(0) as u8;
                            i += 4;
                            Some(Color::Rgb(r, g, b))
                        }
                        _ => None,
                    };
                    if let Some(c) = color {
                        if is_fg {
                            self.pen.fg = Some(c);
                        } else {
                            self.pen.bg = Some(c);
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(emu: &mut TerminalEmulator, s: &str) {
        let mut parser = Parser::new();
        parser.advance(emu, s.as_bytes());
    }

    #[test]
    fn cjk_wide_cells_advance_cursor_by_two() {
        // Issue #78: the grid cursor advances 2 columns per double-width
        // char, matching the child program's cursor.
        let mut emu = TerminalEmulator::new(20, 5);
        feed(&mut emu, "中文");
        assert_eq!(emu.grid_cursor(), (0, 4));
        let row = &emu.screen[0];
        assert_eq!(row.cells[0].ch, '中');
        assert!(row.cells[0].wide);
        assert!(row.cells[1].cont, "follower cell marks the wide pair");
        assert_eq!(row.cells[2].ch, '文');
        assert!(row.cells[2].wide);
        assert!(row.cells[3].cont);
        // Projection emits each glyph once — no duplicate/garbage cells.
        assert_eq!(emu.screen_lines(), vec!["中文".to_string()]);
        let flat = emu.grid_flat_lines();
        assert_eq!(flat[0].raw, "中文");
        let segs = flat[0]
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>();
        assert_eq!(segs, "中文");
    }

    #[test]
    fn overwriting_continuation_cell_blanks_leader() {
        let mut emu = TerminalEmulator::new(20, 5);
        feed(&mut emu, "中文");
        feed(&mut emu, "\x1b[1;2H"); // cursor onto the continuation of 中
        feed(&mut emu, "x");
        let row = &emu.screen[0];
        assert_eq!(row.cells[0].ch, ' ', "leader must be blanked");
        assert!(!row.cells[0].wide);
        assert_eq!(row.cells[1].ch, 'x');
        assert_eq!(emu.screen_lines(), vec![" x文".to_string()]);
    }

    #[test]
    fn erase_ops_on_wide_pair_leave_no_residue() {
        // ECH hitting the continuation half blanks the leader too.
        let mut emu = TerminalEmulator::new(20, 5);
        feed(&mut emu, "a中b");
        feed(&mut emu, "\x1b[1;3H"); // cursor onto the continuation of 中
        feed(&mut emu, "\x1b[1X");
        let row = &emu.screen[0];
        assert_eq!(row.cells[1].ch, ' ', "leader blanked with its continuation");
        assert!(!row.cells[1].wide);
        assert_eq!(row.cells[2].ch, ' ');
        assert_eq!(emu.screen_lines(), vec!["a  b".to_string()]);

        // EL from the continuation half (mode 0) also removes the leader half.
        let mut emu = TerminalEmulator::new(20, 5);
        feed(&mut emu, "a中bc");
        feed(&mut emu, "\x1b[1;3H");
        feed(&mut emu, "\x1b[K");
        assert_eq!(emu.screen_lines(), vec!["a".to_string()]);
    }

    #[test]
    fn combining_mark_does_not_advance_cursor() {
        let mut emu = TerminalEmulator::new(20, 5);
        feed(&mut emu, "a\u{0301}b");
        assert_eq!(emu.grid_cursor(), (0, 2));
        let row = &emu.screen[0];
        assert_eq!(row.cells[0].ch, 'a');
        assert_eq!(row.cells[1].ch, 'b');
        assert_eq!(emu.screen_lines(), vec!["ab".to_string()]);
    }

    #[test]
    fn wide_char_at_last_column_defers_wrap() {
        // Wide char starting at the last column does not fit: blank filler +
        // deferred wrap, then the next char lands on the next row, col 0.
        let mut emu = TerminalEmulator::new(10, 5);
        feed(&mut emu, "abcdefghi中x");
        assert!(emu.screen[0].wrapped, "wrap deferred from the wide char");
        let row = &emu.screen[0];
        assert_eq!(row.cells[9].ch, ' ', "filler blank in the last column");
        assert!(!row.cells[9].wide);
        assert_eq!(emu.screen[1].cells[0].ch, 'x');
        assert_eq!(emu.grid_cursor(), (1, 1));

        // Wide char *ending* at the last column also defers the wrap.
        let mut emu = TerminalEmulator::new(10, 5);
        feed(&mut emu, "abcdefgh中x");
        assert!(emu.screen[0].wrapped);
        assert_eq!(emu.screen[1].cells[0].ch, 'x');
    }
}
