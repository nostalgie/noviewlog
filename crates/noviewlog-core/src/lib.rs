//! NoViewLog shared engine.
//!
//! # Host surface
//!
//! The Slint host primarily uses [`Engine`]:
//! `tick`, `render`, commands, and stats/events.
//!
//! # Vocabulary
//!
//! - **Terminal** — independent session ([`terminal_state::TerminalState`]): PTY,
//!   process, or read-only file.
//! - **Tab / View** — filter view inside a terminal ([`log_view::LogView`]).
//!   JSON/UI commands use the name `tab_*`; the Rust type is `LogView`.
//! - **Terminal tab** — built-in first tab (index 0); display name
//!   [`TERMINAL_TAB_NAME`].
//!
//! See `docs/architecture.md` in the repo root.
//!
//! # Module visibility
//!
//! Many modules are `pub` for historical test access. Prefer `Engine` and
//! `core::types` from hosts; treat other paths as unstable internals.

#![recursion_limit = "256"]

pub mod color_emoji;
pub mod colr_paint;
pub mod core;
pub mod engine;
pub mod file_index;
pub mod file_load;
pub mod file_match;
pub mod log_view;
pub mod pty;
pub mod scrollbar_math;
pub mod spawn_resolve;
pub mod spawn_resolver;
pub mod terminal_state;
pub mod viewport;
pub mod viewport_layout;

pub use engine::{
    parse_engine_event, Command, Engine, EngineEvent, StatsProject, StatsSnapshot, StatsTab,
    StatsTerminal, CARET_BLINK_PERIOD,
};
pub use log_view::TERMINAL_TAB_NAME;
pub use scrollbar_math::{thumb_offset_len, MIN_THUMB_PX};

#[cfg(test)]
mod tests;
