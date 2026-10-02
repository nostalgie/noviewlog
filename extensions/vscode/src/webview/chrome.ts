/**
 * Slim DOM chrome above the canvas: filter tabs (Terminal tab is read-only),
 * the Find bar with regex toggles and counter, the + Tab action, and the
 * filter-panel toggle. Severity, Follow/Wrap, and rule editing live in the
 * filter panel (filtersPanel.ts). Styled with VS Code CSS variables; all
 * behavior is sent to the host as typed commands.
 */

import type { WebviewToHost } from "../protocol";
import { searchBadge, type SessionState } from "./state";

export class Chrome {
  private root: HTMLElement;
  private tabsBar: HTMLElement;
  private findInput: HTMLInputElement;
  private findLabel: HTMLElement;
  private exitLabel: HTMLElement;
  private panelToggleBtn: HTMLButtonElement;
  private renderedTabs: unknown;

  state: SessionState = { session: null, exitStatus: null };
  onCommand: ((cmd: WebviewToHost) => void) | null = null;
  onTogglePanel: (() => void) | null = null;

  constructor(root: HTMLElement) {
    this.root = root;
    this.tabsBar = el("div", "tabs");

    this.findInput = document.createElement("input");
    this.findInput.type = "text";
    this.findInput.placeholder = "Find (highlights matches)";
    this.findInput.spellcheck = false;

    const regexBtn = toggleButton(".*", "Regular expression", false, (on) =>
      this.sendSearch({ regex: on }),
    );
    const caseBtn = toggleButton("Aa", "Match case", false, (on) =>
      this.sendSearch({ caseSensitive: on }),
    );
    const wordBtn = toggleButton("|w", "Whole word", false, (on) =>
      this.sendSearch({ wholeWord: on }),
    );

    const prevBtn = button("<", "Previous match (Shift+Enter)", () => this.send({ type: "searchPrev" }));
    const nextBtn = button(">", "Next match (Enter)", () => this.send({ type: "searchNext" }));
    this.findLabel = el("span", "find-label");

    let debounce: number | undefined;
    this.findInput.addEventListener("input", () => {
      window.clearTimeout(debounce);
      debounce = window.setTimeout(() => this.sendSearch({}), 200);
    });
    this.findInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        this.send(e.shiftKey ? { type: "searchPrev" } : { type: "searchNext" });
      }
    });

    const addTabBtn = button("+ Tab", "New filter tab", () => this.send({ type: "addTab" }));
    this.panelToggleBtn = button("Filters", "Toggle the filter panel", () =>
      this.onTogglePanel?.(),
    );

    const find = el("div", "find");
    find.append(this.findInput, regexBtn, caseBtn, wordBtn, prevBtn, nextBtn, this.findLabel);
    this.exitLabel = el("span", "exit-label");
    const controls = el("div", "controls");
    controls.append(find, this.exitLabel, addTabBtn, this.panelToggleBtn);

    this.root.append(this.tabsBar, controls);
  }

  private send(cmd: WebviewToHost): void {
    this.onCommand?.(cmd);
  }

  private sendSearch(
    flags: { regex?: boolean; caseSensitive?: boolean; wholeWord?: boolean },
  ): void {
    const search = this.state.session?.view.search;
    this.send({
      type: "setSearch",
      query: this.findInput.value,
      regex: flags.regex ?? search?.regex ?? false,
      caseSensitive: flags.caseSensitive ?? search?.case_sensitive ?? false,
      wholeWord: flags.wholeWord ?? search?.whole_word ?? false,
    });
  }

  /** Reflect the panel's open state on the chrome toggle. */
  setPanelOpen(open: boolean): void {
    this.panelToggleBtn.setAttribute("aria-pressed", String(open));
  }

  /** Re-render chrome from state (cheap; called per applied message). */
  render(state: SessionState): void {
    this.state = state;
    const session = state.session;
    if (!session) {
      this.tabsBar.replaceChildren();
      this.renderedTabs = null;
      this.exitLabel.style.display = "none";
      return;
    }
    const tabs = session.tabs;
    // Tabs don't change on appends; rebuilding them per message drops focus
    // and wastes work, so skip when unchanged.
    if (tabs !== this.renderedTabs) {
      this.renderedTabs = tabs;
      this.tabsBar.replaceChildren(
        ...tabs.map((tab) => {
          const b = document.createElement("button");
          b.className = `tab${tab.active ? " active" : ""}${tab.terminal ? " terminal" : ""}`;
          b.textContent = tab.name;
          b.title = tab.terminal ? "Terminal output (filters read-only)" : tab.name;
          b.onclick = () => this.send({ type: "switchTab", index: tab.index });
          if (!tab.terminal) {
            const close = el("span", "tab-close");
            close.textContent = "x";
            close.title = "Close tab";
            close.onclick = (e) => {
              e.stopPropagation();
              this.send({ type: "closeTab", index: tab.index });
            };
            b.appendChild(close);
          }
          return b;
        }),
      );
    }

    this.exitLabel.textContent = state.exitStatus ?? "";
    this.exitLabel.style.display = state.exitStatus ? "" : "none";
    this.findLabel.textContent = searchBadge(session.view.search);
    if (document.activeElement !== this.findInput) {
      this.findInput.value = session.view.search.query;
    }
  }
}

function el(tag: string, className: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = className;
  return e;
}

function button(label: string, title: string, onClick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = label;
  b.title = title;
  b.onclick = onClick;
  return b;
}

function toggleButton(
  label: string,
  title: string,
  initial: boolean,
  onChange: (on: boolean) => void,
): HTMLButtonElement {
  const b = button(label, title, () => {
    const on = b.getAttribute("aria-pressed") !== "true";
    b.setAttribute("aria-pressed", String(on));
    onChange(on);
  });
  b.setAttribute("aria-pressed", String(initial));
  return b;
}
