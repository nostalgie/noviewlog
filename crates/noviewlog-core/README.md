# noviewlog-core

Shared Rust engine for NoViewLog: multi-terminal sessions, PTY I/O, FILES
sliding windows, Projects, and fontdue Viewport rendering. Built as an
`rlib` for `noviewlog-slint` (and used by `noviewlog-tui`).

Parsing, filters, the record buffer, and the VTE/ANSI layer live in
**`noviewlog-terminal`**. This crate re-exports them at
`noviewlog_core::core::*` and adds session orchestration on top.

## Use from Slint

```rust
use noviewlog_core::Engine;
```

Prefer typed `Command` + `send_command` / `apply_command`, and
`parse_engine_event` → `StatsSnapshot`. JSON (`send_command_json` /
`poll_event_json`) remains available for tests and tooling.

Hosts should treat non-`Engine` `pub` modules as unstable internals
(historical test access — see `lib.rs`).

## Layout

| Path | Role |
|------|------|
| `src/engine/` | Façade: commands, tick, stats, events, PTY lifecycle, FILES, Projects, scroll/selection |
| `src/terminal_state.rs` | `TerminalState` session bag (views, buffer, file window) |
| `src/log_view.rs` | Per-tab (`LogView`) filters, search, flat lines |
| `src/core/` | Product `config` + re-exports of `noviewlog-terminal` |
| `src/pty.rs` | PTY manager (reader/writer threads) |
| `src/viewport/` | Fontdue bitmap paint (`mod.rs` + fonts/primitives) |
| `src/viewport_layout.rs` | Soft-wrap / selection geometry |
| `src/file_load.rs` | File open, encoding, window ingest budgets |
| `src/file_index.rs` | Sparse / on-disk line index, shared file handle |
| `src/file_match.rs` | Whole-file match index for filter tabs / search |
| `src/spawn_resolve/` | Sync spawn plan (PATH / shell / WSL / argv) |
| `src/spawn_resolver.rs` | Async PATH/registry resolution off the UI thread |
| `src/colr_paint.rs`, `color_emoji.rs` | Color emoji paint helpers |

See [`docs/architecture.md`](../../docs/architecture.md) for vocabulary and
host data flow.

## Tests

```
cargo test -p noviewlog-core --lib
```
