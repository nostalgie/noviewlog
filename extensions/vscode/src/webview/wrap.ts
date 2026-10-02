/**
 * Soft-wrap: turn flat lines into visual rows by measuring the monospace
 * grid in *display cells* (wide CJK = 2, combining = 0), so the canvas
 * renderer stays a dumb painter and wrap logic stays unit-testable. Cut
 * positions are UTF-16 unit offsets that never split a code point.
 */

import type { LineDto, SegDto } from "../protocol";

export interface VisualRow {
  /** Index into the flat-line array. */
  line: number;
  /** First visual row of the line (for continuation styling). */
  row: number;
  rowCount: number;
  /** Character slice [start, end) of `raw` painted on this row. */
  start: number;
  end: number;
}

/** Split a segment list into (char, segment) runs per segment. */
function segmentCharCount(seg: SegDto): number {
  return seg.text.length;
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
export function computeRows(lines: LineDto[], columns: number): VisualRow[] {
  const rows: VisualRow[] = [];
  for (let i = 0; i < lines.length; i++) {
    const raw = lines[i]!.raw;
    const cuts = rowCuts(raw, columns);
    // Row r spans [prevCut, cuts[r]) — cuts holds row END offsets and
    // implicitly starts at 0.
    const rowCount = cuts.length;
    for (let r = 0; r < rowCount; r++) {
      rows.push({ line: i, row: r, rowCount, start: r === 0 ? 0 : cuts[r - 1]!, end: cuts[r]! });
    }
  }
  return rows;
}

/**
 * Total row count without materializing rows (scroll spacer sizing).
 */
export function countRows(lines: LineDto[], columns: number): number {
  let total = 0;
  for (const line of lines) {
    total += rowCuts(line.raw, columns).length;
  }
  return total;
}

/** True if the char index falls inside the segment's text span. */
export function charInSegment(seg: SegDto, index: number): boolean {
  return index >= 0 && index < segmentCharCount(seg);
}
