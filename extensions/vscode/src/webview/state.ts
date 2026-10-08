/**
 * Webview content state: applies host messages to the rendered model.
 * Pure and testable; the DOM/canvas layer observes it.
 */

import type {
  ExitMsg,
  HostToWebview,
  LineDto,
  SearchDto,
  SessionAppendMsg,
  SessionSnapshotMsg,
  SessionDto,
  ViewDto,
} from "../protocol";

export interface SessionState {
  session: SessionDto | null;
  exitStatus: string | null;
}

export function emptyState(): SessionState {
  return { session: null, exitStatus: null };
}

export function applyMessage(state: SessionState, msg: HostToWebview): SessionState {
  switch (msg.type) {
    case "snapshot":
      return applySnapshot(state, msg);
    case "append":
      return applyAppend(state, msg);
    case "presets":
      // Panel data, not content state: filtersPanel consumes it directly.
      return state;
    case "exit":
      return applyExit(state, msg);
  }
}

export function applySnapshot(state: SessionState, msg: SessionSnapshotMsg): SessionState {
  const { type: _type, exit_status, ...session } = msg;
  // A new session starts with a clean exit status; a snapshot without
  // exit_status keeps the status only for the *same* session (reveals).
  const sameSession = session.session_id === state.session?.session_id;
  return { session, exitStatus: exit_status ?? (sameSession ? state.exitStatus : null) };
}

export function applyAppend(state: SessionState, msg: SessionAppendMsg): SessionState {
  const session = state.session;
  const view = session?.view;
  if (
    !msg.ok ||
    !session ||
    !view ||
    msg.epoch !== view.epoch ||
    msg.base > view.total_lines ||
    msg.base > view.lines.length
  ) {
    // Stale or invalid delta — the host will follow up with a full snapshot.
    return state;
  }
  // Truncate anything past base (live overlay is replaced), then extend.
  // A loop, not a spread: one append can carry tens of thousands of lines
  // and spreading them into `push` is a stack-overflow hazard (issue #322).
  const lines: LineDto[] = view.lines.slice(0, msg.base);
  for (const l of msg.lines) {
    lines.push(l);
  }
  const nextView: ViewDto = {
    ...view,
    total_lines: msg.total_lines,
    lines,
    search: msg.search,
    follow: msg.follow,
  };
  return {
    session: {
      ...session,
      view: nextView,
      dropped_records: msg.dropped_records,
      buffer_records: msg.buffer_records,
      mouse_tracking: msg.mouse_tracking,
      bracketed_paste: msg.bracketed_paste,
    },
    exitStatus: state.exitStatus,
  };
}

export function applyExit(state: SessionState, msg: ExitMsg): SessionState {
  const session = state.session;
  return {
    session: session ? { ...session, finished: true } : null,
    exitStatus: msg.status,
  };
}

/** Search summary the chrome shows (counter label, error). */
export function searchBadge(search: SearchDto): string {
  if (search.error) {
    return search.error;
  }
  return search.query ? search.label : "";
}
