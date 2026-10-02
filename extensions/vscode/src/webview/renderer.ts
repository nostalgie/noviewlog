/**
 * Canvas renderer: paints only the visible slice of visual rows. Soft-wrap
 * is computed in TypeScript (wrap.ts); the canvas is a dumb painter. Draw
 * calls are coalesced with requestAnimationFrame; the scroll container is
 * native, sized by a spacer canvas whose height reflects the row count.
 */

import type { LineDto, SegDto } from "../protocol";
import { computeRows, type VisualRow } from "./wrap";

export interface RendererTheme {
  fontFamily: string;
  fontSize: number;
  lineHeight: number;
  charWidth: number;
  fg: string;
  bg: string;
}

const ANSI_BLACK = "#000000";

export class Renderer {
  private canvas: HTMLCanvasElement;
  private ctx: CanvasRenderingContext2D;
  private scroller: HTMLElement;
  private rows: VisualRow[] = [];
  private lines: LineDto[] = [];
  private theme: RendererTheme;
  private columns = 0;
  private drawScheduled = false;
  /** dpr captured at relayout time; draw reuses it so bitmap and scale
   * never disagree within one frame. */
  private dpr = 1;

  constructor(
    canvas: HTMLCanvasElement,
    scroller: HTMLElement,
    theme: RendererTheme,
    private readonly getDpr: () => number,
  ) {
    this.canvas = canvas;
    this.scroller = scroller;
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      throw new Error("canvas 2d context unavailable");
    }
    this.ctx = ctx;
    this.theme = theme;
  }

  setTheme(theme: RendererTheme): void {
    this.theme = theme;
    this.scheduleDraw();
  }

  setLines(lines: LineDto[], wrap: boolean): void {
    this.lines = lines;
    this.relayout(wrap);
  }

  /** Recompute rows after a container size or wrap change. */
  relayout(wrap: boolean): void {
    if (this.scroller.clientWidth === 0 || this.scroller.clientHeight === 0) {
      return; // hidden webview: a degenerate grid would explode row counts
    }
    const dpr = this.getDpr();
    this.dpr = dpr;
    const cssWidth = this.scroller.clientWidth;
    const charWidth = this.theme.charWidth;
    this.columns = wrap && charWidth > 0 ? Math.max(1, Math.floor(cssWidth / charWidth)) : 0;
    this.rows = computeRows(this.lines, this.columns);
    // The canvas is viewport-sized: Chromium caps bitmap dimensions far
    // below the scroll height of large buffers, so the spacer div carries
    // the scroll height and the canvas paints only the visible slice.
    const cssHeight = Math.max(1, this.scroller.clientHeight);
    this.canvas.width = Math.max(1, Math.floor(cssWidth * dpr));
    this.canvas.height = Math.max(1, Math.floor(cssHeight * dpr));
    this.canvas.style.width = `${cssWidth}px`;
    this.canvas.style.height = `${cssHeight}px`;
    this.spacer().style.height = `${this.rows.length * this.theme.lineHeight}px`;
    this.scheduleDraw();
  }

  private spacerEl: HTMLElement | null = null;

  private spacer(): HTMLElement {
    this.spacerEl ??=
      this.scroller.querySelector<HTMLElement>("#spacer") ??
      (() => {
        const el = document.createElement("div");
        el.id = "spacer";
        this.scroller.prepend(el);
        return el;
      })();
    return this.spacerEl;
  }

  scheduleDraw(): void {
    if (this.drawScheduled) {
      return;
    }
    this.drawScheduled = true;
    requestAnimationFrame(() => {
      this.drawScheduled = false;
      this.draw();
    });
  }

  /** Pixel row range currently visible in the scroll container. */
  private visibleRange(): { first: number; last: number } {
    const rowH = this.theme.lineHeight;
    const first = Math.max(0, Math.floor(this.scroller.scrollTop / rowH) - 5);
    const count = Math.ceil(this.scroller.clientHeight / rowH) + 10;
    return { first, last: Math.min(this.rows.length, first + count) };
  }

  private draw(): void {
    const dpr = this.dpr;
    const ctx = this.ctx;
    ctx.save();
    ctx.scale(dpr, dpr);
    ctx.fillStyle = this.theme.bg;
    ctx.fillRect(0, 0, this.canvas.width / dpr, this.canvas.height / dpr);
    ctx.font = `${this.theme.fontSize}px ${this.theme.fontFamily}`;
    ctx.textBaseline = "top";

    const { first, last } = this.visibleRange();
    const rowH = this.theme.lineHeight;
    const scrollOffset = this.scroller.scrollTop;

    // Selection overlay paints UNDER the glyphs, like a real terminal.
    if (this.selection) {
      const a = this.selection.anchor;
      const b = this.selection.head;
      const [from, to] =
        a.row > b.row || (a.row === b.row && a.col > b.col) ? [b, a] : [a, b];
      ctx.fillStyle = "rgba(90, 140, 220, 0.35)";
      for (let r = Math.max(from.row, first); r <= Math.min(to.row, last - 1); r++) {
        const vrow = this.rows[r];
        if (!vrow) {
          continue;
        }
        const textLen = vrow.end - vrow.start;
        const startCol = r === from.row ? Math.min(from.col, textLen) : 0;
        const endCol = r === to.row ? Math.min(to.col, textLen) : textLen;
        if (endCol <= startCol) {
          continue;
        }
        ctx.fillRect(
          startCol * this.theme.charWidth,
          r * rowH - scrollOffset,
          (endCol - startCol) * this.theme.charWidth,
          rowH,
        );
      }
    }

    for (let r = first; r < last; r++) {
      const row = this.rows[r];
      if (!row) {
        continue;
      }
      const line = this.lines[row.line];
      if (!line) {
        continue;
      }
      this.drawRow(ctx, row, line, r * rowH - scrollOffset);
    }

    // Input-ready caret: a block on the last cell of the live tail.
    if (this.cursor && this.cursorLit) {
      const cell = this.cursor;
      const row = this.rows[cell.row];
      const colWidth = this.theme.charWidth;
      ctx.fillStyle = "rgba(120, 200, 120, 0.85)";
      ctx.fillRect(
        cell.col * colWidth,
        cell.row * rowH - scrollOffset,
        Math.max(colWidth * 0.6, colWidth - 1),
        rowH - 1,
      );
    }
    ctx.restore();
  }

  private drawRow(
    ctx: CanvasRenderingContext2D,
    row: VisualRow,
    line: LineDto,
    y: number,
  ): void {
    // Slice the row's [start,end) char range out of the segment list.
    let charCursor = 0;
    let x = 0;
    for (const seg of line.segments) {
      const segLen = seg.text.length;
      const segStart = charCursor;
      const segEnd = segStart + segLen;
      charCursor = segEnd;
      if (segEnd <= row.start || segStart >= row.end) {
        continue;
      }
      const from = Math.max(row.start, segStart) - segStart;
      const to = Math.min(row.end, segEnd) - segStart;
      const text = seg.text.slice(from, to);
      if (text.length === 0) {
        continue;
      }
      this.styleSegment(ctx, seg, text, x, y);
      x += ctx.measureText(text).width;
    }
  }

  private styleSegment(
    ctx: CanvasRenderingContext2D,
    seg: SegDto,
    text: string,
    x: number,
    y: number,
  ): void {
    const fg = seg.fg ? rgbCss(seg.fg) : this.theme.fg;
    if (seg.bg) {
      ctx.save();
      ctx.fillStyle = rgbCss(seg.bg);
      ctx.fillRect(x, y, this.ctx.measureText(text).width, this.theme.lineHeight);
      ctx.restore();
    }
    if (seg.search) {
      ctx.save();
      ctx.fillStyle = seg.search_current
        ? "rgba(255, 170, 0, 0.55)"
        : "rgba(124, 124, 124, 0.35)";
      ctx.fillRect(x, y, this.ctx.measureText(text).width, this.theme.lineHeight);
      ctx.restore();
    }
    ctx.fillStyle = seg.dim ? dimColor(fg) : fg;
    ctx.fillText(text, x, y + 1);
    if (seg.underline) {
      ctx.fillRect(x, y + this.theme.fontSize, ctx.measureText(text).width, 1);
    }
  }

  /** Scroll so the given flat-line index is visible (follow/search jumps). */
  revealLine(index: number): void {
    const rowIdx = this.rows.findIndex((r) => r.line === index && r.row === 0);
    if (rowIdx >= 0) {
      this.scroller.scrollTop = rowIdx * this.theme.lineHeight;
    }
  }

  get totalHeight(): number {
    return this.rows.length * this.theme.lineHeight;
  }

  // ----- selection (mouse-tracking off only) -----

  /** Visual-row index and character column within the row's text. */
  cellAt(cssX: number, cssY: number): { row: number; col: number } {
    const row = Math.max(
      0,
      Math.min(this.rows.length - 1, Math.floor((cssY + this.scroller.scrollTop) / this.theme.lineHeight)),
    );
    const vrow = this.rows[row];
    const rowLen = vrow ? vrow.end - vrow.start : 0;
    const col = Math.max(0, Math.min(rowLen, Math.round(cssX / this.theme.charWidth)));
    return { row, col };
  }

  setSelection(anchor: { row: number; col: number }, head: { row: number; col: number }): void {
    this.selection = { anchor, head };
    this.scheduleDraw();
  }

  currentSelectionAnchor(): { row: number; col: number } {
    return this.selection?.anchor ?? { row: 0, col: 0 };
  }

  // ----- input-ready caret -----

  private cursor: { row: number; col: number } | null = null;
  private cursorLit = true;

  /** Block-caret cell: the first empty cell after the last visible row —
   * the live tail always ends where the shell prompt cursor sits. */
  private tailCursorCell(): { row: number; col: number } {
    const r = Math.max(0, this.rows.length - 1);
    const vrow = this.rows[r];
    return { row: r, col: vrow ? vrow.end - vrow.start : 0 };
  }

  /** Show the caret iff `on`; the cell follows the live tail. */
  setCursor(on: boolean): void {
    const next = on ? this.tailCursorCell() : null;
    const moved =
      (next === null) !== (this.cursor === null) ||
      (next !== null &&
        this.cursor !== null &&
        (next.row !== this.cursor.row || next.col !== this.cursor.col));
    this.cursor = next;
    if (moved) {
      this.cursorLit = true;
      this.scheduleDraw();
    }
  }

  /** Blink phase; a flip only redraws when a caret is shown. */
  setCursorLit(lit: boolean): void {
    if (this.cursor && this.cursorLit !== lit) {
      this.cursorLit = lit;
      this.scheduleDraw();
    }
  }

  clearSelection(): void {
    if (this.selection) {
      this.selection = null;
      this.scheduleDraw();
    }
  }

  /** Selected text, rows joined with "\n" (empty when nothing selected). */
  selectionText(): string {
    if (!this.selection) {
      return "";
    }
    const a = this.selection.anchor;
    const b = this.selection.head;
    const [from, to] =
      a.row > b.row || (a.row === b.row && a.col > b.col) ? [b, a] : [a, b];
    const out: string[] = [];
    for (let r = from.row; r <= to.row && r < this.rows.length; r++) {
      const vrow = this.rows[r];
      if (!vrow) {
        continue;
      }
      const text = this.rowText(vrow);
      const start = r === from.row ? Math.min(from.col, text.length) : 0;
      const end = r === to.row ? Math.min(to.col, text.length) : text.length;
      out.push(text.slice(start, end));
    }
    return out.join("\n");
  }

  private rowText(vrow: VisualRow): string {
    return this.lines[vrow.line]?.raw.slice(vrow.start, vrow.end) ?? "";
  }

  private selection: { anchor: { row: number; col: number }; head: { row: number; col: number } } | null =
    null;
}

function rgbCss(rgb: [number, number, number]): string {
  return `rgb(${rgb[0]},${rgb[1]},${rgb[2]})`;
}

function dimColor(color: string): string {
  return color === ANSI_BLACK ? color : `color-mix(in srgb, ${color} 60%, transparent)`;
}

/** Measure the monospace grid once at startup. */
export function measureTheme(
  fontFamily: string,
  fontSize: number,
  lineHeightRatio: number,
  fg: string,
  bg: string,
): RendererTheme {
  const probe = document.createElement("canvas");
  const ctx = probe.getContext("2d");
  if (!ctx) {
    return {
      fontFamily,
      fontSize,
      lineHeight: fontSize * lineHeightRatio,
      charWidth: fontSize * 0.6,
      fg,
      bg,
    };
  }
  ctx.font = `${fontSize}px ${fontFamily}`;
  return {
    fontFamily,
    fontSize,
    lineHeight: Math.round(fontSize * lineHeightRatio),
    charWidth: ctx.measureText("M").width,
    fg,
    bg,
  };
}
