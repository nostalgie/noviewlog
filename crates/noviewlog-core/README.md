# noviewlog-core

Shared Rust engine for NoViewLog: PTY sessions, log parsing, filters, file
windows, and fontdue viewport rendering. Built as an `rlib` for
`noviewlog-slint`.

## Use from Slint

```rust
use noviewlog_core::Engine;
```

Prefer typed `Command` + `send_command` / `apply_command`, and
`parse_engine_event` → `StatsSnapshot`. JSON (`send_command_json` /
`poll_event_json`) remains available for tests and tooling.

## Layout

| Path | Role |
|------|------|
| `src/engine/` | Façade + commands, events, stats, file/PTY/scroll |
| `src/terminal_state.rs` | `TerminalState` session bag |
| `src/log_view.rs` | Per-tab (`LogView`) filters and search |
| `src/core/` | Parser, filters, buffer, ANSI, config |
| `src/viewport.rs` | Bitmap renderer |
| `src/pty.rs` | PTY manager |

See [`docs/architecture.md`](../../docs/architecture.md) for vocabulary and
data flow.

## Tests

```
cargo test -p noviewlog-core --lib
```
