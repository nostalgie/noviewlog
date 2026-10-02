import { describe, expect, it } from "vitest";
import type { LineDto } from "../src/protocol";
import { computeRows, countRows } from "../src/webview/wrap";

function line(raw: string): LineDto {
  return {
    record_id: 1,
    line_index: 0,
    segments: [{ text: raw }],
    raw,
    level: null,
    collapsible: false,
    collapsed: false,
    hidden_line_count: 0,
  };
}

describe("computeRows", () => {
  it("one row per line when wrapping is disabled", () => {
    const lines = [line("a"), line("bb"), line("ccc")];
    const rows = computeRows(lines, 0);
    expect(rows).toHaveLength(3);
    expect(rows[0]).toMatchObject({ line: 0, row: 0, rowCount: 1, start: 0, end: 1 });
  });

  it("splits long lines into column-sized rows", () => {
    const lines = [line("abcdefghij")];
    const rows = computeRows(lines, 4);
    expect(rows).toHaveLength(3);
    expect(rows[0]).toMatchObject({ row: 0, rowCount: 3, start: 0, end: 4 });
    expect(rows[1]).toMatchObject({ row: 1, rowCount: 3, start: 4, end: 8 });
    expect(rows[2]).toMatchObject({ row: 2, rowCount: 3, start: 8, end: 10 });
  });

  it("keeps empty lines as a single empty row", () => {
    const rows = computeRows([line("")], 4);
    expect(rows).toHaveLength(1);
    expect(rows[0]).toMatchObject({ start: 0, end: 0 });
  });
});

describe("countRows", () => {
  it("aggregates without materializing rows", () => {
    const lines = [line("aaaa"), line(""), line("aaaaaa")];
    // 1 + 1 (empty line still paints one row) + 2 = 4 visual rows.
    expect(countRows(lines, 4)).toBe(4);
    expect(countRows(lines, 0)).toBe(3);
  });
});

describe("surrogate pairs", () => {
  it("does not split an astral character across rows", () => {
    // "a" + U+1F600 (surrogate pair) + "bbbb": a naive cut at column 2
    // would split the pair.
    const rows = computeRows([line("a\ud83d\ude00bbbb")], 2);
    const painted = rows.map((r) => "a\ud83d\ude00bbbb".slice(r.start, r.end)).join("");
    expect(painted).toBe("a\ud83d\ude00bbbb");
    for (const r of rows) {
      const text = "a\ud83d\ude00bbbb".slice(r.start, r.end);
      expect(text).not.toMatch(/[\ud800-\udbff](?![\udc00-\udfff])/);
    }
  });

  it("countRows agrees with computeRows on surrogate content", () => {
    const lines = [line("a\ud83d\ude00bbbb")];
    expect(countRows(lines, 2)).toBe(computeRows(lines, 2).length);
  });
});

describe("display cells", () => {
  it("wraps CJK text by cells, not UTF-16 units", () => {
    const rows = computeRows([line("日本語")], 4);
    expect(rows).toHaveLength(2);
    // First row holds the two wide glyphs (4 cells), second the tail.
    expect(rows[0]).toMatchObject({ start: 0, end: 2 });
    expect(rows[1]).toMatchObject({ start: 2, end: 3 });
  });

  it("never splits a surrogate pair (emoji = 2 cells)", () => {
    const raw = "a\ud83d\ude00bbbb";
    const rows = computeRows([line(raw)], 2);
    for (const r of rows) {
      const text = raw.slice(r.start, r.end);
      expect(text).not.toMatch(/[\ud800-\udbff](?![\udc00-\udfff])/);
    }
    expect(countRows([line(raw)], 2)).toBe(rows.length);
  });

  it("zero-width combining marks do not add cells", () => {
    // e + combining acute = 1 cell; 3 such lines chars fit into 3 columns.
    const rows = computeRows([line("e\u0301e\u0301e\u0301x")], 3);
    expect(rows).toHaveLength(2);
    expect(rows[0]).toMatchObject({ start: 0, end: 6 });
  });

  it("countRows matches computeRows for ascii", () => {
    const lines = [line("aaaa"), line(""), line("aaaaaa")];
    expect(countRows(lines, 4)).toBe(4);
    expect(countRows(lines, 0)).toBe(3);
  });
});
