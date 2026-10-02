# Architecture

NoViewLog is a native desktop log viewer. **Slint** (`noviewlog-slint`) is the
desktop UI. The shared engine lives in `noviewlog-core`, which builds on the
GUI-free terminal layer `noviewlog-terminal`. The same terminal layer also
hosts the other frontends: `noviewlog-tui` (terminal UI incl. SSH) and
`noviewlog-wasm` (wasm facade behind the VS Code extension in
`extensions/vscode/`). **Linux** and **Windows** are equally supported;
macOS and other OSes are best-effort. Daily build/run:
`scripts/run-slint.sh` (Linux) or `scripts/run-slint-windows.ps1` (Windows).

## Vocabulary

| Term | Meaning |
|------|---------|
| **Terminal** | Independent session (PTY shell, process, or read-only log file) |
| **Tab / View** | Filter view inside a terminal (`LogView`). JSON commands use `tab_*`; the type is `LogView` |
| **Terminal tab** | Built-in first tab (index 0); display name `Terminal`; not filter-editable; not renameable in Slint |
| **Record** | Parsed log unit (may span multiple physical lines) |
| **Viewport** | Fontdue-rendered RGBA bitmap of the visible slice |

```
TerminalState
 ├── views: Vec<LogView>     # Terminal tab + filter tabs
 ├── buffer: RecordBuffer
 ├── ingest / parser
 └── optional file session (file_load / file_backed)
```

## Data flow

```
Slint UI  --Command (typed or JSON)-->  Engine
          <--stats / events JSON-----
          <--RGBA pixels (render)----  ViewportRenderer
PTY bytes --> Engine --> TerminalIngest (VTE grid)
                         ├── committed rows --> RecordBuffer --> filter tabs / scroll-up Terminal tab
                         └── Follow Terminal tab paints the cell grid (not LogView overlay)
```

### Host API (Slint)

Slint links `noviewlog-core` as an `rlib` and calls `Engine` directly:

- `tick` / `needs_render` / `render`
- Typed `Command` via `send_command` / `apply_command` (preferred)
- `poll_event_json` + `parse_engine_event` → `StatsSnapshot` / `EngineEvent`
- `handle_key`, `set_launch`, `selection_text`
- JSON (`send_command_json`) remains for tests and tooling

First paint: the host forces an opaque winit surface (`with_transparent(false)`),
gates the Viewport `Image` until a bitmap exists, and seeds an opaque placeholder
so the first map never composites the desktop through an empty cell.

## Dual ANSI paths

| Layer | Module | Role |
|-------|--------|------|
| Live VT | `core/terminal.rs` | `vte` grid; Follow paints grid rows; scroll-up overlay `FlatLine`s from cells; committed rows serialized to ANSI for Records |
| Line SGR | `core/ansi.rs` | Parse/strip/overlay SGR on stored record lines (non-VT) |

When fixing coloring or escape handling, identify which layer owns the bug
before changing code.

## Filter pipeline (per Tab/View)

1. Include/exclude `FilterRule`s (`FilterEngine`)
2. Severity mode (`SeverityFilter` on `LogView`: All / Error / Warn / Info / Debug / Unleveled)
3. Collapse multiline Records (default collapsed unless Record id is in the view's expand set)
4. Flat lines → search → Viewport paint (muted severity gutter + disclosure cues)

Severity is orthogonal to presets and does not persist in `config.yaml` in v1.
Expand/collapse state is per Tab/View and not persisted across restarts.
Click a collapsed preview (or disclosure cue) to toggle; View → Expand/Collapse all records.

## Key modules (engine)

| Module | Responsibility |
|--------|----------------|
| `engine` | Session façade: commands, tick, stats, multi-terminal, Projects |
| `engine/projects` | Project open/create, Program restore, manual Start |
| `terminal_state` / `TerminalState` | Per-terminal bag (views, buffer, file window) |
| `log_view` | Per-tab filters + search + flat lines |
| `pty` | Process I/O |
| `viewport` + `viewport_layout` | Paint + soft-wrap / selection geometry |

Parser, filters, record buffer, formats, and the ANSI/VTE layer live in the
separate **`noviewlog-terminal`** crate so non-Slint hosts (TUI, wasm) can
reuse them; `noviewlog-core` composes them into sessions and views.

## Projects (high level)

Programs persist in `projects.yaml`. **Open / cold start** always selects the
**Terminal** tab on live sessions and leaves Programs **stopped** until Start
(FILES may begin load). CLI launch MAY start a one-shot command. Details:
[`docs/terminals.md`](terminals.md).
