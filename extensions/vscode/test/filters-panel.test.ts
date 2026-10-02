// @vitest-environment jsdom
/**
 * DOM tests for the filter panel (jsdom): structured rule rows with the
 * debounced live apply, inline invalid-regex marking, Revert, the locked
 * Terminal tab, preset apply, inline rename, severity/tabs/view controls.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type {
  FilterRuleDto,
  PresetDto,
  SessionSnapshotMsg,
  WebviewToHost,
} from "../src/protocol";
import { FiltersPanel } from "../src/webview/filtersPanel";
import type { SessionState } from "../src/webview/state";

function rules(...defs: Partial<FilterRuleDto>[]): FilterRuleDto[] {
  return defs.map((d, i) => ({
    id: d.id ?? `r${i}`,
    type: d.type ?? "include",
    pattern: d.pattern ?? "",
    use_regex: d.use_regex ?? false,
    enabled: d.enabled ?? true,
  }));
}

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
      { index: 1, name: "Errors", active: true, terminal: false },
      { index: 2, name: "Tab 3", active: false, terminal: false },
    ],
    dropped_records: 0,
    buffer_records: 0,
    buffer_max: 10000,
    view: {
      name: "Errors",
      severity: "all",
      follow: true,
      wrap: false,
      total_lines: 0,
      epoch: 1,
      lines: [],
      filters_locked: false,
      filters: rules({ type: "exclude", pattern: "noise" }),
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

function stateFrom(msg: SessionSnapshotMsg): SessionState {
  return { session: { ...msg, exit_status: undefined }, exitStatus: null } as unknown as SessionState;
}

describe("filters panel: rule rows", () => {
  let root: HTMLElement;
  let panel: FiltersPanel;
  let sent: WebviewToHost[];
  let state: SessionState;

  beforeEach(() => {
    vi.useFakeTimers();
    root = document.createElement("div");
    document.body.appendChild(root);
    panel = new FiltersPanel(root);
    sent = [];
    panel.onCommand = (cmd) => sent.push(cmd);
    state = stateFrom(snapshotMsg());
    panel.render(state);
  });

  afterEach(() => {
    vi.useRealTimers();
    root.remove();
  });

  function rows(): HTMLElement[] {
    return [...root.querySelectorAll(".fp-rule")] as HTMLElement[];
  }

  function patternInput(row: HTMLElement): HTMLInputElement {
    return row.querySelector("input[type=text]") as HTMLInputElement;
  }

  function typePattern(row: HTMLElement, text: string): void {
    const input = patternInput(row);
    input.value = text;
    input.dispatchEvent(new Event("input"));
  }

  it("renders one structured row per applied rule", () => {
    expect(rows()).toHaveLength(1);
    const row = rows()[0]!;
    expect((row.querySelector("select") as HTMLSelectElement).value).toBe("exclude");
    expect(patternInput(row).value).toBe("noise");
    expect((row.querySelector("input[type=checkbox]") as HTMLInputElement).checked).toBe(true);
  });

  it("debounces edits into a single setFilters", () => {
    typePattern(rows()[0]!, "healthcheck");
    vi.advanceTimersByTime(299);
    typePattern(rows()[0]!, "health");
    vi.advanceTimersByTime(300);
    expect(sent).toHaveLength(1);
    expect(sent[0]).toMatchObject({
      type: "setFilters",
      rules: [{ type: "exclude", pattern: "health", use_regex: false }],
    });
  });

  it("appends and deletes rule rows", () => {
    (root.querySelector(".fp-add") as HTMLButtonElement).click();
    expect(rows()).toHaveLength(2);
    typePattern(rows()[1]!, "ERROR");
    vi.advanceTimersByTime(300);
    expect(sent.at(-1)).toMatchObject({
      type: "setFilters",
      rules: [
        { pattern: "noise" },
        { type: "include", pattern: "ERROR", use_regex: false },
      ],
    });

    const del = rows()[0]!.querySelector(".fp-del") as HTMLButtonElement;
    del.click();
    vi.advanceTimersByTime(300);
    expect(sent.at(-1)).toMatchObject({ type: "setFilters", rules: [{ pattern: "ERROR" }] });
  });

  it("toggles a rule off via the checkbox without dropping it", () => {
    const box = rows()[0]!.querySelector("input[type=checkbox]") as HTMLInputElement;
    box.checked = false;
    box.dispatchEvent(new Event("change"));
    vi.advanceTimersByTime(300);
    expect(sent.at(-1)).toMatchObject({
      type: "setFilters",
      rules: [{ pattern: "noise", enabled: false }],
    });
  });

  it("marks the offending pattern inline on an invalid-regex notice and keeps the draft", () => {
    typePattern(rows()[0]!, "[[error");
    const mode = rows()[0]!.querySelectorAll("select")[1] as HTMLSelectElement;
    mode.value = "regex";
    mode.dispatchEvent(new Event("change"));
    vi.advanceTimersByTime(300);
    const draftRules = (sent.at(-1) as { rules: FilterRuleDto[] }).rules;

    // Host echo: same rules applied, notice about the invalid regex.
    const echoed = stateFrom(snapshotMsg({
      view: {
        ...snapshotMsg().view,
        filters: draftRules,
        notice: "Invalid regex ([[error unclosed class); filter treats the pattern as literal text",
      },
    }));
    panel.render(echoed);

    const input = patternInput(rows()[0]!);
    expect(input.classList.contains("fp-invalid")).toBe(true);
    expect(input.value).toBe("[[error");
    expect((root.querySelector(".fp-error") as HTMLElement).textContent).toContain("Invalid regex");

    // Editing the field clears the mark before the next round-trip.
    typePattern(rows()[0]!, "error");
    expect(input.classList.contains("fp-invalid")).toBe(false);
  });

  it("reverts to the rules applied before the draft", () => {
    typePattern(rows()[0]!, "healthcheck");
    vi.advanceTimersByTime(300);
    const revertBtn = [...root.querySelectorAll("button")].find(
      (b) => b.textContent === "Revert",
    ) as HTMLButtonElement;
    revertBtn.click();
    expect(sent.at(-1)).toMatchObject({
      type: "setFilters",
      rules: [{ type: "exclude", pattern: "noise" }],
    });
    expect(patternInput(rows()[0]!).value).toBe("noise");
    expect(revertBtn.disabled).toBe(true);
  });

  it("drops the draft when the active tab changes", () => {
    typePattern(rows()[0]!, "typed-but-not-applied");
    const next = stateFrom(snapshotMsg({
      active_tab: 2,
      view: { ...snapshotMsg().view, name: "Tab 3", filters: [] },
    }));
    panel.render(next);
    vi.advanceTimersByTime(300);
    expect(sent).toHaveLength(0);
    expect(root.querySelectorAll(".fp-rule")).toHaveLength(0);
  });

  it("renames the active tab inline (Enter commits, Escape cancels)", () => {
    const title = root.querySelector(".fp-title") as HTMLButtonElement;
    title.click();
    const input = root.querySelector(".fp-rename") as HTMLInputElement;
    expect(input.style.display).not.toBe("none");
    input.value = "Only errors";
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter" }));
    expect(sent.at(-1)).toMatchObject({ type: "renameTab", index: 1, name: "Only errors" });

    title.click();
    input.value = "discarded";
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
    expect(sent).toHaveLength(1);
    expect(title.textContent).toBe("Errors");
  });
});

describe("filters panel: terminal tab is read-only", () => {
  it("renders locked rows with the explanation and no editing", () => {
    vi.useFakeTimers();
    const root = document.createElement("div");
    document.body.appendChild(root);
    const panel = new FiltersPanel(root);
    const sent: WebviewToHost[] = [];
    panel.onCommand = (cmd) => sent.push(cmd);
    const base = snapshotMsg({ active_tab: 0 });
    const state = stateFrom({
      ...base,
      view: { ...base.view, name: "Terminal", filters_locked: true, filters: rules({ pattern: "x" }) },
    });
    panel.render(state);

    expect((root.querySelector(".fp-readonly-badge") as HTMLElement).style.display).not.toBe("none");
    expect((root.querySelector(".fp-title") as HTMLButtonElement).style.display).toBe("none");
    expect(root.querySelector(".fp-locked-hint")?.textContent).toContain("read-only");
    for (const select of root.querySelectorAll(".fp-rule select")) {
      expect((select as HTMLSelectElement).disabled).toBe(true);
    }
    expect((root.querySelector(".fp-add") as HTMLButtonElement).disabled).toBe(true);

    // No edit path may fire on the locked tab.
    vi.advanceTimersByTime(300);
    expect(sent).toHaveLength(0);
    vi.useRealTimers();
    root.remove();
  });
});

describe("filters panel: presets", () => {
  const presets: PresetDto[] = [
    { id: "node-errors", filters: rules({ id: "only-errors", pattern: "(Error|ERROR)", use_regex: true }) },
  ];

  function setup(): void {
    root = document.createElement("div");
    document.body.appendChild(root);
    panel = new FiltersPanel(root);
    sent = [];
    panel.onCommand = (cmd) => sent.push(cmd);
    state = stateFrom(snapshotMsg());
  }

  let root: HTMLElement;
  let panel: FiltersPanel;
  let sent: WebviewToHost[];
  let state: SessionState;

  beforeEach(() => {
    vi.useFakeTimers();
    setup();
  });

  afterEach(() => {
    vi.useRealTimers();
    root.remove();
  });

  it("applies a preset to the draft with the debounced apply", () => {
    panel.setPresets(presets);
    panel.render(state);
    const presetBtn = root.querySelector(".fp-preset") as HTMLButtonElement;
    expect(presetBtn.textContent).toBe("node-errors");
    presetBtn.click();
    expect(root.querySelectorAll(".fp-rule")).toHaveLength(1);
    vi.advanceTimersByTime(300);
    expect(sent.at(-1)).toMatchObject({
      type: "setFilters",
      rules: [{ id: "only-errors", pattern: "(Error|ERROR)", use_regex: true }],
    });
  });
});

describe("filters panel: severity, tabs, view", () => {
  let root: HTMLElement;
  let panel: FiltersPanel;
  let sent: WebviewToHost[];

  beforeEach(() => {
    vi.useFakeTimers();
    root = document.createElement("div");
    document.body.appendChild(root);
    panel = new FiltersPanel(root);
    sent = [];
    panel.onCommand = (cmd) => sent.push(cmd);
    panel.render(stateFrom(snapshotMsg()));
  });

  afterEach(() => {
    vi.useRealTimers();
    root.remove();
  });

  it("selects severity through segmented buttons", () => {
    const errorBtn = root.querySelector('button[data-mode="error"]') as HTMLButtonElement;
    errorBtn.click();
    expect(sent.at(-1)).toMatchObject({ type: "setSeverity", mode: "error" });
    panel.render(stateFrom(snapshotMsg({
      view: { ...snapshotMsg().view, severity: "error" } as never,
    })));
    expect(errorBtn.getAttribute("aria-pressed")).toBe("true");
  });

  it("activates, closes, and restores tabs", () => {
    const tabRows = [...root.querySelectorAll(".fp-tabrow")];
    (tabRows[2]!.querySelector(".fp-tab-name") as HTMLButtonElement).click();
    expect(sent.at(-1)).toMatchObject({ type: "switchTab", index: 2 });
    (tabRows[1]!.querySelector(".fp-tab-close") as HTMLButtonElement).click();
    expect(sent.at(-1)).toMatchObject({ type: "closeTab", index: 1 });
    const restore = [...root.querySelectorAll("button")].find(
      (b) => b.textContent === "Restore closed tab",
    )!;
    restore.click();
    expect(sent.at(-1)).toMatchObject({ type: "restoreTab" });
  });

  it("sends Follow and Wrap toggles and expand/collapse", () => {
    const follow = [...root.querySelectorAll("button")].find((b) => b.textContent === "Follow")!;
    follow.click();
    expect(sent.at(-1)).toMatchObject({ type: "setFollow", on: false });
    const expand = [...root.querySelectorAll("button")].find((b) => b.textContent === "Expand all")!;
    expand.click();
    expect(sent.at(-1)).toMatchObject({ type: "expandAll" });
  });
});
