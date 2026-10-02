/**
 * Integration tests over the real wasm facade (dist/wasm). These run the
 * same code path as the extension host in plain Node, so engine regressions
 * (ingest, filters, tabs, persistence) are caught without launching VS Code.
 * Requires a prior `npm run build:wasm` + `npm run compile`.
 */

import { createRequire } from "node:module";
import { describe, expect, it } from "vitest";

const require = createRequire(import.meta.url);
interface WasmModule {
  WebEngine: new () => WasmEngine;
}
// The wasm-bindgen nodejs glue is CJS; vitest's interop may wrap it in
// `default`, so unwrap before use.
// eslint-disable-next-line @typescript-eslint/no-unsafe-assignment
const glue = require("../dist/wasm/noviewlog_wasm.js") as WasmModule & { default?: WasmModule };
const wasm: WasmModule = glue.WebEngine ? glue : (glue.default as WasmModule);

interface TabInfo {
  name: string;
  active: boolean;
  terminal: boolean;
}

interface FilterRuleDto {
  id: string;
  type: "include" | "exclude";
  pattern: string;
  enabled?: boolean;
  use_regex: boolean;
}

interface ViewSnapshot {
  total_lines: number;
  severity: string;
  filters: FilterRuleDto[];
  filters_locked: boolean;
  search: { query: string; label: string; error: string | null };
}

interface Snapshot {
  session_id: number;
  tabs: TabInfo[];
  active_tab: number;
  view: ViewSnapshot;
}

interface WasmEngine {
  session_create: (name: string, source: string) => number;
  session_ingest: (id: number, bytes: Uint8Array) => void;
  session_flush_pending: (id: number) => boolean;
  session_finish: (id: number) => void;
  set_max_records: (max: number) => void;
  toggle_collapse: (id: number, recordId: number) => void;
  expand_all: (id: number) => void;
  session_snapshot: (id: number) => Snapshot;
  session_append_since: (id: number, epoch: number, base: number) => Record<string, unknown> & { ok: boolean };
  set_severity: (id: number, mode: string) => void;
  search_set: (id: number, query: string, regex: boolean, cs: boolean, ww: boolean) => void;
  filter_set: (id: number, rules: object) => string | undefined;
  tab_add: (id: number) => void;
  tab_switch: (id: number, index: number) => void;
  tab_close: (id: number, index: number) => void;
  tabs_export: (id: number) => { tabs: object[]; active_tab: number };
  tabs_import: (id: number, config: object) => void;
}

const encoder = new TextEncoder();

function newEngine(): WasmEngine {
  return new wasm.WebEngine();
}

/** Ingest lines, flush pending tail, finish the session. */
function feed(engine: WasmEngine, id: number, lines: string[]): void {
  for (const line of lines) {
    engine.session_ingest(id, encoder.encode(`${line}\r\n`));
  }
  engine.session_flush_pending(id);
  engine.session_finish(id);
}

const LEVELS = ["INFO", "WARN", "ERROR"];

describe("engine facade: session lifecycle", () => {
  it("ingests and snapshots records", () => {
    const engine = newEngine();
    const id = engine.session_create("test.log", "file");
    feed(engine, id, Array.from({ length: 10 }, (_, i) => `line ${i} ${LEVELS[i % 3]}`));
    const snap = engine.session_snapshot(id);
    expect(snap.view.total_lines).toBe(10);
    expect(snap.tabs[0]?.terminal).toBe(true);
  });

  it("serves incremental appends and invalidates on epoch bump", () => {
    const engine = newEngine();
    const id = engine.session_create("t", "file");
    engine.session_ingest(id, encoder.encode("line 0 INFO\r\n"));
    engine.session_flush_pending(id);
    const snap = engine.session_snapshot(id);
    const append = engine.session_append_since(id, snap.view.epoch as unknown as number, snap.view.total_lines) as {
      ok: boolean;
      lines: unknown[];
    };
    expect(append.ok).toBe(true);
    expect(append.lines).toHaveLength(0);

    engine.session_ingest(id, encoder.encode("line 1 INFO\r\n"));
    engine.session_flush_pending(id);
    const more = engine.session_append_since(id, snap.view.epoch as unknown as number, snap.view.total_lines) as {
      ok: boolean;
      lines: { raw: string }[];
    };
    expect(more.ok).toBe(true);
    expect(more.lines).toHaveLength(1);
  });
});

describe("engine facade: filters", () => {
  it("includes matching lines on a filter tab", () => {
    const engine = newEngine();
    const id = engine.session_create("t", "file");
    feed(engine, id, Array.from({ length: 9 }, (_, i) => `line ${i} ${LEVELS[i % 3]}`));
    engine.tab_add(id);
    const notice = engine.filter_set(id, [
      { id: "r1", type: "include", pattern: "ERROR", use_regex: false },
    ]);
    expect(notice).toBeUndefined();
    const snap = engine.session_snapshot(id);
    expect(snap.view.total_lines).toBe(3);
    expect(snap.tabs).toHaveLength(2);
  });

  it("refuses filters on the Terminal tab", () => {
    const engine = newEngine();
    const id = engine.session_create("t", "file");
    feed(engine, id, ["line 0 INFO"]);
    expect(() =>
      engine.filter_set(id, [{ id: "r", type: "include", pattern: "x", use_regex: false }]),
    ).toThrow();
  });

  it("filters survive a tabs export/import round trip", () => {
    const engine = newEngine();
    const id = engine.session_create("t", "file");
    feed(engine, id, ["line 0 INFO", "line 1 ERROR"]);
    engine.tab_add(id);
    engine.tab_switch(id, 1);
    engine.filter_set(id, [{ id: "r1", type: "include", pattern: "ERROR", use_regex: false }]);
    engine.set_severity(id, "warn");
    const saved = engine.tabs_export(id);

    const fresh = newEngine();
    const id2 = fresh.session_create("t", "file");
    fresh.tabs_import(id2, saved);
    const snap = fresh.session_snapshot(id2);
    expect(snap.tabs.map((t) => t.name)).toEqual(["Terminal", "Tab 2"]);
    expect(snap.active_tab).toBe(1);
    expect(snap.view.filters).toHaveLength(1);
    expect(snap.view.severity).toBe("warn");
  });
});

describe("engine facade: search", () => {
  it("counts matches and reports match navigation metadata", () => {
    const engine = newEngine();
    const id = engine.session_create("t", "file");
    feed(engine, id, ["line 1 INFO", "needle INFO", "needle WARN"]);
    engine.search_set(id, "needle", false, false, false);
    const snap = engine.session_snapshot(id);
    expect(snap.view.search.query).toBe("needle");
    // Two matches; the view starts pinned to the last one (follow).
    expect(snap.view.search.label).toBe("2/2");
    engine.search_next(id);
    const wrapped = engine.session_snapshot(id);
    expect(wrapped.view.search.label).toBe("1/2");
    expect(snap.view.search.error ?? null).toBeNull();
  });

  it("falls back to literal on an invalid regex with a notice", () => {
    const engine = newEngine();
    const id = engine.session_create("t", "file");
    feed(engine, id, ["a ( b INFO"]);
    engine.search_set(id, "(unclosed", true, false, false);
    const snap = engine.session_snapshot(id);
    expect(snap.view.search.error).not.toBeNull();
  });
});

describe("engine facade: severity modes", () => {
  it("shows only lines of the selected severity", () => {
    const engine = newEngine();
    const id = engine.session_create("t", "file");
    feed(engine, id, [
      "start INFO",
      "boom ERROR",
      "careful WARN",
      "done INFO",
      "fatal ERROR",
    ]);
    engine.set_severity(id, "error");
    const snap = engine.session_snapshot(id);
    expect(snap.view.severity).toBe("error");
    expect(snap.view.total_lines).toBe(2);
    engine.set_severity(id, "all");
    expect(engine.session_snapshot(id).view.total_lines).toBe(5);
  });
});

describe("engine facade: collapse", () => {
  // Multiline records (stack traces) default to collapsed, as on desktop.
  it("multiline records start collapsed and toggle/expand", () => {
    const engine = newEngine();
    const id = engine.session_create("t", "file");
    feed(engine, id, [
      "Error: boom ERROR",
      "    at app.main(app.rs:1)",
      "    at lib.run(lib.rs:9)",
      "after INFO",
    ]);
    let snap = engine.session_snapshot(id);
    expect(snap.view.total_lines).toBe(2);
    expect(snap.view.lines[0]?.collapsible).toBe(true);
    expect(snap.view.lines[0]?.collapsed).toBe(true);
    engine.toggle_collapse(id, snap.view.lines[0]!.record_id);
    snap = engine.session_snapshot(id);
    expect(snap.view.total_lines).toBe(4);
    expect(snap.view.lines[0]?.collapsed).toBe(false);
    engine.collapse_all(id);
    snap = engine.session_snapshot(id);
    expect(snap.view.total_lines).toBe(2);
  });
});

describe("engine facade: large buffers", () => {
  it("handles a 20k-line flood and serves incremental appends", () => {
    const engine = newEngine();
    engine.set_max_records(30_000);
    const id = engine.session_create("t", "file");
    feed(
      engine,
      id,
      Array.from({ length: 20_000 }, (_, i) => `line ${i} INFO`),
    );
    const snap = engine.session_snapshot(id);
    expect(snap.view.total_lines).toBe(20_000);
    const append = engine.session_append_since(id, snap.view.epoch as unknown as number, 19_990) as {
      ok: boolean;
      lines: unknown[];
    };
    expect(append.ok).toBe(true);
    expect(append.lines).toHaveLength(10);
  });

  it("drops oldest records past the ring cap and invalidates appends", () => {
    const engine = newEngine();
    engine.set_max_records(100);
    const id = engine.session_create("t", "file");
    // One chunk, session stays open (finish would freeze the epoch).
    const flood = Array.from({ length: 1_000 }, (_, i) => `line ${i} INFO`).join("\r\n");
    engine.session_ingest(id, encoder.encode(`${flood}\r\n`));
    engine.session_flush_pending(id);
    const snap = engine.session_snapshot(id);
    expect(snap.buffer_records).toBe(100);
    // Some lines group into one record, so the dropped count is close to
    // 900 but not exact.
    expect(snap.dropped_records).toBeGreaterThan(800);
    expect(snap.view.lines[0]?.raw).not.toContain("line 0");

    // A later shift bumps the epoch; deltas against the old epoch fail.
    const staleEpoch = snap.view.epoch as unknown as number;
    engine.session_ingest(id, encoder.encode("extra INFO\r\n"));
    engine.session_flush_pending(id);
    const append = engine.session_append_since(id, staleEpoch, 10) as { ok: boolean };
    expect(append.ok).toBe(false);
  });
});
