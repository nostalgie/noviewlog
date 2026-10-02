/**
 * Right-hand filter panel: tab header with inline rename (Terminal tab shows
 * a read-only badge), structured FILTERS rule rows with a 300 ms debounced
 * live apply and inline invalid-regex marking, one-click bundled presets,
 * SEVERITY segmented buttons, the TABS list (activate/close/restore), and
 * VIEW toggles (Follow/Wrap/Expand). A pure UI layer over the existing
 * protocol commands: state derives from the session snapshot the webview
 * already receives; only edits travel to the host.
 */

import type { FilterRuleDto, PresetDto, SeverityMode, WebviewToHost } from "../protocol";
import type { SessionState } from "./state";

const APPLY_DEBOUNCE_MS = 300;
const SEVERITY_MODES: SeverityMode[] = ["all", "error", "warn", "info", "debug", "unleveled"];

let ruleIdCounter = 0;

export class FiltersPanel {
  state: SessionState = { session: null, exitStatus: null };
  onCommand: ((cmd: WebviewToHost) => void) | null = null;
  /** Panel close button (layout collapse is owned by main.ts). */
  onClose: (() => void) | null = null;

  private head: HTMLElement;
  private titleBtn: HTMLButtonElement;
  private renameInput: HTMLInputElement;
  private readonlyBadge: HTMLElement;
  private rulesBox: HTMLElement;
  private errorLine: HTMLElement;
  private addRuleBtn: HTMLButtonElement;
  private lockedHint: HTMLElement;
  private presetsBox: HTMLElement;
  private severityBox: HTMLElement;
  private tabsBox: HTMLElement;
  private restoreBtn: HTMLButtonElement;
  private followBtn: HTMLButtonElement;
  private wrapBtn: HTMLButtonElement;
  private revertBtn: HTMLButtonElement;

  private presets: PresetDto[] = [];
  private renderedPresets: unknown;
  private presetsRenderedLocked = true;
  /** Host-applied rules for the active tab (value-tracking). */
  private appliedJson: string | null = null;
  /** What the rows DOM currently shows (draft or applied). */
  private shownRules: FilterRuleDto[] = [];
  /** User edits not yet confirmed by the host; null = no draft. */
  private draft: FilterRuleDto[] | null = null;
  /** Applied rules when the draft started — the Revert target. */
  private baseline: FilterRuleDto[] | null = null;
  private lastSentJson: string | null = null;
  private applyTimer: number | undefined;
  private renderedTabKey: string | null = null;
  private renaming = false;

  constructor(root: HTMLElement) {
    this.head = el("div", "fp-head");
    this.titleBtn = document.createElement("button");
    this.titleBtn.className = "fp-title";
    this.titleBtn.title = "Rename tab";
    this.titleBtn.onclick = () => this.startRename();
    this.renameInput = document.createElement("input");
    this.renameInput.type = "text";
    this.renameInput.className = "fp-rename";
    this.renameInput.style.display = "none";
    this.renameInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        this.commitRename();
      } else if (e.key === "Escape") {
        this.endRename(false);
      }
    });
    this.renameInput.addEventListener("blur", () => {
      if (this.renaming) {
        this.commitRename();
      }
    });
    this.readonlyBadge = el("span", "fp-readonly-badge");
    this.readonlyBadge.textContent = "read-only";
    this.readonlyBadge.style.display = "none";
    const closeBtn = button("x", "Close filter panel", () => this.onClose?.());
    closeBtn.className = "fp-close";
    this.head.append(this.titleBtn, this.renameInput, this.readonlyBadge, closeBtn);

    const filters = el("div", "fp-section");
    const filtersTitle = el("p", "fp-section-title");
    filtersTitle.textContent = "Filters";
    this.lockedHint = el("div", "fp-locked-hint");
    this.rulesBox = el("div", "fp-rules");
    this.errorLine = el("div", "fp-error");
    this.errorLine.style.display = "none";
    this.addRuleBtn = button("+ Rule", "Append a filter rule", () => this.addRule());
    this.addRuleBtn.className = "fp-add";
    this.revertBtn = button("Revert", "Restore the tab's last applied rules", () =>
      this.revert(),
    );
    filters.append(filtersTitle, this.lockedHint, this.rulesBox, this.errorLine, this.addRuleBtn, this.revertBtn);

    const presetsTitle = el("p", "fp-section-title");
    presetsTitle.textContent = "Presets";
    this.presetsBox = el("div", "fp-presets");
    const presets = el("div", "fp-section");
    presets.append(presetsTitle, this.presetsBox);

    const severityTitle = el("p", "fp-section-title");
    severityTitle.textContent = "Severity";
    this.severityBox = el("div", "fp-seg");
    for (const mode of SEVERITY_MODES) {
      const b = button(mode, `Severity: ${mode}`, () =>
        this.send({ type: "setSeverity", mode }),
      );
      b.dataset.mode = mode;
      this.severityBox.appendChild(b);
    }
    const severity = el("div", "fp-section");
    severity.append(severityTitle, this.severityBox);

    const tabsTitle = el("p", "fp-section-title");
    tabsTitle.textContent = "Tabs";
    this.tabsBox = el("div", "fp-tabs");
    this.restoreBtn = button("Restore closed tab", "Reopen the last closed tab", () =>
      this.send({ type: "restoreTab" }),
    );
    const tabs = el("div", "fp-section");
    tabs.append(tabsTitle, this.tabsBox, this.restoreBtn);

    const viewTitle = el("p", "fp-section-title");
    viewTitle.textContent = "View";
    this.followBtn = toggleButton("Follow", "Follow output", true, (on) =>
      this.send({ type: "setFollow", on }),
    );
    this.wrapBtn = toggleButton("Wrap", "Wrap lines", false, (on) =>
      this.send({ type: "setWrap", on }),
    );
    const expandBtn = button("Expand all", "Expand all collapsed records", () =>
      this.send({ type: "expandAll" }),
    );
    const collapseBtn = button("Collapse all", "Collapse all multiline records", () =>
      this.send({ type: "collapseAll" }),
    );
    const view = el("div", "fp-section");
    const viewRow = el("div", "fp-seg");
    viewRow.append(this.followBtn, this.wrapBtn, expandBtn, collapseBtn);
    view.append(viewTitle, viewRow);

    root.append(this.head, filters, presets, severity, tabs, view);
  }

  /** Presets message from the host (once per webview attach). */
  setPresets(presets: PresetDto[]): void {
    this.presets = presets;
    this.renderedPresets = null;
    this.renderPresets();
  }  render(state: SessionState): void {
    this.state = state;
    const session = state.session;
    if (!session) {
      this.renderEmpty();
      return;
    }
    const view = session.view;
    const tabKey = `${session.session_id}:${session.active_tab}`;
    if (tabKey !== this.renderedTabKey) {
      // Desktop parity: switching tabs (or sessions) drops the draft.
      this.renderedTabKey = tabKey;
      this.dropDraft();
      this.clearError();
      this.endRename(false);
    }

    this.renderHeader(session);
    this.renderLocked(view.filters_locked);

    const appliedJson = JSON.stringify(view.filters);
    if (appliedJson !== this.appliedJson) {
      const isEcho = appliedJson === this.lastSentJson;
      this.appliedJson = appliedJson;
      this.lastSentJson = null;
      if (isEcho) {
        // Our own debounced apply came back; keep the DOM (and any draft
        // text) so the user can keep typing. A clean echo clears marks.
        if (!view.notice) {
          this.clearError();
        }
      } else {
        // Host-initiated change (restore, persistence, new session).
        this.dropDraft();
        this.clearError();
        this.renderRules(view.filters, view.filters_locked);
      }
    }

    this.markNotice(view.notice);
    this.renderPresets();
    this.renderSeverity(view.severity);
    this.renderTabs(session);
    this.followBtn.setAttribute("aria-pressed", String(view.follow));
    this.wrapBtn.setAttribute("aria-pressed", String(view.wrap));
    this.revertBtn.disabled = view.filters_locked || this.draft === null;
  }

  // ----- tab header -----

  private renderHeader(session: NonNullable<SessionState["session"]>): void {
    const tab = session.tabs[session.active_tab];
    if (!tab) {
      return;
    }
    if (!this.renaming) {
      this.titleBtn.textContent = tab.name;
    }
    const terminal = tab.terminal === true;
    this.titleBtn.style.display = terminal ? "none" : "";
    this.readonlyBadge.style.display = terminal ? "" : "none";
  }

  private startRename(): void {
    const session = this.state.session;
    const tab = session?.tabs[session.active_tab];
    if (!session || !tab || tab.terminal) {
      return;
    }
    this.renaming = true;
    this.renameInput.value = tab.name;
    this.titleBtn.style.display = "none";
    this.renameInput.style.display = "";
    this.renameInput.focus();
    this.renameInput.select();
  }

  private commitRename(): void {
    const session = this.state.session;
    const tab = session?.tabs[session.active_tab];
    const name = this.renameInput.value.trim();
    this.endRename(false);
    if (session && tab && name.length > 0 && name !== tab.name) {
      this.titleBtn.textContent = name; // optimistic; the snapshot confirms
      this.send({ type: "renameTab", index: session.active_tab, name });
    }
  }

  private endRename(applyText: boolean): void {
    if (!this.renaming) {
      return;
    }
    this.renaming = false;
    this.renameInput.style.display = "none";
    this.titleBtn.style.display = "";
    if (!applyText) {
      const session = this.state.session;
      const tab = session?.tabs[session.active_tab];
      this.titleBtn.textContent = tab?.name ?? "";
    }
  }

  // ----- filters -----

  private renderLocked(locked: boolean): void {
    this.lockedHint.style.display = locked ? "" : "none";
    if (locked) {
      this.lockedHint.textContent =
        "Terminal tab: raw output — filters are read-only. Use + Tab to filter.";
    }
    this.addRuleBtn.disabled = locked;
    this.revertBtn.disabled = locked || this.draft === null;
    if (locked && this.draft) {
      this.dropDraft();
    }
  }

  private currentRules(): FilterRuleDto[] {
    if (this.draft) {
      return this.draft;
    }
    return this.state.session?.view.filters ?? [];
  }

  private renderRules(rules: FilterRuleDto[], locked: boolean): void {
    this.shownRules = rules;
    const rows: Node[] = [];
    rules.forEach((rule, index) => {
      rows.push(this.buildRuleRow(rule, index, locked));
    });
    if (rules.length === 0 && !locked) {
      const empty = el("div", "fp-empty");
      empty.textContent = "No rules — all lines match.";
      rows.push(empty);
    }
    this.rulesBox.replaceChildren(...rows);
  }

  private buildRuleRow(rule: FilterRuleDto, index: number, locked: boolean): HTMLElement {
    const row = el("div", "fp-rule");

    const typeSelect = document.createElement("select");
    for (const kind of ["include", "exclude"] as const) {
      const opt = document.createElement("option");
      opt.value = kind;
      opt.textContent = kind;
      typeSelect.appendChild(opt);
    }
    typeSelect.value = rule.type;
    typeSelect.disabled = locked;
    typeSelect.setAttribute("aria-label", `Rule ${index + 1} type`);
    typeSelect.onchange = () =>
      this.editRule(index, (r) => {
        r.type = typeSelect.value as "include" | "exclude";
      });

    const modeSelect = document.createElement("select");
    for (const mode of ["literal", "regex"] as const) {
      const opt = document.createElement("option");
      opt.value = mode;
      opt.textContent = mode;
      modeSelect.appendChild(opt);
    }
    modeSelect.value = rule.use_regex ? "regex" : "literal";
    modeSelect.disabled = locked;
    modeSelect.setAttribute("aria-label", `Rule ${index + 1} match mode`);
    modeSelect.onchange = () =>
      this.editRule(index, (r) => {
        r.use_regex = modeSelect.value === "regex";
        const pattern = this.patternInput(index, row);
        if (pattern) {
          this.unmarkRow(pattern);
        }
      });

    const pattern = document.createElement("input");
    pattern.type = "text";
    pattern.value = rule.pattern;
    pattern.placeholder = "pattern";
    pattern.disabled = locked;
    pattern.spellcheck = false;
    pattern.setAttribute("aria-label", `Rule ${index + 1} pattern`);
    pattern.dataset.ruleIndex = String(index);
    pattern.oninput = () => {
      this.editRule(index, (r) => {
        r.pattern = pattern.value;
      });
      this.unmarkRow(pattern);
    };

    const enabled = document.createElement("input");
    enabled.type = "checkbox";
    enabled.checked = rule.enabled !== false;
    enabled.disabled = locked;
    enabled.title = enabled.checked ? "Enabled" : "Disabled";
    enabled.setAttribute("aria-label", `Rule ${index + 1} enabled`);
    enabled.onchange = () =>
      this.editRule(index, (r) => {
        r.enabled = enabled.checked;
      });

    row.append(typeSelect, modeSelect, pattern, enabled);
    if (!locked) {
      const del = button("x", "Delete rule", () => this.deleteRule(index));
      del.className = "fp-del";
      row.appendChild(del);
    }
    return row;
  }

  /** The pattern input of row `index`, scoped to the row when available. */
  private patternInput(index: number, row?: HTMLElement): HTMLInputElement | null {
    const scope = row ?? this.rulesBox;
    for (const input of scope.querySelectorAll<HTMLInputElement>("input[data-rule-index]")) {
      if (input.dataset.ruleIndex === String(index)) {
        return input;
      }
    }
    return null;
  }

  private ensureDraft(): FilterRuleDto[] {
    if (!this.draft) {
      this.draft = cloneRules(this.currentRules());
      if (!this.baseline) {
        this.baseline = cloneRules(this.currentRules());
      }
    }
    return this.draft;
  }

  private editRule(index: number, mutate: (rule: FilterRuleDto) => void): void {
    const draft = this.ensureDraft();
    const rule = draft[index];
    if (!rule) {
      return;
    }
    mutate(rule);
    this.scheduleApply();
    this.revertBtn.disabled = false;
  }

  private addRule(): void {
    if (this.state.session?.view.filters_locked) {
      return;
    }
    const draft = this.ensureDraft();
    draft.push({
      id: `panel-rule-${ruleIdCounter++}`,
      type: "include",
      pattern: "",
      use_regex: false,
      enabled: true,
    });
    this.scheduleApply();
    this.revertBtn.disabled = false;
    this.renderRules(draft, false);
    const added = this.patternInput(draft.length - 1);
    added?.focus();
  }

  private deleteRule(index: number): void {
    const draft = this.ensureDraft();
    if (index >= draft.length) {
      return;
    }
    draft.splice(index, 1);
    this.scheduleApply();
    this.renderRules(draft, false);
  }

  private revert(): void {
    const view = this.state.session?.view;
    if (!view || view.filters_locked) {
      return;
    }
    const rules = cloneRules(this.baseline ?? view.filters);
    this.dropDraft();
    this.clearError();
    this.lastSentJson = JSON.stringify(rules);
    this.send({ type: "setFilters", rules });
    this.renderRules(rules, false);
    this.revertBtn.disabled = true;
  }

  private scheduleApply(): void {
    window.clearTimeout(this.applyTimer);
    this.applyTimer = window.setTimeout(() => {
      this.applyTimer = undefined;
      if (this.draft) {
        this.lastSentJson = JSON.stringify(this.draft);
        this.send({ type: "setFilters", rules: cloneRules(this.draft) });
      }
    }, APPLY_DEBOUNCE_MS);
  }

  private dropDraft(): void {
    window.clearTimeout(this.applyTimer);
    this.applyTimer = undefined;
    this.draft = null;
    this.baseline = null;
    this.lastSentJson = null;
  }

  // ----- inline invalid-regex marking -----

  /** Mark the applied rule named in the host notice; keep the draft text. */
  private markNotice(notice: string | null): void {
    if (!notice) {
      return;
    }
    const offender = (this.state.session?.view.filters ?? []).find(
      (rule) => rule.use_regex && rule.pattern.length > 0 && notice.includes(rule.pattern),
    );
    if (!offender) {
      return;
    }
    for (const input of this.rulesBox.querySelectorAll<HTMLInputElement>("input[data-rule-index]")) {
      if (input.value === offender.pattern) {
        input.classList.add("fp-invalid");
        input.title = notice;
      }
    }
    this.errorLine.textContent = notice;
    this.errorLine.style.display = "";
  }

  private unmarkRow(input: HTMLInputElement): void {
    input.classList.remove("fp-invalid");
    input.title = "";
    if (!this.rulesBox.querySelector("input.fp-invalid")) {
      this.clearError();
    }
  }

  private clearError(): void {
    this.errorLine.textContent = "";
    this.errorLine.style.display = "none";
    for (const input of this.rulesBox.querySelectorAll<HTMLInputElement>("input.fp-invalid")) {
      input.classList.remove("fp-invalid");
      input.title = "";
    }
  }

  // ----- presets -----

  private renderPresets(): void {
    const locked = this.state.session?.view.filters_locked ?? true;
    if (this.renderedPresets === this.presets && this.presetsRenderedLocked === locked) {
      return;
    }
    this.renderedPresets = this.presets;
    this.presetsRenderedLocked = locked;
    if (this.presets.length === 0) {
      this.presetsBox.replaceChildren();
      return;
    }
    this.presetsBox.replaceChildren(
      ...this.presets.map((preset) => {
        const b = button(preset.id, `Apply preset '${preset.id}'`, () => this.applyPreset(preset));
        b.className = "fp-preset";
        b.disabled = locked;
        return b;
      }),
    );
  }

  private applyPreset(preset: PresetDto): void {
    const view = this.state.session?.view;
    if (!view || view.filters_locked) {
      return;
    }
    const rules = cloneRules(preset.filters);
    const draft = this.ensureDraft();
    draft.splice(0, draft.length, ...rules);
    this.renderRules(draft, false);
    this.clearError();
    this.revertBtn.disabled = false;
    this.scheduleApply();
  }

  // ----- severity / tabs / view -----

  private renderSeverity(active: string): void {
    for (const b of this.severityBox.querySelectorAll<HTMLButtonElement>("button[data-mode]")) {
      b.setAttribute("aria-pressed", String(b.dataset.mode === active));
    }
  }

  private renderTabs(session: NonNullable<SessionState["session"]>): void {
    this.tabsBox.replaceChildren(
      ...session.tabs.map((tab) => {
        const row = el("div", `fp-tabrow${tab.active ? " active" : ""}`);
        const name = button(tab.name, `Switch to ${tab.name}`, () =>
          this.send({ type: "switchTab", index: tab.index }),
        );
        name.className = "fp-tab-name";
        row.appendChild(name);
        if (!tab.terminal) {
          const close = button("x", `Close ${tab.name}`, () =>
            this.send({ type: "closeTab", index: tab.index }),
          );
          close.className = "fp-tab-close";
          row.appendChild(close);
        }
        return row;
      }),
    );
  }

  private renderEmpty(): void {
    this.dropDraft();
    this.clearError();
    this.renderedTabKey = null;
    this.appliedJson = null;
    this.titleBtn.textContent = "";
    this.readonlyBadge.style.display = "none";
    this.rulesBox.replaceChildren();
    this.presetsBox.replaceChildren();
    this.tabsBox.replaceChildren();
    for (const b of this.severityBox.querySelectorAll<HTMLButtonElement>("button[data-mode]")) {
      b.setAttribute("aria-pressed", "false");
    }
    const empty = el("div", "fp-empty");
    empty.textContent = "No active session.";
    this.rulesBox.appendChild(empty);
  }

  private send(cmd: WebviewToHost): void {
    this.onCommand?.(cmd);
  }
}

function cloneRules(rules: FilterRuleDto[]): FilterRuleDto[] {
  return rules.map((r) => ({ ...r }));
}

function el(tag: string, className: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = className;
  return e;
}

function button(label: string, titleText: string, onClick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = label;
  b.title = titleText;
  b.onclick = onClick;
  return b;
}

function toggleButton(
  label: string,
  titleText: string,
  initial: boolean,
  onChange: (on: boolean) => void,
): HTMLButtonElement {
  const b = button(label, titleText, () => {
    const on = b.getAttribute("aria-pressed") !== "true";
    b.setAttribute("aria-pressed", String(on));
    onChange(on);
  });
  b.setAttribute("aria-pressed", String(initial));
  return b;
}
