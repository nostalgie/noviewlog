use crate::color_emoji::{char_kind, display_cell_count, CharKind};
use crate::core::types::FlatLine;
use crate::viewport::ViewportMetrics;

pub const LEFT_PAD: u32 = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPos {
    pub line_index: usize,
    pub byte_offset: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextSelection {
    pub anchor: TextPos,
    pub caret: TextPos,
}

impl TextSelection {
    pub fn new(anchor: TextPos, caret: TextPos) -> Self {
        Self { anchor, caret }
    }

    pub fn normalized(&self) -> (TextPos, TextPos) {
        let (a, b) = (self.anchor, self.caret);
        if a < b {
            (a, b)
        } else {
            (b, a)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.anchor == self.caret
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VisualLine {
    pub flat_index: usize,
    pub start: usize,
    pub end: usize,
}

pub fn content_width(viewport_width: u32) -> u32 {
    viewport_width.saturating_sub(LEFT_PAD)
}

pub fn max_cols(content_width: u32, cell_width: u32) -> usize {
    (content_width / cell_width.max(1)).max(1) as usize
}

/// Visual rows contributed by one flat line under wrap (empty → 1).
pub fn visual_rows_for_line(raw: &str, cols: usize) -> usize {
    let cols = cols.max(1);
    let cells = display_cell_count(raw);
    if cells == 0 {
        1
    } else {
        cells.div_ceil(cols)
    }
}

/// Prefix sums of visual rows per flat line for the current wrap geometry.
///
/// `prefix[i]` = cumulative visual rows after flat lines `0..=i`.
/// Enables O(1) totals and O(log n) jump to the flat line at a visual row.
#[derive(Clone, Debug, Default)]
pub struct VisualRowIndex {
    prefix: Vec<u32>,
    viewport_width: u32,
    cell_width: u32,
    wrap: bool,
    /// When false, callers must rebuild before use.
    valid: bool,
}

impl VisualRowIndex {
    pub fn invalid() -> Self {
        Self {
            prefix: Vec::new(),
            viewport_width: 0,
            cell_width: 0,
            wrap: false,
            valid: false,
        }
    }

    pub fn is_valid_for(
        &self,
        flat_len: usize,
        wrap: bool,
        viewport_width: u32,
        cell_width: u32,
    ) -> bool {
        self.valid
            && self.prefix.len() == flat_len
            && self.wrap == wrap
            && self.viewport_width == viewport_width
            && self.cell_width == cell_width
    }

    /// Valid index for incremental mutate (append/trim) at the same wrap mode.
    /// Geometry (width/cell) must already match whatever was used to build it.
    pub fn valid_geometry_only(&self, wrap: bool) -> bool {
        self.valid && self.wrap == wrap
    }

    pub fn rebuild(lines: &[FlatLine], wrap: bool, viewport_width: u32, cell_width: u32) -> Self {
        let mut prefix = Vec::with_capacity(lines.len());
        let mut sum = 0u32;
        if !wrap {
            for _ in 0..lines.len() {
                sum += 1;
                prefix.push(sum);
            }
        } else {
            let cols = max_cols(content_width(viewport_width), cell_width).max(1);
            for line in lines {
                sum = sum.saturating_add(visual_rows_for_line(&line.raw, cols) as u32);
                prefix.push(sum);
            }
        }
        Self {
            prefix,
            viewport_width,
            cell_width,
            wrap,
            valid: true,
        }
    }

    pub fn total_rows(&self) -> usize {
        self.prefix.last().copied().unwrap_or(0) as usize
    }

    /// Exclusive visual-row end of flat line `flat_index` (0 if out of range).
    pub fn visual_end_of_flat(&self, flat_index: usize) -> usize {
        self.prefix.get(flat_index).copied().unwrap_or(0) as usize
    }

    /// Flat index and the absolute visual-row start of that flat line for `row`.
    /// Returns `None` when `row` is past the end (or the index is empty).
    pub fn flat_at_visual_row(&self, row: usize) -> Option<(usize, usize)> {
        if self.prefix.is_empty() {
            return None;
        }
        let row_u = row as u32;
        let i = self.prefix.partition_point(|&p| p <= row_u);
        if i >= self.prefix.len() {
            return None;
        }
        let start = if i == 0 {
            0
        } else {
            self.prefix[i - 1] as usize
        };
        Some((i, start))
    }

    pub fn invalidate(&mut self) {
        self.prefix.clear();
        self.valid = false;
    }

    /// Drop the first `n` flat lines (ring trim).
    pub fn drop_prefix(&mut self, n: usize) {
        if !self.valid || n == 0 {
            return;
        }
        if n >= self.prefix.len() {
            self.prefix.clear();
            return;
        }
        let base = self.prefix[n - 1];
        self.prefix.drain(0..n);
        for p in &mut self.prefix {
            *p = p.saturating_sub(base);
        }
    }

    /// Keep only the first `n` flat-line prefix entries.
    pub fn truncate_flat(&mut self, n: usize) {
        if !self.valid {
            return;
        }
        self.prefix.truncate(n);
    }

    /// Append prefix entries for newly added flat lines (same geometry).
    pub fn extend_lines(&mut self, lines: &[FlatLine]) {
        if !self.valid || lines.is_empty() {
            return;
        }
        let mut sum = self.prefix.last().copied().unwrap_or(0);
        if !self.wrap {
            for _ in lines {
                sum += 1;
                self.prefix.push(sum);
            }
            return;
        }
        let cols = max_cols(content_width(self.viewport_width), self.cell_width).max(1);
        for line in lines {
            sum = sum.saturating_add(visual_rows_for_line(&line.raw, cols) as u32);
            self.prefix.push(sum);
        }
    }

    /// Number of flat lines covered (for sync checks).
    pub fn flat_len(&self) -> usize {
        self.prefix.len()
    }
}

pub fn build_visual_lines(
    lines: &[FlatLine],
    wrap: bool,
    viewport_width: u32,
    cell_width: u32,
) -> Vec<VisualLine> {
    if wrap {
        // Same clamp as every other call site: selection/link mapping and the
        // scroll-height index must share one row space even at degenerate widths.
        let cols = max_cols(content_width(viewport_width), cell_width).max(1);
        lines
            .iter()
            .enumerate()
            .flat_map(|(flat_index, line)| wrap_flat_line(flat_index, &line.raw, cols))
            .collect()
    } else {
        lines
            .iter()
            .enumerate()
            .map(|(flat_index, line)| VisualLine {
                flat_index,
                start: 0,
                end: line.raw.len(),
            })
            .collect()
    }
}

/// Row count for scroll height / prefetch — no `VisualLine` allocation.
pub fn count_visual_rows(
    lines: &[FlatLine],
    wrap: bool,
    viewport_width: u32,
    cell_width: u32,
) -> usize {
    VisualRowIndex::rebuild(lines, wrap, viewport_width, cell_width).total_rows()
}

/// Materialize only the visual rows needed to paint `[first_row, first_row + max_rows)`.
/// Avoids allocating a full wrap layout for multi-thousand-line file windows.
///
/// Prefer passing a maintained [`VisualRowIndex`] so mid-buffer Wrap ON is
/// O(log n + viewport) instead of O(scrollback).
pub fn collect_visible_visual_lines(
    lines: &[FlatLine],
    wrap: bool,
    viewport_width: u32,
    cell_width: u32,
    first_row: usize,
    max_rows: usize,
) -> Vec<VisualLine> {
    collect_visible_visual_lines_with_total(
        lines,
        wrap,
        viewport_width,
        cell_width,
        first_row,
        max_rows,
        None,
        None,
    )
}

pub fn collect_visible_visual_lines_with_total(
    lines: &[FlatLine],
    wrap: bool,
    viewport_width: u32,
    cell_width: u32,
    first_row: usize,
    max_rows: usize,
    total_visual_rows: Option<usize>,
    visual_row_index: Option<&VisualRowIndex>,
) -> Vec<VisualLine> {
    collect_visible_visual_lines_counted(
        lines,
        wrap,
        viewport_width,
        cell_width,
        first_row,
        max_rows,
        total_visual_rows,
        visual_row_index,
        None,
    )
}

/// Like [`collect_visible_visual_lines_with_total`], optionally recording how many
/// flat lines were examined (for complexity tests).
pub fn collect_visible_visual_lines_counted(
    lines: &[FlatLine],
    wrap: bool,
    viewport_width: u32,
    cell_width: u32,
    first_row: usize,
    max_rows: usize,
    total_visual_rows: Option<usize>,
    visual_row_index: Option<&VisualRowIndex>,
    visited_flat: Option<&mut usize>,
) -> Vec<VisualLine> {
    if max_rows == 0 || lines.is_empty() {
        return Vec::new();
    }
    if !wrap {
        let end = (first_row + max_rows).min(lines.len());
        if first_row >= end {
            return Vec::new();
        }
        if let Some(v) = visited_flat {
            *v = end - first_row;
        }
        return (first_row..end)
            .map(|flat_index| VisualLine {
                flat_index,
                start: 0,
                end: lines[flat_index].raw.len(),
            })
            .collect();
    }

    let cols = max_cols(content_width(viewport_width), cell_width).max(1);
    let owned_index;
    let index = if let Some(idx) =
        visual_row_index.filter(|i| i.is_valid_for(lines.len(), true, viewport_width, cell_width))
    {
        idx
    } else {
        owned_index = VisualRowIndex::rebuild(lines, true, viewport_width, cell_width);
        &owned_index
    };
    let total = total_visual_rows.unwrap_or_else(|| index.total_rows());

    // Near the bottom (Follow): walk from the end (O(viewport)).
    if first_row > 0 && first_row + max_rows + 1 >= total {
        let out = collect_visible_from_end(lines, cols, first_row, max_rows, total);
        if let Some(v) = visited_flat {
            *v = out
                .first()
                .map(|vl| lines.len() - vl.flat_index)
                .unwrap_or(0);
        }
        return out;
    }

    let Some((flat_start, line_visual_start)) = index.flat_at_visual_row(first_row) else {
        if let Some(v) = visited_flat {
            *v = 0;
        }
        return Vec::new();
    };

    let mut out = Vec::with_capacity(max_rows.min(256));
    let mut skipped = line_visual_start;
    let mut visited = 0usize;

    for (flat_index, line) in lines.iter().enumerate().skip(flat_start) {
        if out.len() >= max_rows {
            break;
        }
        visited += 1;
        let rows = visual_rows_for_line(&line.raw, cols);
        let line_end = skipped + rows;
        if line_end <= first_row {
            skipped = line_end;
            continue;
        }
        let wraps = wrap_flat_line(flat_index, &line.raw, cols);
        for (local, visual) in wraps.into_iter().enumerate() {
            let abs = skipped + local;
            if abs < first_row {
                continue;
            }
            if out.len() >= max_rows {
                break;
            }
            out.push(visual);
        }
        skipped = line_end;
    }
    if let Some(v) = visited_flat {
        *v = visited;
    }
    out
}

fn collect_visible_from_end(
    lines: &[FlatLine],
    cols: usize,
    first_row: usize,
    max_rows: usize,
    total: usize,
) -> Vec<VisualLine> {
    let end = (first_row + max_rows).min(total);
    let start = first_row.min(total);
    if start >= end {
        return Vec::new();
    }
    let mut stack = Vec::with_capacity(max_rows.min(256));
    let mut visual_end = total;
    for flat_index in (0..lines.len()).rev() {
        let wraps = wrap_flat_line(flat_index, &lines[flat_index].raw, cols);
        let rows = wraps.len();
        let visual_start = visual_end.saturating_sub(rows);
        if visual_end > start && visual_start < end {
            for (local, visual) in wraps.into_iter().enumerate() {
                let abs = visual_start + local;
                if abs >= start && abs < end {
                    stack.push(visual);
                }
            }
        }
        visual_end = visual_start;
        if visual_end <= start {
            break;
        }
    }
    stack.reverse();
    stack
}

fn wrap_flat_line(flat_index: usize, raw: &str, max_cols: usize) -> Vec<VisualLine> {
    if raw.is_empty() {
        return vec![VisualLine {
            flat_index,
            start: 0,
            end: 0,
        }];
    }

    let mut out = Vec::new();
    let mut chunk_start = 0usize;
    let mut col = 0usize;

    for (byte_idx, ch) in raw.char_indices() {
        if char_kind(ch) != CharKind::Advance {
            continue;
        }
        if col >= max_cols {
            out.push(VisualLine {
                flat_index,
                start: chunk_start,
                end: byte_idx,
            });
            chunk_start = byte_idx;
            col = 0;
        }
        col += 1;
    }
    out.push(VisualLine {
        flat_index,
        start: chunk_start,
        end: raw.len(),
    });
    out
}

pub fn max_scroll_x(lines: &[FlatLine], viewport_width: u32, cell_width: u32) -> f32 {
    let cw = cell_width.max(1) as f32;
    let available = content_width(viewport_width) as f32;
    let max_line_px = lines
        .iter()
        .map(|l| display_cell_count(&l.raw) as f32 * cw)
        .fold(0.0f32, f32::max);
    (max_line_px - available).max(0.0)
}

pub fn pos_at_pixel(
    x: f32,
    y: f32,
    scroll_y: f32,
    scroll_x: f32,
    wrap: bool,
    metrics: &ViewportMetrics,
    visual_lines: &[VisualLine],
    flat_lines: &[FlatLine],
) -> TextPos {
    if visual_lines.is_empty() {
        return TextPos::default();
    }

    let row = ((scroll_y + y.max(0.0)) / metrics.row_stride)
        .floor()
        .max(0.0) as usize;
    let row = row.min(visual_lines.len().saturating_sub(1));
    let visual = &visual_lines[row];
    let flat = &flat_lines[visual.flat_index];
    let slice = &flat.raw[visual.start..visual.end];

    let rel_x = x - LEFT_PAD as f32 + if wrap { 0.0 } else { scroll_x };
    let col = (rel_x / metrics.cell_width as f32).floor().max(0.0) as usize;
    let byte_in_slice = byte_offset_for_char_col(slice, col);
    TextPos {
        line_index: visual.flat_index,
        byte_offset: visual.start + byte_in_slice,
    }
}

/// Byte offset of the character under display-cell column `col`.
///
/// Columns are display cells, not raw chars: zero-width marks (VS16, ZWJ,
/// combining marks — [`char_kind`] `!= Advance`) occupy no cell, matching the
/// draw path's glyph placement (issue #238). `col` past the line end maps to
/// `text.len()`.
pub fn byte_offset_for_char_col(text: &str, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    let mut cells = 0usize;
    for (byte_idx, ch) in text.char_indices() {
        if char_kind(ch) != CharKind::Advance {
            continue;
        }
        if cells == col {
            return byte_idx;
        }
        cells += 1;
    }
    text.len()
}

pub fn selection_slice_range(
    sel: &TextSelection,
    flat_index: usize,
    slice_start: usize,
    slice_end: usize,
) -> Option<(usize, usize)> {
    if sel.is_empty() {
        return None;
    }
    let (start, end) = sel.normalized();
    if start.line_index > flat_index || end.line_index < flat_index {
        return None;
    }

    let sel_start = if start.line_index == flat_index {
        start.byte_offset
    } else {
        slice_start
    };
    let sel_end = if end.line_index == flat_index {
        end.byte_offset
    } else {
        slice_end
    };

    let from = sel_start.max(slice_start);
    let to = sel_end.min(slice_end);
    if from >= to {
        return None;
    }
    Some((from - slice_start, to - slice_start))
}

pub fn selection_plain_text(flat_lines: &[FlatLine], sel: &TextSelection) -> String {
    if sel.is_empty() {
        return String::new();
    }
    let (start, end) = sel.normalized();
    if start.line_index >= flat_lines.len() || end.line_index >= flat_lines.len() {
        return String::new();
    }
    // Clamp byte offsets to char boundaries: a selection that survived a
    // buffer swap can hold stale offsets landing mid-character, and plain
    // slicing would panic. Internal producers emit boundaries today; this
    // guard keeps the function total.
    fn floor_boundary(s: &str, at: usize) -> usize {
        let at = at.min(s.len());
        if s.is_char_boundary(at) {
            at
        } else {
            (0..at).rev().find(|&i| s.is_char_boundary(i)).unwrap_or(0)
        }
    }

    if start.line_index == end.line_index {
        let line = &flat_lines[start.line_index].raw;
        let from = floor_boundary(line, start.byte_offset);
        let to = floor_boundary(line, end.byte_offset).max(from);
        return line[from..to].to_string();
    }

    let mut out = String::new();
    for (i, line) in flat_lines
        .iter()
        .enumerate()
        .skip(start.line_index)
        .take(end.line_index - start.line_index + 1)
    {
        if i == start.line_index {
            let from = floor_boundary(&line.raw, start.byte_offset);
            out.push_str(&line.raw[from..]);
        } else if i == end.line_index {
            let to = floor_boundary(&line.raw, end.byte_offset);
            out.push_str(&line.raw[..to]);
        } else {
            out.push_str(&line.raw);
        }
        if i < end.line_index {
            out.push('\n');
        }
    }
    out
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Select the word (or whitespace / punctuation run) under `pos`.
pub fn word_selection_at(flat_lines: &[FlatLine], pos: TextPos) -> Option<TextSelection> {
    let line = flat_lines.get(pos.line_index)?;
    let raw = &line.raw;
    if raw.is_empty() {
        return Some(TextSelection::new(
            TextPos {
                line_index: pos.line_index,
                byte_offset: 0,
            },
            TextPos {
                line_index: pos.line_index,
                byte_offset: 0,
            },
        ));
    }

    let chars: Vec<(usize, char)> = raw.char_indices().collect();
    let char_idx = if pos.byte_offset >= raw.len() {
        chars.len().saturating_sub(1)
    } else {
        chars
            .iter()
            .position(|(b, _)| *b == pos.byte_offset)
            .or_else(|| {
                chars
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, (b, _))| *b <= pos.byte_offset)
                    .map(|(i, _)| i)
            })
            .unwrap_or(0)
    };

    let ch = chars[char_idx].1;
    let mut start = char_idx;
    let mut end = char_idx + 1;

    if ch.is_whitespace() {
        while start > 0 && chars[start - 1].1.is_whitespace() {
            start -= 1;
        }
        while end < chars.len() && chars[end].1.is_whitespace() {
            end += 1;
        }
    } else {
        let word = is_word_char(ch);
        while start > 0 {
            let prev = chars[start - 1].1;
            if prev.is_whitespace() || is_word_char(prev) != word {
                break;
            }
            start -= 1;
        }
        while end < chars.len() {
            let next = chars[end].1;
            if next.is_whitespace() || is_word_char(next) != word {
                break;
            }
            end += 1;
        }
    }

    let start_b = chars[start].0;
    let end_b = if end < chars.len() {
        chars[end].0
    } else {
        raw.len()
    };
    Some(TextSelection::new(
        TextPos {
            line_index: pos.line_index,
            byte_offset: start_b,
        },
        TextPos {
            line_index: pos.line_index,
            byte_offset: end_b,
        },
    ))
}

/// Select every FlatLine sharing the same `record_id` as the line under `pos`.
pub fn record_selection_at(flat_lines: &[FlatLine], pos: TextPos) -> Option<TextSelection> {
    let line = flat_lines.get(pos.line_index)?;
    let record_id = line.record_id;
    let mut first = pos.line_index;
    let mut last = pos.line_index;
    while first > 0 && flat_lines[first - 1].record_id == record_id {
        first -= 1;
    }
    while last + 1 < flat_lines.len() && flat_lines[last + 1].record_id == record_id {
        last += 1;
    }
    Some(TextSelection::new(
        TextPos {
            line_index: first,
            byte_offset: 0,
        },
        TextPos {
            line_index: last,
            byte_offset: flat_lines[last].raw.len(),
        },
    ))
}

pub fn slice_segments(
    segments: &[crate::core::types::TextSegment],
    start: usize,
    end: usize,
) -> Vec<crate::core::types::TextSegment> {
    use crate::core::types::TextSegment;

    if start >= end {
        return Vec::new();
    }

    let mut out = Vec::new();
    let mut cursor = 0usize;
    for seg in segments {
        let seg_start = cursor;
        let seg_end = cursor + seg.text.len();
        cursor = seg_end;

        if seg_end <= start || seg_start >= end {
            continue;
        }

        let local_start = start.saturating_sub(seg_start);
        let local_end = (end - seg_start).min(seg.text.len());
        out.push(TextSegment {
            text: seg.text[local_start..local_end].to_string(),
            style: seg.style.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{FlatLine, TextSegment};

    fn flat_line(text: &str) -> FlatLine {
        FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![TextSegment {
                text: text.to_string(),
                style: None,
            }],
            raw: text.to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        }
    }

    /// Issue #238: click columns are display cells — VS16 (`⏱️` = U+23F1 +
    /// U+FE0F) and combining marks occupy no cell, matching the draw path.
    #[test]
    fn click_column_maps_display_cells_not_raw_chars() {
        let line = "ab\u{23F1}\u{FE0F}cd";
        // Cells: a=0, b=1, ⏱=2, c=3, d=4 (FE0F is a silent skip).
        assert_eq!(byte_offset_for_char_col(line, 0), 0);
        assert_eq!(byte_offset_for_char_col(line, 2), "ab".len());
        assert_eq!(
            byte_offset_for_char_col(line, 3),
            "ab\u{23F1}\u{FE0F}".len()
        );
        assert_eq!(
            byte_offset_for_char_col(line, 4),
            "ab\u{23F1}\u{FE0F}c".len()
        );
        assert_eq!(byte_offset_for_char_col(line, 5), line.len());
        assert_eq!(byte_offset_for_char_col(line, 99), line.len());
        // Combining mark overlays the previous cell — no column of its own.
        assert_eq!(byte_offset_for_char_col("a\u{0301}b", 1), "a\u{0301}".len());
    }

    #[test]
    fn visual_row_index_matches_naive_count() {
        let lines: Vec<_> = (0..100)
            .map(|i| flat_line(&format!("line-{i}-{}", "x".repeat(i % 40))))
            .collect();
        let idx = VisualRowIndex::rebuild(&lines, true, 80, 8);
        assert_eq!(idx.total_rows(), count_visual_rows(&lines, true, 80, 8));
        assert_eq!(
            idx.total_rows(),
            build_visual_lines(&lines, true, 80, 8).len()
        );
    }

    #[test]
    fn degenerate_width_keeps_visual_row_parity() {
        // Issue #205: at viewport widths <= LEFT_PAD the content width is 0;
        // selection/link mapping (build_visual_lines) and scroll height
        // (VisualRowIndex) must still agree on the row space.
        let lines = vec![
            flat_line("abcdef"),
            flat_line(""),
            flat_line(&"x".repeat(7)),
        ];
        for width in [0, 1, LEFT_PAD - 1, LEFT_PAD, LEFT_PAD + 1] {
            let idx = VisualRowIndex::rebuild(&lines, true, width, 8);
            assert_eq!(
                idx.total_rows(),
                build_visual_lines(&lines, true, width, 8).len(),
                "index/full-build parity broken at viewport_width={width}"
            );
            assert_eq!(
                idx.total_rows(),
                count_visual_rows(&lines, true, width, 8),
                "index/count parity broken at viewport_width={width}"
            );
            assert_eq!(
                collect_visible_visual_lines(&lines, true, width, 8, 0, idx.total_rows()),
                build_visual_lines(&lines, true, width, 8),
                "visible-slice parity broken at viewport_width={width}"
            );
        }
    }

    #[test]
    fn mid_scroll_collect_with_index_does_not_visit_all_lines() {
        let lines: Vec<_> = (0..20_000)
            .map(|i| flat_line(&format!("row-{i}-{}", "abcdefghij".repeat(5))))
            .collect();
        let idx = VisualRowIndex::rebuild(&lines, true, 80, 8);
        let total = idx.total_rows();
        let first_row = total / 2;
        let mut visited = 0usize;
        let out = collect_visible_visual_lines_counted(
            &lines,
            true,
            80,
            8,
            first_row,
            40,
            Some(total),
            Some(&idx),
            Some(&mut visited),
        );
        assert!(!out.is_empty());
        assert!(
            visited < 200,
            "mid-scroll must not walk scrollback; visited={visited}"
        );
        assert!(
            visited < lines.len() / 10,
            "visited {visited} of {}",
            lines.len()
        );
        // Sanity: same slice as full build
        let full = build_visual_lines(&lines, true, 80, 8);
        assert_eq!(out, full[first_row..first_row + out.len()]);
    }

    #[test]
    fn index_drop_prefix_and_extend_stay_coherent() {
        let mut lines: Vec<_> = (0..100).map(|i| flat_line(&format!("L{i}"))).collect();
        let mut idx = VisualRowIndex::rebuild(&lines, true, 200, 8);
        let before = idx.total_rows();
        idx.drop_prefix(10);
        lines.drain(0..10);
        assert_eq!(idx.flat_len(), lines.len());
        assert_eq!(idx.total_rows(), count_visual_rows(&lines, true, 200, 8));
        assert!(idx.total_rows() < before);
        let extra = vec![flat_line("new-a"), flat_line("new-b")];
        idx.extend_lines(&extra);
        lines.extend(extra);
        assert_eq!(
            idx.total_rows(),
            VisualRowIndex::rebuild(&lines, true, 200, 8).total_rows()
        );
    }

    #[test]
    fn wrap_splits_long_line_into_visual_rows() {
        let line = flat_line("abcdefghijklmnopqrstuvwxyz");
        let visual = build_visual_lines(&[line.clone()], true, 80, 8);
        assert!(
            visual.len() >= 3,
            "expected multiple wrapped rows, got {}",
            visual.len()
        );
        assert!(visual.iter().all(|v| v.flat_index == 0));
        let joined: String = visual.iter().map(|v| &line.raw[v.start..v.end]).collect();
        assert_eq!(joined, "abcdefghijklmnopqrstuvwxyz");
    }

    #[test]
    fn collect_visible_matches_full_wrap_slice() {
        let line = flat_line(&"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ".repeat(4));
        let full = build_visual_lines(&[line.clone()], true, 80, 8);
        assert!(full.len() > 4);
        assert_eq!(
            collect_visible_visual_lines(&[line.clone()], true, 80, 8, 0, full.len()),
            full
        );
        assert_eq!(
            collect_visible_visual_lines(&[line], true, 80, 8, 2, 3),
            full[2..5]
        );
        assert_eq!(
            count_visual_rows(&[flat_line(&"x".repeat(200))], true, 80, 8),
            build_visual_lines(&[flat_line(&"x".repeat(200))], true, 80, 8).len()
        );
    }

    #[test]
    fn wrap_on_vs_off_differ_for_long_line() {
        let line = flat_line(&"x".repeat(200));
        let wrapped = build_visual_lines(&[line.clone()], true, 80, 8);
        let nowrap = build_visual_lines(&[line], false, 80, 8);
        assert!(
            wrapped.len() > 1,
            "Wrap ON must soft-wrap a long line, got {} visual rows",
            wrapped.len()
        );
        assert_eq!(
            nowrap.len(),
            1,
            "Wrap OFF must keep a single visual row for H-scroll"
        );
    }

    #[test]
    fn wrap_tracks_viewport_width_not_fixed_ceiling() {
        let line = flat_line(&"a".repeat(150));
        // content_width 72 / cell 8 → 9 cols; content_width 792 → 99 cols
        let narrow = build_visual_lines(&[line.clone()], true, 80, 8);
        let wide = build_visual_lines(&[line], true, 800, 8);
        assert!(
            narrow.len() > wide.len(),
            "wider viewport must wrap less (narrow={}, wide={})",
            narrow.len(),
            wide.len()
        );
        assert!(wide.len() >= 2, "150 chars still wrap at 99 cols");
    }

    #[test]
    fn nowrap_plus_scroll_x_reveals_tail_of_line() {
        let renderer = crate::viewport::ViewportRenderer::new();
        let metrics = renderer.metrics();
        let cell = metrics.cell_width;
        let line = flat_line("http://localhost:1337/admin/dashboard");
        let visual = build_visual_lines(&[line.clone()], false, 120, cell);
        assert_eq!(visual.len(), 1);

        let scroll_x = max_scroll_x(&[line.clone()], 120, cell);
        assert!(scroll_x > 0.0);

        let tail_col = line.raw.chars().count().saturating_sub(4);
        let pos = pos_at_pixel(
            LEFT_PAD as f32 + tail_col as f32 * cell as f32 - scroll_x,
            0.0,
            0.0,
            scroll_x,
            false,
            metrics,
            &visual,
            &[line.clone()],
        );
        assert_eq!(pos.line_index, 0);
        assert!(pos.byte_offset >= line.raw.len().saturating_sub(8));
    }

    #[test]
    fn pos_at_pixel_maps_click_to_char_index() {
        let renderer = crate::viewport::ViewportRenderer::new();
        let metrics = renderer.metrics();
        let cell = metrics.cell_width;
        let line = flat_line("hello world");
        let visual = build_visual_lines(&[line.clone()], true, 200, cell);

        let pos = pos_at_pixel(
            LEFT_PAD as f32 + 6.0 * cell as f32,
            metrics.row_stride * 0.5,
            0.0,
            0.0,
            true,
            metrics,
            &visual,
            &[line],
        );
        assert_eq!(pos.line_index, 0);
        assert_eq!(pos.byte_offset, "hello ".len());
    }

    #[test]
    fn selection_plain_text_strips_to_raw_plain() {
        let lines = vec![flat_line("line one"), flat_line("line two")];
        let sel = TextSelection::new(
            TextPos {
                line_index: 0,
                byte_offset: 5,
            },
            TextPos {
                line_index: 1,
                byte_offset: 4,
            },
        );
        assert_eq!(selection_plain_text(&lines, &sel), "one\nline");
    }

    #[test]
    fn word_selection_picks_identifier_under_cursor() {
        let lines = vec![flat_line("foo bar_baz qux")];
        // Click inside bar_baz
        let pos = TextPos {
            line_index: 0,
            byte_offset: "foo ".len() + 2,
        };
        let sel = word_selection_at(&lines, pos).unwrap();
        assert_eq!(selection_plain_text(&lines, &sel), "bar_baz");
    }

    #[test]
    fn record_selection_spans_all_lines_of_record() {
        let mut a = flat_line("line A1");
        a.record_id = 10;
        a.line_index = 0;
        let mut b = flat_line("line A2");
        b.record_id = 10;
        b.line_index = 1;
        let mut c = flat_line("line B");
        c.record_id = 11;
        c.line_index = 0;
        let lines = vec![a, b, c];
        let sel = record_selection_at(
            &lines,
            TextPos {
                line_index: 1,
                byte_offset: 0,
            },
        )
        .unwrap();
        assert_eq!(selection_plain_text(&lines, &sel), "line A1\nline A2");
    }
}
