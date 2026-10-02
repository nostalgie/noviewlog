import { describe, expect, it } from "vitest";
import type { SessionAppendMsg, SessionSnapshotMsg } from "../src/protocol";
import { applyMessage, emptyState, searchBadge } from "../src/webview/state";

const snapshot: SessionSnapshotMsg = {
  type: "snapshot",
  session_id: 1,
  name: "cmd",
  source: "pty",
  finished: false,
  active_tab: 0,
  tabs: [{ index: 0, name: "Terminal", active: true, terminal: true }],
  dropped_records: 0,
  buffer_records: 2,
  buffer_max: 10000,
  view: {
    name: "Terminal",
    severity: "all",
    follow: true,
    wrap: false,
    total_lines: 2,
    epoch: 1,
    lines: [
      {
        record_id: 1,
        line_index: 0,
        raw: "one",
        segments: [{ text: "one" }],
        level: null,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
      },
      {
        record_id: 2,
        line_index: 0,
        raw: "two",
        segments: [{ text: "two" }],
        level: null,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
      },
    ],
    search: {
      query: "",
      regex: false,
      case_sensitive: false,
      whole_word: false,
      error: null,
      label: "",
      match_count: 0,
      active_line: null,
      scroll_request: 0,
    },
    filters_locked: true,
    filters: [],
    notice: null,
  },
};

function append(overrides: Partial<SessionAppendMsg> = {}): SessionAppendMsg {
  return {
    type: "append",
    ok: true,
    epoch: 1,
    base: 2,
    total_lines: 3,
    lines: [
      {
        record_id: 3,
        line_index: 0,
        raw: "three",
        segments: [{ text: "three" }],
        level: null,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
      },
    ],
    search: snapshot.view.search,
    follow: true,
    dropped_records: 0,
    buffer_records: 3,
    ...overrides,
  };
}

describe("applyMessage", () => {
  it("replaces state from a full snapshot", () => {
    const state = applyMessage(emptyState(), snapshot);
    expect(state.session?.name).toBe("cmd");
    expect(state.session?.view.total_lines).toBe(2);
  });

  it("extends lines from a valid append", () => {
    let state = applyMessage(emptyState(), snapshot);
    state = applyMessage(applyMessage(emptyState(), snapshot), append());
    expect(state.session?.view.lines.map((l) => l.raw)).toEqual(["one", "two", "three"]);
    expect(state.session?.view.total_lines).toBe(3);
  });

  it("truncates past base before extending", () => {
    let state = applyMessage(emptyState(), snapshot);
    // base 1 drops "two", then adds "three".
    state = applyMessage(applyMessage(emptyState(), snapshot), append({ base: 1, total_lines: 2 }));
    expect(state.session?.view.lines.map((l) => l.raw)).toEqual(["one", "three"]);
  });

  it("ignores a stale epoch append", () => {
    let state = applyMessage(emptyState(), snapshot);
    state = applyMessage(applyMessage(emptyState(), snapshot), append({ epoch: 999 }));
    expect(state.session?.view.total_lines).toBe(2);
  });

  it("ignores an append past the total", () => {
    let state = applyMessage(emptyState(), snapshot);
    state = applyMessage(applyMessage(emptyState(), snapshot), append({ base: 5 }));
    expect(state.session?.view.total_lines).toBe(2);
  });

  it("records exit status", () => {
    let state = applyMessage(emptyState(), snapshot);
    state = applyMessage(applyMessage(emptyState(), snapshot), { type: "exit", status: "exit code 1" });
    expect(state.session?.finished).toBe(true);
    expect(state.exitStatus).toBe("exit code 1");
  });
});

describe("searchBadge", () => {
  it("prefers the error text", () => {
    expect(
      searchBadge({ ...snapshot.view.search, error: "Invalid regex", query: "[" }),
    ).toBe("Invalid regex");
  });

  it("shows the counter label for an active query", () => {
    expect(searchBadge({ ...snapshot.view.search, query: "x", label: "2/5" })).toBe("2/5");
  });

  it("is empty without a query", () => {
    expect(searchBadge(snapshot.view.search)).toBe("");
  });
});

describe("exit status", () => {
  it("clears the previous session's exit status on a new session snapshot", () => {
    const exited = applyMessage(applyMessage(emptyState(), snapshot), { type: "exit", status: "exit code 1" });
    const next = applyMessage(exited, {
      ...snapshot,
      session_id: snapshot.session_id + 1,
    });
    expect(next.exitStatus).toBeNull();
  });

  it("keeps the exit status across reveals of the same session", () => {
    const exited = applyMessage(applyMessage(emptyState(), snapshot), { type: "exit", status: "exit code 1" });
    const next = applyMessage(exited, snapshot);
    expect(next.exitStatus).toBe("exit code 1");
  });
});
