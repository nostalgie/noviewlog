/**
 * Typed host <-> webview message protocol.
 *
 * Host -> webview: full snapshots, incremental appends, process exit.
 * Webview -> host: user commands (filters, search, collapse, follow).
 *
 * The webview is a dumb renderer: it owns only scroll position and DOM
 * input state; every content byte arrives through the messages below.
 * DTO field names mirror the wasm facade's serde structs exactly.
 */

/** One styled text run inside a flat line (Line-SGR segment). */
export interface SegDto {
  text: string;
  /** Truecolor RGB, already resolved from basic ANSI by the parser. */
  fg?: [number, number, number];
  bg?: [number, number, number];
  bold?: boolean;
  dim?: boolean;
  underline?: boolean;
  /** Search hit (a non-current one is just `search`). */
  search?: boolean;
  search_current?: boolean;
  link?: string;
}

export interface LineDto {
  record_id: number;
  line_index: number;
  /** Plain text; the webview soft-wraps on this. */
  raw: string;
  segments: SegDto[];
  level: string | null;
  collapsible: boolean;
  collapsed: boolean;
  hidden_line_count: number;
}

export interface SearchDto {
  query: string;
  regex: boolean;
  case_sensitive: boolean;
  whole_word: boolean;
  error: string | null;
  /** Desktop counter label ("3/40"), empty when no search is active. */
  label: string;
  match_count: number;
  active_line: number | null;
  /** Monotonic counter; bumped when the webview should scroll to the match. */
  scroll_request: number;
}

export interface FilterRuleDto {
  id: string;
  type: "include" | "exclude";
  pattern: string;
  use_regex: boolean;
  name?: string;
  enabled?: boolean;
}

export interface TabInfoDto {
  index: number;
  name: string;
  active: boolean;
  /** Index 0 is the Terminal tab: read-only filter semantics. */
  terminal: boolean;
}

export interface ViewDto {
  name: string;
  severity: string;
  follow: boolean;
  wrap: boolean;
  total_lines: number;
  epoch: number;
  lines: LineDto[];
  search: SearchDto;
  filters_locked: boolean;
  filters: FilterRuleDto[];
  notice: string | null;
}

export interface SessionDto {
  session_id: number;
  name: string;
  source: "pty" | "file";
  finished: boolean;
  active_tab: number;
  tabs: TabInfoDto[];
  view: ViewDto;
  dropped_records: number;
  buffer_records: number;
  buffer_max: number;
  /** Hosted app wants mouse events (forward canvas mouse to the PTY). */
  mouse_tracking: boolean;
  /** Hosted app wants bracketed paste. */
  bracketed_paste: boolean;
}

export interface SessionSnapshotMsg extends SessionDto {
  type: "snapshot";
  exit_status?: string;
}

/** One bundled preset from the shared `presets/defaults.yaml`. */
export interface PresetDto {
  id: string;
  filters: FilterRuleDto[];
}

/** Sent once per webview attach, before/with the first snapshot. */
export interface PresetsMsg {
  type: "presets";
  presets: PresetDto[];
}

/** Incremental delta; on `ok === false` the host sends a full snapshot. */
export interface SessionAppendMsg {
  type: "append";
  ok: boolean;
  epoch: number;
  base: number;
  total_lines: number;
  lines: LineDto[];
  search: SearchDto;
  follow: boolean;
  mouse_tracking: boolean;
  bracketed_paste: boolean;
  dropped_records: number;
  buffer_records: number;
}

export interface ExitMsg {
  type: "exit";
  status: string;
}

export type HostToWebview = SessionSnapshotMsg | SessionAppendMsg | PresetsMsg | ExitMsg;

const isFiniteNumber = (v: unknown): v is number =>
  typeof v === "number" && Number.isFinite(v);

/** Webviews receive postMessage from VS Code internals too — validate shape
 * before feeding a message into the state pipeline. */
export function isHostToWebview(value: unknown): value is HostToWebview {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const msg = value as { type?: unknown };
  switch (msg.type) {
    case "snapshot":
      return isFiniteNumber((value as SessionDto).session_id);
    case "append":
      return (
        typeof (value as SessionAppendMsg).ok === "boolean" &&
        isFiniteNumber((value as SessionAppendMsg).epoch) &&
        Array.isArray((value as SessionAppendMsg).lines)
      );
    case "presets":
      return Array.isArray((value as PresetsMsg).presets);
    case "exit":
      return typeof (value as ExitMsg).status === "string";
    default:
      return false;
  }
}

export type SeverityMode = "all" | "error" | "warn" | "info" | "debug" | "unleveled";

export interface CommandSetFilters {
  type: "setFilters";
  rules: FilterRuleDto[];
}

export interface CommandSetSeverity {
  type: "setSeverity";
  mode: SeverityMode;
}

export interface CommandSetSearch {
  type: "setSearch";
  query: string;
  regex: boolean;
  caseSensitive: boolean;
  wholeWord: boolean;
}

export interface CommandSearchNav {
  type: "searchNext" | "searchPrev";
}

export interface CommandToggleCollapse {
  type: "toggleCollapse";
  recordId: number;
}

export interface CommandExpandAll {
  type: "expandAll" | "collapseAll";
}

export interface CommandSetFollow {
  type: "setFollow";
  on: boolean;
}

export interface CommandSetWrap {
  type: "setWrap";
  on: boolean;
}

export interface CommandSwitchTab {
  type: "switchTab";
  index: number;
}

export interface CommandCloseTab {
  type: "closeTab";
  index: number;
}

export interface CommandAddTab {
  type: "addTab";
}

export interface CommandRestoreTab {
  type: "restoreTab";
}

export interface CommandRenameTab {
  type: "renameTab";
  index: number;
  name: string;
}

/** Splitter drag end: persist the filter panel width per workspace. */
export interface CommandSavePanelWidth {
  type: "savePanelWidth";
  width: number;
}

/** Raw keystrokes for the PTY (terminal tab focused only). */
export interface CommandInput {
  type: "input";
  data: string;
}

export interface CommandResize {
  type: "resize";
  cols: number;
  rows: number;
}

export interface CommandReady {
  type: "ready";
}

export type WebviewToHost =
  | CommandReady
  | CommandResize
  | CommandAddTab
  | CommandSetFilters
  | CommandSetSeverity
  | CommandSetSearch
  | CommandSearchNav
  | CommandToggleCollapse
  | CommandExpandAll
  | CommandSetFollow
  | CommandSetWrap
  | CommandSwitchTab
  | CommandCloseTab
  | CommandRestoreTab
  | CommandRenameTab
  | CommandSavePanelWidth
  | CommandInput;

export function isWebviewToHost(value: unknown): value is WebviewToHost {
  if (typeof value !== "object" || value === null) {
    return false;
  }
  const type = (value as { type?: unknown }).type;
  return (
    typeof type === "string" &&
    [
      "ready",
      "resize",
      "addTab",
      "setFilters",
      "setSeverity",
      "setSearch",
      "searchNext",
      "searchPrev",
      "toggleCollapse",
      "expandAll",
      "collapseAll",
      "setFollow",
      "setWrap",
      "switchTab",
      "closeTab",
      "restoreTab",
      "renameTab",
      "savePanelWidth",
      "input",
    ].includes(type)
  );
}
