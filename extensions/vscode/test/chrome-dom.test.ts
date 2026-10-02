// @vitest-environment jsdom
/**
 * DOM tests for the slim chrome bar (jsdom): tab strip, Find bar, exit
 * badge, and the filter-panel toggle. Rule editing lives in the filter
 * panel (filters-panel.test.ts).
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { SessionSnapshotMsg, WebviewToHost } from "../src/protocol";
import { Chrome } from "../src/webview/chrome";
import { emptyState, type SessionState } from "../src/webview/state";

function snapshotMsg(overrides: Partial<SessionSnapshotMsg> = {}): SessionSnapshotMsg {
  return {
    type: "snapshot",
    session_id: 1,
    name: "cmd",
    source: "pty",
    finished: false,
    active_tab: 1,
    tabs: [
      { index: 0, name: "Terminal", active: false, terminal: true },
      { index: 1, name: "Tab 2", active: true, terminal: false },
    ],
    dropped_records: 0,
    buffer_records: 2,
    buffer_max: 10000,
    view: {
      name: "Tab 2",
      severity: "all",
      follow: true,
      wrap: false,
      total_lines: 2,
      epoch: 1,
      lines: [],
      filters_locked: false,
      filters: [{ id: "r1", type: "exclude", pattern: "noise", use_regex: false }],
      notice: null,
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
    },
    ...overrides,
  } as SessionSnapshotMsg;
}

function stateFrom(msg: SessionSnapshotMsg, exitStatus: string | null = null): SessionState {
  return { session: { ...msg, exit_status: undefined }, exitStatus } as unknown as SessionState;
}

describe("chrome: panel toggle and tabs", () => {
  let root: HTMLElement;
  let chrome: Chrome;
  let sent: WebviewToHost[];
  let state: SessionState;

  beforeEach(() => {
    vi.useFakeTimers();
    root = document.createElement("div");
    document.body.appendChild(root);
    chrome = new Chrome(root);
    sent = [];
    chrome.onCommand = (cmd) => sent.push(cmd);
    state = stateFrom(snapshotMsg());
    chrome.state = state;
    chrome.render(state);
  });

  afterEach(() => {
    vi.useRealTimers();
    root.remove();
  });

  it("renders the tab strip with close buttons on filter tabs", () => {
    const tabs = [...root.querySelectorAll(".tab")] as HTMLButtonElement[];
    expect(tabs.map((t) => t.textContent)).toEqual(["Terminal", "Tab 2x"]);
    expect(tabs[0]?.classList.contains("terminal")).toBe(true);
    tabs[1]?.querySelector(".tab-close")?.dispatchEvent(new Event("click"));
    expect(sent.at(-1)).toMatchObject({ type: "closeTab", index: 1 });
  });

  it("toggles the filter panel through the chrome button", () => {
    const toggles: boolean[] = [];
    chrome.onTogglePanel = () => toggles.push(true);
    const btn = [...root.querySelectorAll("button")].find((b) => b.textContent === "Filters");
    expect(btn).toBeDefined();
    btn!.click();
    expect(toggles).toEqual([true]);
    chrome.setPanelOpen(true);
    expect(btn!.getAttribute("aria-pressed")).toBe("true");
    chrome.setPanelOpen(false);
    expect(btn!.getAttribute("aria-pressed")).toBe("false");
  });

  it("debounces Find input into one setSearch", () => {
    const find = root.querySelector(".find input") as HTMLInputElement;
    find.value = "needle";
    find.dispatchEvent(new Event("input"));
    find.value = "needle2";
    find.dispatchEvent(new Event("input"));
    vi.advanceTimersByTime(250);
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({ type: "setSearch", query: "needle2" });
  });

  it("shows the exit status badge when a session exits", () => {
    const exit = stateFrom(snapshotMsg(), "exit code 1");
    chrome.render(exit);
    const badge = root.querySelector(".exit-label") as HTMLElement;
    expect(badge.textContent).toBe("exit code 1");
    expect(badge.style.display).not.toBe("none");
  });

  it("renders the empty state for a missing session", () => {
    chrome.render(emptyState());
    expect(root.querySelectorAll(".tab")).toHaveLength(0);
  });
});
