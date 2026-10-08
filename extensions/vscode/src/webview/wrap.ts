/**
 * Soft-wrap: turn flat lines into visual rows by measuring the monospace
 * grid in *display cells* (wide CJK = 2, combining = 0), so the canvas
 * renderer stays a dumb painter and wrap logic stays unit-testable. Cut
 * positions are UTF-16 unit offsets that never split a code point.
 */

import type { LineDto } from "../protocol";

export interface VisualRow {
  /** Index into the flat-line array. */
  line: number;
  /** This row's index within its line (0 = first visual row of the line). */
  row: number;
  rowCount: number;
  /** Character slice [start, end) of `raw` painted on this row. */
  start: number;
  end: number;
}

/** Terminal-style display width of a code point (rough wcwidth). */
function charCells(cp: number): number {
  if (cp < 32 || (cp >= 0x7f && cp < 0xa0)) {
    return 0; // control characters occupy no cells
  }
  if (
    (cp >= 0x0300 && cp <= 0x036f) || // combining diacritics
    (cp >= 0x200b && cp <= 0x200f) || // zero-width
    (cp >= 0xfe00 && cp <= 0xfe0f) // variation selectors
  ) {
    return 0;
  }
  if (
    (cp >= 0x1100 && cp <= 0x115f) || // Hangul Jamo
    (cp >= 0x2e80 && cp <= 0xa4cf && cp !== 0x303f) || // CJK radicals..Yi
    (cp >= 0xac00 && cp <= 0xd7a3) || // Hangul syllables
    (cp >= 0xf900 && cp <= 0xfaff) || // CJK compatibility ideographs
    (cp >= 0xfe30 && cp <= 0xfe6f) || // CJK compatibility forms
    (cp >= 0xff00 && cp <= 0xff60) || // fullwidth forms
    (cp >= 0xffe0 && cp <= 0xffe6) ||
    (cp >= 0x1f300 && cp <= 0x1f64f) || // emoji
    (cp >= 0x1f900 && cp <= 0x1f9ff) ||
    (cp >= 0x20000 && cp <= 0x3fffd) // CJK extension B+
  ) {
    return 2;
  }
  return 1;
}

/**
 * UTF-16 unit offsets where `raw` may be cut so a row never exceeds
 * `columns` cells and no code point (incl. surrogate pairs) is split.
 * A glyph wider than the grid is emitted alone on its own row.
 */
export function rowCuts(raw: string, columns: number): number[] {
  if (columns < 1 || raw.length === 0) {
    return [raw.length];
  }
  const cuts: number[] = [];
  let units = 0;
  let cells = 0;
  let rowStart = 0;
  while (units < raw.length) {
    const cp = raw.codePointAt(units)!;
    const width = charCells(cp);
    const unitLen = cp > 0xffff ? 2 : 1;
    if (cells + width > columns && units > rowStart) {
      cuts.push(units);
      rowStart = units;
      cells = 0;
    }
    cells += width;
    units += unitLen;
  }
  cuts.push(raw.length);
  return cuts;
}

/**
 * Compute visual rows for `lines` at `columns` grid columns.
 * `columns < 1` disables wrapping (one row per line).
 */
export function computeRows(
  lines: LineDto[],
  columns: number,
  lineOffset = 0,
): VisualRow[] {
  const rows: VisualRow[] = [];
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i]!.raw;
    const cuts = rowCuts(raw, columns);
    // Row r spans [prevCut, cuts[r]) — cuts holds row END offsets and
    // implicitly starts at 0.
    const rowCount = cuts.length;
    for (let r = 0; r < rowCount; r++) {
      rows.push({
        line: i + lineOffset,
        row: r,
        rowCount,
        start: r === 0 ? 0 : cuts[r - 1]!,
        end: cuts[r]!,
      });
    }
  }
  return rows;
}

/**
 * Incremental visual-row layout (issue #322): under PTY flood the webview
 * receives up to ~60 appends per second, so re-running `computeRows` over
 * the whole buffer per append is O(buffer) churn. When the new line array
 * keeps the previous references for the common prefix (the append shape:
 * `[...view.lines]` truncates to `base` and pushes new DTOs), only the
 * previously-last line — it may have grown — and the appended tail are
 * recomputed; any other change (resize, wrap toggle, truncation, mid-buffer
 * replacement) falls back to a full recompute. The produced rows are always
 * equal to `computeRows(lines, columns)`, and reused prefix rows keep
 * object identity.
 */
export class RowLayout {
  rows: VisualRow[] = [];
  private lines: LineDto[] = [];
  /** Row index where each line of `lines` starts (every line owns >= 1). */
  private rowStarts: number[] = [];
  private currentColumns = 0;

  update(lines: LineDto[], columns: number): VisualRow[] {
    const prev = this.lines;
    const k = prev.length;
    // Longest common prefix by reference: the append shape from state.ts is
    // `old[0..base)` (same refs) plus new DTOs, where the previously-last
    // line arrives as a NEW object when it grew — so p === k - 1 is the
    // common grown-last-line case, p === k a pure append.
    let p = 0;
    const max = Math.min(k, lines.length);
    while (p < max && prev[p] === lines[p]) {
      p++;
    }
    const canReuse = columns === this.currentColumns && p >= Math.max(1, k - 1) && k > 0;
    if (canReuse) {
      const keepRows = this.rowStarts[p - 1]!;
      const tail = computeRows(lines.slice(p - 1), columns, p - 1);
      this.rows = this.rows.slice(0, keepRows).concat(tail);
    } else {
      this.rows = computeRows(lines, columns);
    }
    this.currentColumns = columns;
    this.lines = lines;
    this.rowStarts = new Array(lines.length);
    let r = 0;
    for (let i = 0; i < lines.length; i++) {
      this.rowStarts[i] = r;
      r += this.rows[r]?.rowCount ?? 1;
    }
    return this.rows;
  }
}
