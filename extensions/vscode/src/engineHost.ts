/**
 * Session host: owns the wasm engine (one active session in v1), pumps PTY
 * or log-file bytes into it with batching, and publishes snapshots/appends
 * to the visible webview surface.
 *
 * Mirrors the desktop engine's ingest driver: committed rows flow through
 * the parser into the ring on every feed; a partial trailing record is
 * flushed after an idle gap (120 ms, matching the desktop PENDING_IDLE_FLUSH).
 */

import * as nodePath from "node:path";
import * as vscode from "vscode";
import {
  type HostToWebview,
  type PresetsMsg,
  type SessionAppendMsg,
  type SessionSnapshotMsg,
  type WebviewToHost,
} from "./protocol";
import { spawnPty } from "./pty";

// The wasm-bindgen nodejs glue is shipped unbundled in dist/wasm/ so its
// __dirname-relative .wasm read keeps working inside the esbuild bundle.
interface WasmEngine {
  set_max_records: (maxRecords: number) => void;
  session_create: (name: string, source: string) => number;
  session_close: (id: number) => void;
  session_ingest: (id: number, bytes: Uint8Array) => void;
  session_flush_pending: (id: number) => boolean;
  session_finish: (id: number) => void;
  session_resize: (id: number, cols: number, rows: number) => void;
  session_snapshot: (id: number) => object;
  session_append_since: (id: number, epoch: number, base: number) => object;
  set_severity: (id: number, mode: string) => void;
  set_follow: (id: number, on: boolean) => void;
  set_wrap: (id: number, on: boolean) => void;
  toggle_collapse: (id: number, recordId: number) => void;
  expand_all: (id: number) => void;
  collapse_all: (id: number) => void;
  search_set: (
    id: number,
    query: string,
    regex: boolean,
    caseSensitive: boolean,
    wholeWord: boolean,
  ) => void;
  search_next: (id: number) => void;
  search_prev: (id: number) => void;
  filter_set: (id: number, rules: object) => string | undefined;
  builtin_presets: () => object;
  tab_add: (id: number) => void;
  tab_close: (id: number, index: number) => void;
  tab_switch: (id: number, index: number) => void;
  tab_rename: (id: number, index: number, name: string) => void;
  tab_restore: (id: number) => void;
  can_restore_tab: (id: number) => boolean;
  tabs_export: (id: number) => object;
  tabs_import: (id: number, config: object) => void;
}

/** Persisted per workspace: full tab config (names, rules, severity). */
export interface SavedTabs {
  tabs: object[];
  active_tab: number;
}

export interface TabPersistence {
  load: () => SavedTabs | undefined;
  save: (tabs: SavedTabs) => void;
}

/** Desktop parity: idle gap before a partial trailing record is flushed. */
const PENDING_IDLE_FLUSH_MS = 120;
/** Coalesce PTY chunks; flush at 16 ms or when 64 KiB are buffered. */
const BATCH_FLUSH_MS = 16;
const BATCH_FLUSH_BYTES = 64 * 1024;

export interface SessionInfo {
  id: number;
  name: string;
  source: "pty" | "file";
  command?: string;
  filePath?: string;
  cwd?: string;
}

export class EngineHost implements vscode.Disposable {
  private engine: WasmEngine | null = null;
  private maxRecords = 10_000;
  private session: SessionInfo | null = null;
  private pty: {
    resize: (cols: number, rows: number) => void;
    write: (data: string) => void;
    kill: () => void;
  } | null = null;
  private fileWatcher: vscode.Disposable | null = null;
  private fileOffset = 0;

  private batch: Uint8Array[] = [];
  private batchBytes = 0;
  private batchTimer: NodeJS.Timeout | null = null;
  private pendingTimer: NodeJS.Timeout | null = null;

  /** Workspace tab persistence (filters survive reloads/restarts). */
  private persistence: TabPersistence | null = null;

  /** Bumped on every session replacement; stale PTY callbacks compare
   * against it so a killed process's exit/data cannot touch the successor. */
  private ptyGeneration = 0;

  /** Notified whenever a session starts or stops (context keys, UI state). */
  onSessionChanged: (() => void) | null = null;

  /** Last state sent to the webview — append_since keys off these. */
  private sentEpoch = 0;
  private sentTotal = 0;

  private publish_: ((msg: HostToWebview) => void) | null = null;

  setPublisher(publish: (msg: HostToWebview) => void): void {
    this.publish_ = publish;
  }

  /** Wire workspace storage for tab/filter persistence. */
  setPersistence(persistence: TabPersistence): void {
    this.persistence = persistence;
  }

  /** Restore the workspace's saved tabs right after session creation. */
  private restoreTabs(engine: WasmEngine, id: number): void {
    const saved = this.persistence?.load();
    if (!saved) {
      return;
    }
    try {
      engine.tabs_import(id, saved);
    } catch (err) {
      // A stale/incompatible config must not break session startup.
      console.warn("NoViewLog: tab restore skipped:", err);
    }
  }

  /** Persist the current tab configuration for this workspace. */
  private saveTabs(): void {
    if (!this.persistence || this.session === null || !this.engine) {
      return;
    }
    try {
      const saved = this.engine.tabs_export(this.session.id) as SavedTabs;
      this.persistence.save(saved);
    } catch (err) {
      console.warn("NoViewLog: tab save failed:", err);
    }
  }

  get active(): SessionInfo | null {
    return this.session;
  }

  /** Built-in presets from the shared bundled YAML, via the wasm facade.
   * Sent once per webview attach; a facade failure degrades to no presets. */
  presetsMessage(): HostToWebview | null {
    try {
      const presets = this.ensureEngine().builtin_presets() as PresetsMsg["presets"];
      return { type: "presets", presets };
    } catch (err) {
      console.warn("NoViewLog: preset list unavailable:", err);
      return null;
    }
  }

  private ensureEngine(): WasmEngine {
    if (!this.engine) {
      const mod = require("./wasm/noviewlog_wasm.js") as {
        WebEngine: new () => WasmEngine;
      };
      this.engine = new mod.WebEngine();
      this.engine.set_max_records(this.maxRecords);
    }
    return this.engine;
  }

  setMaxRecords(maxRecords: number): void {
    this.maxRecords = maxRecords;
    if (this.engine) {
      this.engine.set_max_records(maxRecords);
    }
  }

  /** Full snapshot on (re)veal or after any non-append change. */
  sendSnapshot(exitStatus?: string): void {
    if (this.session === null || !this.publish_) {
      return;
    }
    const snap = this.ensureEngine().session_snapshot(this.session.id) as {
      view: { epoch: number; total_lines: number };
    } & Record<string, unknown>;
    this.sentEpoch = snap.view.epoch;
    this.sentTotal = snap.view.total_lines;
    this.publish_({
      ...(snap as unknown as HostToWebview),
      type: "snapshot",
      ...(exitStatus !== undefined ? { exit_status: exitStatus } : {}),
    } as SessionSnapshotMsg);
  }

  /** Try an incremental delta; fall back to a full snapshot. */
  private publish(): void {
    if (this.session === null || !this.publish_) {
      return;
    }
    const append = this.ensureEngine().session_append_since(
      this.session.id,
      this.sentEpoch,
      this.sentTotal,
    ) as SessionAppendMsg;
    if (append.ok) {
      this.sentEpoch = append.epoch;
      this.sentTotal = append.total_lines;
      this.publish_({ ...append, type: "append" });
    } else {
      this.sendSnapshot();
    }
  }

  startPty(command: string, cwd: string): void {
    this.stopSession();
    // An empty command is the auto-start interactive shell; name the
    // session after the shell instead of an empty string.
    const name =
      command.trim() ||
      (process.platform === "win32"
        ? process.env.COMSPEC ?? "cmd.exe"
        : process.env.SHELL ?? "/bin/sh");
    let id: number;
    try {
      const engine = this.ensureEngine();
      id = engine.session_create(name, "pty");
    } catch (err) {
      this.startFailed("engine error", err);
      return;
    }
    this.session = { id, name, source: "pty", command, cwd };
    this.restoreTabs(this.ensureEngine(), id);
    this.sentEpoch = 0;
    this.sentTotal = 0;
    // Desktop default grid; the surface refines it once the webview reports.
    const gen = ++this.ptyGeneration;
    try {
      this.pty = spawnPty(command, cwd, 80, 24, {
        onData: (data) => {
          if (this.ptyGeneration === gen) {
            this.enqueue(new TextEncoder().encode(data));
          }
        },
        onExit: (status) => {
          if (this.ptyGeneration === gen) {
            this.onProcessExit(status);
          }
        },
      });
    } catch (err) {
      this.ensureEngine().session_close(id);
      this.session = null;
      this.startFailed("failed to spawn command", err);
      return;
    }
    this.onSessionChanged?.();
    this.publish();
  }

  /** Surface a start failure to the user instead of swallowing it. */
  private startFailed(what: string, err: unknown): void {
    const message = err instanceof Error ? err.message : String(err);
    void vscode.window.showErrorMessage(`NoViewLog: ${what}: ${message}`);
    this.onSessionChanged?.();
    this.publish();
  }

  startFile(path: string): void {
    this.stopSession();
    let id: number;
    try {
      const engine = this.ensureEngine();
      id = engine.session_create(path, "file");
    } catch (err) {
      this.startFailed("engine error", err);
      return;
    }
    this.session = { id, name: path, source: "file", filePath: path };
    this.restoreTabs(this.ensureEngine(), id);
    this.sentEpoch = 0;
    this.sentTotal = 0;
    this.fileOffset = 0;

    const readTail = () => void this.readFileTail(path);
    readTail();

    // Watch the exact file (basename in its dir) — a "*" RelativePattern
    // against a file base is version-dependent and may never fire.
    const watcher = vscode.workspace.createFileSystemWatcher(
      new vscode.RelativePattern(
        vscode.Uri.file(nodePath.dirname(path)),
        nodePath.basename(path),
      ),
    );
    const readTailIfCurrent = () => {
      if (this.session?.filePath === path) {
        readTail();
      }
    };
    const subs = [
      watcher.onDidChange(readTailIfCurrent),
      watcher.onDidCreate(readTailIfCurrent),
      watcher.onDidDelete(() => {
        // The watched file is gone; finish the session visibly instead of
        // leaving a stale tail.
        if (this.session?.filePath !== path) {
          return;
        }
        this.flushBatch();
        this.ensureEngine().session_finish(this.session.id);
        this.sendSnapshot("file deleted");
        this.publish_?.({ type: "exit", status: "file deleted" });
      }),
    ];
    this.fileWatcher = new vscode.Disposable(() => {
      subs.forEach((s) => s.dispose());
      watcher.dispose();
    });
    this.publish();
  }

  private async readFileTail(path: string): Promise<void> {
    if (this.session?.filePath !== path) {
      return;
    }
    try {
      const uri = vscode.Uri.file(path);
      const data = await vscode.workspace.fs.readFile(uri);
      if (this.session?.filePath !== path) {
        return;
      }
      if (data.length < this.fileOffset) {
        // Truncated (rotated) file: restart the session buffer. Bytes batched
        // before the rotation belong to the old content — drop them.
        this.ensureEngine().session_close(this.session.id);
        const id = this.ensureEngine().session_create(path, "file");
        this.session = { ...this.session, id };
        this.restoreTabs(this.ensureEngine(), id);
        this.batch = [];
        this.batchBytes = 0;
        this.clearBatchTimer();
        this.sentEpoch = 0;
        this.sentTotal = 0;
        this.fileOffset = 0;
        // New session id / epoch 0: the webview's append epoch check would
        // freeze the view waiting for a full snapshot that never comes —
        // push one now, before any append for the new session is published.
        this.sendSnapshot();
      }
      if (data.length > this.fileOffset) {
        const chunk = data.subarray(this.fileOffset);
        this.fileOffset = data.length;
        this.enqueue(chunk);
      }
    } catch {
      // File not readable yet; the watcher fires again.
    }
  }

  private onProcessExit(status: string): void {
    if (this.session === null || this.session.source !== "pty") {
      return;
    }
    this.flushBatch();
    this.ensureEngine().session_finish(this.session.id);
    this.sendSnapshot(status);
    this.publish_?.({ type: "exit", status });
  }

  stopSession(): void {
    this.pty?.kill();
    this.pty = null;
    this.fileWatcher?.dispose();
    this.fileWatcher = null;
    this.clearTimers();
    if (this.session !== null) {
      try {
        this.ensureEngine().session_close(this.session.id);
      } catch {
        // Engine may not be initialized yet.
      }
      this.session = null;
    }
    this.batch = [];
    this.batchBytes = 0;
    this.onSessionChanged?.();
  }

  restart(): void {
    const session = this.session;
    if (!session) {
      return;
    }
    if (session.source === "pty" && session.command && session.cwd) {
      this.startPty(session.command, session.cwd);
    } else if (session.filePath) {
      this.startFile(session.filePath);
    }
  }

  resizeSession(cols: number, rows: number): void {
    if (this.session === null) {
      return;
    }
    // The webview computes these from layout; a NaN/garbage message must
    // never reach node-pty or the wasm engine.
    if (!Number.isFinite(cols) || !Number.isFinite(rows)) {
      return;
    }
    cols = Math.max(1, Math.min(1000, Math.floor(cols)));
    rows = Math.max(1, Math.min(1000, Math.floor(rows)));
    this.pty?.resize(cols, rows);
    this.ensureEngine().session_resize(this.session.id, cols, rows);
    this.publish();
  }

  /** Buffer bytes; flush on the batch timer or a size threshold. */
  private enqueue(bytes: Uint8Array): void {
    if (this.session === null) {
      return;
    }
    this.batch.push(bytes);
    this.batchBytes += bytes.length;
    if (this.batchBytes >= BATCH_FLUSH_BYTES) {
      this.flushBatch();
      return;
    }
    if (!this.batchTimer) {
      this.batchTimer = setTimeout(() => this.flushBatch(), BATCH_FLUSH_MS);
    }
  }

  private flushBatch(): void {
    this.clearBatchTimer();
    if (this.session === null || this.batch.length === 0) {
      return;
    }
    const merged = mergeChunks(this.batch);
    this.batch = [];
    this.batchBytes = 0;
    this.ensureEngine().session_ingest(this.session.id, merged);
    this.publish();
    // Flush the parser's pending partial record after an idle gap.
    if (!this.pendingTimer) {
      this.pendingTimer = setTimeout(() => {
        this.pendingTimer = null;
        try {
          if (
            this.session !== null &&
            this.ensureEngine().session_flush_pending(this.session.id)
          ) {
            this.publish();
          }
        } catch {
          // The session may have been finished/closed between arming the
          // timer and this callback; a stale flush is a no-op.
        }
      }, PENDING_IDLE_FLUSH_MS);
    }
  }

  private clearBatchTimer(): void {
    if (this.batchTimer) {
      clearTimeout(this.batchTimer);
      this.batchTimer = null;
    }
  }

  private clearTimers(): void {
    this.clearBatchTimer();
    if (this.pendingTimer) {
      clearTimeout(this.pendingTimer);
      this.pendingTimer = null;
    }
  }

  /** Route a webview command into the engine, then re-publish. */
  handleCommand(cmd: WebviewToHost): void {
    if (cmd.type === "ready") {
      this.sendSnapshot();
      return;
    }
    if (this.session === null) {
      return;
    }
    const engine = this.ensureEngine();
    const id = this.session.id;
    try {
      switch (cmd.type) {
        case "resize":
          this.resizeSession(cmd.cols, cmd.rows);
          return;
        case "setFilters":
          // The invalid-regex notice (if any) reaches the webview through
          // the snapshot's view.notice; the panel marks the rule inline.
          engine.filter_set(id, cmd.rules);
          break;
        case "setSeverity":
          engine.set_severity(id, cmd.mode);
          break;
        case "setSearch":
          engine.search_set(id, cmd.query, cmd.regex, cmd.caseSensitive, cmd.wholeWord);
          break;
        case "searchNext":
          engine.search_next(id);
          break;
        case "searchPrev":
          engine.search_prev(id);
          break;
        case "toggleCollapse":
          if (Number.isFinite(cmd.recordId)) {
            engine.toggle_collapse(id, cmd.recordId);
          }
          break;
        case "expandAll":
          engine.expand_all(id);
          break;
        case "collapseAll":
          engine.collapse_all(id);
          break;
        case "setFollow":
          engine.set_follow(id, cmd.on);
          break;
        case "setWrap":
          engine.set_wrap(id, cmd.on);
          break;
        case "addTab":
          engine.tab_add(id);
          break;
        case "input":
          if (typeof cmd.data === "string" && cmd.data.length > 0 && cmd.data.length <= 4096) {
            // Desktop rule: typing in the terminal re-engages follow.
            engine.set_follow(id, true);
            this.pty?.write(cmd.data);
          }
          break;
        case "switchTab":
          engine.tab_switch(id, cmd.index);
          break;
        case "closeTab":
          engine.tab_close(id, cmd.index);
          break;
        case "restoreTab":
          try {
            engine.tab_restore(id);
          } catch (restoreErr) {
            const text = restoreErr instanceof Error ? restoreErr.message : String(restoreErr);
            if (text.includes("no closed tab")) {
              // A stale Restore click is not an error worth a popup.
              return;
            }
            throw restoreErr;
          }
          break;
        case "renameTab":
          engine.tab_rename(id, cmd.index, cmd.name);
          break;
        }
    } catch (err) {
      vscode.window.showErrorMessage(
        `NoViewLog: ${err instanceof Error ? err.message : String(err)}`,
      );
      return;
    }
    switch (cmd.type) {
      case "setFilters":
      case "setSeverity":
      case "setWrap":
      case "addTab":
      case "switchTab":
      case "closeTab":
      case "restoreTab":
      case "renameTab":
        this.saveTabs();
        break;
    }
    this.publish();
  }

  dispose(): void {
    this.stopSession();
  }
}

function mergeChunks(chunks: Uint8Array[]): Uint8Array {
  if (chunks.length === 1) {
    return chunks[0]!;
  }
  const total = chunks.reduce((sum, c) => sum + c.length, 0);
  const merged = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    merged.set(chunk, offset);
    offset += chunk.length;
  }
  return merged;
}
