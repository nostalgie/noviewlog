# noviewlog-slint

Active desktop UI for NoViewLog (Slint + Rust). Default product on `main`.

Linux and Windows are equally supported. Pick the command for the **current host**.

## Run

From the repo root:

Linux:

```
bash scripts/run-slint.sh
bash scripts/run-slint.sh -- path/to/app.log
```

Windows (PowerShell):

```
.\scripts\run-slint-windows.ps1
.\scripts\run-slint-windows.ps1 -- path/to/app.log
```

## Layout

| Path | Role |
|------|------|
| `src/main.rs` | Window wiring, callbacks, tick/render loop |
| `src/engine_bridge.rs` | Timer/occlusion helpers, JSON escape |
| `src/stats_sync.rs` | `StatsSnapshot` → Slint models/properties |
| `src/input.rs` | Keys, zoom, clipboard |
| `src/window_chrome.rs` | Title-drag / occlusion (winit) |
| `src/launch_args.rs` | CLI → `LaunchConfig` |
| `ui/app.slint` | `AppWindow` shell |
| `ui/chrome-menus.slint` | Title-bar / context menus |
| `ui/sidebar.slint` | Tabs, terminals, filters, scrollbar |
| `ui/theme.slint` | Color + spacing tokens |

Engine interaction: `noviewlog_core::Engine` (commands + stats + RGBA paint).
See [`docs/architecture.md`](../../docs/architecture.md).

## Build

Daily (incremental, `opt-level = 3`, no fat LTO):

```
cargo build --profile release-dev -p noviewlog-slint
```

Publish (fat LTO):

```
cargo build --release -p noviewlog-slint
```

Windows folder staging: `.\scripts\publish-slint-windows.ps1`.
