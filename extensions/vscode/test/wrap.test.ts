import { describe, expect, it } from "vitest";
import type { LineDto } from "../src/protocol";
import { computeRows, RowLayout, type VisualRow } from "../src/webview/wrap";

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
    expect(rows.length).toBeGreaterThan(0);
  });

  it("zero-width combining marks do not add cells", () => {
    // e + combining acute = 1 cell; 3 such lines chars fit into 3 columns.
    const rows = computeRows([line("e\u0301e\u0301e\u0301x")], 3);
    expect(rows).toHaveLength(2);
    expect(rows[0]).toMatchObject({ start: 0, end: 6 });
  });

});

describe("RowLayout", () => {
  // Issue #322: appends must reuse the prefix rows (object identity) and
  // always produce rows equal to a full recompute.
  function updateAll(
    layouts: LineDto[][],
    columns: number,
  ): { rows: VisualRow[]; identities: VisualRow[][] } {
    const layout = new RowLayout();
    const identities: VisualRow[][] = [];
    let rows: VisualRow[] = [];
    for (const lines of layouts) {
      rows = layout.update(lines, columns);
      identities.push([...rows]);
    }
    return { rows, identities };
  }

  it("append reuses prefix rows and equals a full recompute", () => {
    // Real append shape (state.ts): same refs for the common prefix, the
    // previously-last line arrives as a NEW dto when it grew.
    const base = [line("aaaa"), line("bb")];
    const grown = [base[0]!, line("bbcc"), line("dd")];
    const appended = [...grown, line("ee")];
    const { rows, identities } = updateAll([base, grown, appended], 3);
    const last = identities[identities.length - 1]!;
    expect(rows).toEqual(computeRows(appended, 3));
    // A pure append keeps the untouched lines' row objects (identity).
    expect(last[0]).toBe(identities[1]![0]);
    expect(last[1]).toBe(identities[1]![1]);
    // The grown-last step (p = k-1) still equals a full recompute.
    expect(identities[1]).toEqual(computeRows(grown, 3));
  });

  it("column change recomputes fully", () => {
    const base = [line("aaaa"), line("bb")];
    const layout = new RowLayout();
    layout.update(base, 4);
    const rows = layout.update(base, 2);
    expect(rows).toEqual(computeRows(base, 2));
  });

  it("truncation (overlay replace) recomputes fully", () => {
    const base = [line("aaaa"), line("bb"), line("cc")];
    const layout = new RowLayout();
    layout.update(base, 4);
    const shorter = [base[0]!, line("zz")];
    const rows = layout.update(shorter, 4);
    expect(rows).toEqual(computeRows(shorter, 4));
  });

  it("wrap off: one row per line, appends still equal full recompute", () => {
    const base = [line("a"), line("b")];
    const appended = [...base, line("c")];
    const layout = new RowLayout();
    layout.update(base, 0);
    const rows = layout.update(appended, 0);
    expect(rows).toEqual(computeRows(appended, 0));
    expect(rows).toHaveLength(3);
  });
});
