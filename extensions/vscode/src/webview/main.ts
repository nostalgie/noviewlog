/**
 * Webview entry: builds chrome + canvas, applies host messages to state,
 * renders, and forwards user commands. Reveals re-initialize this whole
 * file (hidden webviews drop their DOM); a full snapshot rebuilds view.
 */

import { Chrome } from "./chrome";
import { FiltersPanel } from "./filtersPanel";
import type { HostToWebview } from "../protocol";
import { chunkInputData, isHostToWebview } from "../protocol";
import { measureTheme, Renderer } from "./renderer";
import { applyMessage, emptyState, type SessionState } from "./state";

declare global {
  interface Window {
    acquireVsCodeApi?: () => { postMessage: (msg: unknown) => void };
  }
}

const vscodeApi = window.acquireVsCodeApi?.() ?? null;

const chromeRoot = document.getElementById("chrome")!;
const scroller = document.getElementById("scroller")!;
const canvas = document.getElementById("canvas") as HTMLCanvasElement;
const splitter = document.getElementById("splitter")!;

const style = getComputedStyle(document.body);
const cssVar = (name: string, fallback: string) =>
  style.getPropertyValue(`--vscode-${name}`).trim() || fallback;
const initialTheme = measureTheme(
  cssVar("editor-font-family", "Consolas, 'Courier New', monospace"),
  13,
  1.4,
  cssVar("editor-foreground", style.color || "#cccccc"),
  cssVar("editor-background", style.backgroundColor || "#1e1e1e"),
);

const renderer = new Renderer(canvas, scroller, initialTheme, () => window.devicePixelRatio || 1);
const chrome = new Chrome(chromeRoot);
const panel = new FiltersPanel(document.getElementById("side")!);

// Empty state: shown while no session exists, explains how to start one.
const emptyEl = document.createElement("div");
emptyEl.id = "empty-state";
emptyEl.innerHTML = `
  <div class="empty-title">No active session</div>
  <div class="empty-hint">Run a command or open a log file:</div>
  <ul>
    <li>Ctrl+Shift+P → <b>NoViewLog: Run Command…</b></li>
    <li>Ctrl+Shift+P → <b>NoViewLog: Open Log File…</b></li>
  </ul>
  <div class="empty-hint">Tabs filter, Find only highlights. Use <b>+ Tab</b> and the <b>Filters</b> panel to show matching lines only.</div>`;
document.body.appendChild(emptyEl);

/** Canonical webview state; the views only receive it through `render()`. */
let state: SessionState = emptyState();

let lastScrollRequest = 0;
/** Set on user scroll-up; blocks forced follow scrolls until the host
 * echoes follow=false, so in-flight appends cannot yank the view down. */
let followReleased = false;

function postToHost(msg: unknown): void {
  vscodeApi?.postMessage(msg);
}

chrome.onCommand = (cmd) => {
  postToHost(cmd);
};
panel.onCommand = (cmd) => {
  postToHost(cmd);
};

// ----- panel collapse + splitter -----
const MIN_PANEL_W = 200;
const MAX_PANEL_W = 600;
/** Below this the panel overlays instead of shrinking the viewport grid. */
const OVERLAY_MAX_PX = 480;
let panelOpen = true;

const setPanelOpen = (open: boolean): void => {
  panelOpen = open;
  document.body.classList.toggle("panel-closed", !open);
  chrome.setPanelOpen(open);
};
chrome.onTogglePanel = () => setPanelOpen(!panelOpen);
panel.onClose = () => setPanelOpen(false);

const clampPanelWidth = (raw: number): number => {
  const usable = Math.max(MIN_PANEL_W, window.innerWidth - 240);
  return Math.round(Math.max(MIN_PANEL_W, Math.min(MAX_PANEL_W, usable, raw)));
};

splitter.addEventListener("pointerdown", (event: PointerEvent) => {
  if (!panelOpen || window.innerWidth < OVERLAY_MAX_PX) {
    return; // collapsed or overlay mode: nothing to drag
  }
  splitter.setPointerCapture(event.pointerId);
  splitter.classList.add("dragging");
  document.body.style.userSelect = "none";
});
splitter.addEventListener("pointermove", (event: PointerEvent) => {
  if (!splitter.hasPointerCapture(event.pointerId)) {
    return;
  }
  document.body.style.setProperty("--panel-w", `${clampPanelWidth(window.innerWidth - event.clientX)}px`);
});
const endSplitterDrag = (event: PointerEvent): void => {
  if (!splitter.hasPointerCapture(event.pointerId)) {
    return;
  }
  splitter.releasePointerCapture(event.pointerId);
  splitter.classList.remove("dragging");
  document.body.style.userSelect = "";
  const width = clampPanelWidth(window.innerWidth - event.clientX);
  document.body.style.setProperty("--panel-w", `${width}px`);
  postToHost({ type: "savePanelWidth", width });
};
splitter.addEventListener("pointerup", endSplitterDrag);
splitter.addEventListener("pointercancel", endSplitterDrag);

function apply(msg: HostToWebview): void {
  if (msg.type === "presets") {
    panel.setPresets(msg.presets);
    return;
  }
  state = applyMessage(state, msg);
  chrome.render(state);
  panel.render(state);
  emptyEl.style.display = state.session ? "none" : "";
  const view = state.session?.view;
  renderer.setLines(view?.lines ?? [], view?.wrap ?? false);
  if (view) {
    if (view.search.scroll_request !== lastScrollRequest) {
      lastScrollRequest = view.search.scroll_request;
      if (view.search.active_line !== null) {
        renderer.revealLine(view.search.active_line);
      }
    } else if (view.follow && !followReleased) {
      scroller.scrollTop = renderer.totalHeight;
    }
    if (!view.follow) {
      followReleased = false;
    }
  }
  renderer.scheduleDraw();
}

window.addEventListener("message", (event: MessageEvent) => {
  // VS Code delivers messages from other sources too; an unrecognized shape
  // must not reach the state pipeline (it would corrupt `state`).
  if (isHostToWebview(event.data)) {
    apply(event.data);
    syncCaret();
  }
});

// Size changes: recompute the character grid, resize the PTY to match, and
// relay out the canvas. Grid math is in CSS pixels (charWidth was measured
// that way); devicePixelRatio only affects the canvas bitmap.
const reportResize = () => {
  if (scroller.clientWidth === 0 || scroller.clientHeight === 0) {
    return; // hidden or not yet laid out — a degenerate grid harms the PTY
  }
  const cols = Math.max(1, Math.floor(scroller.clientWidth / initialTheme.charWidth));
  const rows = Math.max(1, Math.floor(scroller.clientHeight / initialTheme.lineHeight));
  postToHost({ type: "resize", cols, rows });
  renderer.relayout(state.session?.view.wrap ?? false);
};
let resizeTimer: number | undefined;
const onResize = () => {
  window.clearTimeout(resizeTimer);
  resizeTimer = window.setTimeout(reportResize, 100);
};
window.addEventListener("resize", onResize);
new ResizeObserver(onResize).observe(scroller);

// Scrolling up away from the bottom releases Follow (setFollow(false)).
// Reaching the bottom again does not re-engage it; only the Follow toggle does.
scroller.addEventListener("scroll", () => {
  const follow = state.session?.view.follow ?? false;
  const atBottom = scroller.scrollTop + scroller.clientHeight >= renderer.totalHeight - 4;
  if (follow && !atBottom && !followReleased) {
    followReleased = true;
    postToHost({ type: "setFollow", on: false });
  }
  renderer.scheduleDraw();
});

// ----- keyboard input (terminal tab only) -----
// The canvas has no text field: keystrokes are translated to VT sequences
// and written straight to the PTY, like a real terminal.
const terminalTabActive = (): boolean => {
  const session = state.session;
  if (!session) {
    return false;
  }
  return session.tabs[session.active_tab]?.terminal === true;
};

const sessionModes = (): { mouse: boolean; bracketed: boolean } => {
  const session = state.session;
  return {
    mouse: session?.mouse_tracking ?? false,
    bracketed: session?.bracketed_paste ?? false,
  };
};

// ----- input-ready caret -----
// A blinking block after the live tail shows when the terminal can take a
// command: PTY session alive, Terminal tab active, surface focused, and no
// mouse-tracking app owning the cursor (vim/htop draw their own).
let blinkTimer: number | undefined;
let blinkLit = true;

const syncCaret = (): void => {
  const session = state.session;
  const ready =
    terminalTabActive() &&
    document.activeElement === scroller &&
    session !== null &&
    session.source === "pty" &&
    !session.finished &&
    !sessionModes().mouse;
  scroller.style.cursor = sessionModes().mouse ? "default" : "text";
  renderer.setCursor(ready);
  if (ready && blinkTimer === undefined) {
    blinkTimer = window.setInterval(() => {
      blinkLit = !blinkLit;
      renderer.setCursorLit(blinkLit);
    }, 530);
  } else if (!ready && blinkTimer !== undefined) {
    window.clearInterval(blinkTimer);
    blinkTimer = undefined;
    blinkLit = true;
  }
};

scroller.addEventListener("focus", syncCaret);
scroller.addEventListener("blur", syncCaret);

scroller.tabIndex = 0;
scroller.addEventListener("pointerdown", () => scroller.focus());

const sgrMouse = (button: number, event: MouseEvent, release: boolean): void => {
  const rect = canvas.getBoundingClientRect();
  const col = Math.max(1, Math.floor((event.clientX - rect.left) / initialTheme.charWidth) + 1);
  const row = Math.max(1, Math.floor((event.clientY - rect.top) / initialTheme.lineHeight) + 1);
  const maxCols = Math.floor(rect.width / initialTheme.charWidth);
  const maxRows = Math.floor(rect.height / initialTheme.lineHeight);
  const c = Math.min(col, Math.max(1, maxCols));
  const r = Math.min(row, Math.max(1, maxRows));
  postToHost({ type: "input", data: `\x1b[<${button};${c};${r}${release ? "m" : "M"}` });
};

let selecting = false;

scroller.addEventListener("mousedown", (event: MouseEvent) => {
  if (!terminalTabActive() || event.button !== 0) {
    return;
  }
  if (sessionModes().mouse) {
    sgrMouse(0, event, false);
    event.preventDefault();
    return;
  }
  const rect = canvas.getBoundingClientRect();
  selecting = true;
  const cell = renderer.cellAt(event.clientX - rect.left, event.clientY - rect.top);
  renderer.setSelection(cell, cell);
});

scroller.addEventListener("mousemove", (event: MouseEvent) => {
  if (!terminalTabActive()) {
    return;
  }
  if (sessionModes().mouse) {
    if (event.buttons === 1) {
      sgrMouse(32, event, false);
    }
    return;
  }
  if (!selecting) {
    return;
  }
  const rect = canvas.getBoundingClientRect();
  renderer.setSelection(
    renderer.currentSelectionAnchor(),
    renderer.cellAt(event.clientX - rect.left, event.clientY - rect.top),
  );
});

scroller.addEventListener("mouseup", (event: MouseEvent) => {
  if (!terminalTabActive() || event.button !== 0) {
    return;
  }
  if (sessionModes().mouse) {
    sgrMouse(0, event, true);
    event.preventDefault();
  }
  selecting = false;
});

scroller.addEventListener(
  "wheel",
  (event: WheelEvent) => {
    if (!terminalTabActive() || !sessionModes().mouse) {
      return; // native scrolling owns the wheel otherwise
    }
    event.preventDefault();
    sgrMouse(event.deltaY < 0 ? 64 : 65, event, false);
  },
  { passive: false },
);

scroller.addEventListener("keydown", (event: KeyboardEvent) => {
  if (!terminalTabActive() || event.metaKey || event.altKey) {
    return;
  }
  const key = event.key;
  // Copy beats SIGINT: Ctrl+C copies a selection, ^C only when none.
  const selection = renderer.selectionText();
  if (event.ctrlKey && key.toLowerCase() === "c" && (event.shiftKey || selection)) {
    if (selection) {
      void navigator.clipboard.writeText(selection);
      renderer.clearSelection();
    }
    event.preventDefault();
    return;
  }
  let data: string | null = null;
  if (event.ctrlKey && key.length === 1 && key >= "a" && key <= "z") {
    data = String.fromCharCode(key.charCodeAt(0) - 96); // ^A..^Z
  } else if (key === "Enter") {
    data = "\r";
  } else if (key === "Backspace") {
    data = "\x7f";
  } else if (key === "Escape") {
    data = "\x1b";
    renderer.clearSelection();
  } else if (key === "Tab") {
    data = "\t";
  } else if (key === "ArrowUp") {
    data = "\x1b[A";
  } else if (key === "ArrowDown") {
    data = "\x1b[B";
  } else if (key === "ArrowRight") {
    data = "\x1b[C";
  } else if (key === "ArrowLeft") {
    data = "\x1b[D";
  } else if (key === "Home") {
    data = "\x1b[H";
  } else if (key === "End") {
    data = "\x1b[F";
  } else if (key === "PageUp") {
    data = "\x1b[5~";
  } else if (key === "PageDown") {
    data = "\x1b[6~";
  } else if (key === "Delete") {
    data = "\x1b[3~";
  } else if (key.length === 1 && !event.ctrlKey) {
    data = key;
    renderer.clearSelection();
  }
  if (data !== null) {
    event.preventDefault();
    postToHost({ type: "input", data });
  }
});

scroller.addEventListener("paste", (event: ClipboardEvent) => {
  if (!terminalTabActive()) {
    return;
  }
  const text = event.clipboardData?.getData("text");
  if (text) {
    event.preventDefault();
    // Bracket the whole paste, then chunk so each host message stays within
    // MAX_INPUT_CHARS (issue #330). Sequential writes keep one paste stream.
    const payload = sessionModes().bracketed ? `\x1b[200~${text}\x1b[201~` : text;
    for (const data of chunkInputData(payload)) {
      postToHost({ type: "input", data });
    }
  }
});

// Fresh DOM after every reveal: report the grid, then ask for a snapshot.
reportResize();
postToHost({ type: "ready" });
