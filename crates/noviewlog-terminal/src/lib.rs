//! NoViewLog terminal layer — the GUI-independent half of the engine.
//!
//! Owns everything that turns a byte stream into filterable, styled log
//! records: VTE screen emulation ([`terminal`]), record parsing
//! ([`parser`]), the record buffer ([`buffer`]), filters ([`filter`]),
//! SGR handling ([`ansi`]), and projection to display lines ([`visible`]).
//!
//! No PTY, no fonts, no filesystem, no GUI: a host supplies bytes and
//! consumes records / [`types::FlatLine`]s. This crate is the intended
//! publish target (its dependency set is wasm-friendly; actual wasm gating
//! is deferred until a wasm host exists — #200); product concerns (config
//! files, presets, engine façade, viewport) stay in `noviewlog-core`.

pub mod ansi;
pub mod buffer;
pub mod filter;
pub mod formats;
pub mod parser;
pub mod terminal;
pub mod types;
pub mod visible;
