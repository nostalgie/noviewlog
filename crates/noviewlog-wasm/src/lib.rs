//! NoViewLog wasm facade — a multi-session log engine for JS hosts.
//!
//! [`WebEngine`] wraps `noviewlog-terminal` (VTE ingest, Record parsing,
//! buffer, filters) for non-Slint hosts such as the VS Code webview
//! extension. Sessions are addressed by id from day one; the v1 host runs
//! one session, v2 workspace projects reuse the same API. Snapshots cross
//! to JavaScript via serde-wasm-bindgen as plain objects — no JSON string
//! round-trip.

pub mod presets;
pub mod session;
pub mod snapshot;

use wasm_bindgen::prelude::*;

use noviewlog_terminal::types::WorkspaceConfig;

use crate::session::{Session, SessionSource};
use crate::snapshot::{session_append_since, session_snapshot};

/// Host-facing engine: owns N sessions and compiles to wasm32.
///
/// Method errors are human-readable `String`s (desktop `Result<_, String>`
/// convention); wasm-bindgen turns them into JS exceptions.
#[wasm_bindgen]
pub struct WebEngine {
    sessions: Vec<Session>,
    next_id: u32,
    max_records: usize,
}

#[wasm_bindgen]
impl WebEngine {
    #[wasm_bindgen(constructor)]
    pub fn new() -> WebEngine {
        console_error_panic_hook::set_once();
        WebEngine {
            sessions: Vec::new(),
            next_id: 1,
            max_records: Session::DEFAULT_MAX_RECORDS,
        }
    }

    /// Ring capacity (records) for new sessions; clamped to the desktop
    /// config bounds.
    pub fn set_max_records(&mut self, max_records: usize) {
        self.max_records = noviewlog_terminal::types::clamp_max_scrollback_lines(max_records);
    }

    /// Create a session (`source`: `"pty"` or `"file"`). Returns its id.
    pub fn session_create(&mut self, name: &str, source: &str) -> Result<u32, String> {
        let source = SessionSource::parse(source)?;
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let name = if name.trim().is_empty() {
            "Terminal".to_string()
        } else {
            name.trim().to_string()
        };
        self.sessions
            .push(Session::new(id, name, source, self.max_records));
        Ok(id as u32)
    }

    pub fn session_close(&mut self, id: u32) -> Result<(), String> {
        let before = self.sessions.len();
        self.sessions.retain(|s| s.id != id);
        if self.sessions.len() == before {
            return Err(format!("no session {id}"));
        }
        Ok(())
    }

    pub fn session(&self, id: u32) -> Result<bool, String> {
        Ok(self.sessions.iter().any(|s| s.id == id))
    }

    /// Feed raw bytes (PTY output or file content) into a session.
    pub fn session_ingest(&mut self, id: u32, bytes: &[u8]) -> Result<(), String> {
        self.session_mut(id)?.feed(bytes);
        Ok(())
    }

    /// Flush a pending Record after an idle period; `true` when flushed.
    pub fn session_flush_pending(&mut self, id: u32) -> Result<bool, String> {
        Ok(self.session_mut(id)?.flush_pending())
    }

    /// Commit the final screen state (process exit / end of file).
    pub fn session_finish(&mut self, id: u32) -> Result<(), String> {
        self.session_mut(id)?.finish();
        Ok(())
    }

    /// Resize the emulator grid (columns / rows from the webview).
    pub fn session_resize(&mut self, id: u32, cols: u32, rows: u32) -> Result<(), String> {
        self.session_mut(id)?.resize(cols as usize, rows as usize);
        Ok(())
    }

    /// Full snapshot of the session's active Tab/View as a JS object.
    pub fn session_snapshot(&mut self, id: u32) -> Result<JsValue, String> {
        let session = self.session_mut(id)?;
        serde_wasm_bindgen::to_value(&session_snapshot(session))
            .map_err(|e| format!("snapshot serialization failed: {e}"))
    }

    /// Incremental snapshot: lines after `base`, valid only while `epoch`
    /// is unchanged. `ok=false` → request a full snapshot instead.
    pub fn session_append_since(
        &mut self,
        id: u32,
        epoch: u32,
        base: usize,
    ) -> Result<JsValue, String> {
        let session = self.session_mut(id)?;
        serde_wasm_bindgen::to_value(&session_append_since(session, epoch, base))
            .map_err(|e| format!("append serialization failed: {e}"))
    }

    // ----- view commands -----

    /// Severity mode for the active tab: `all|error|warn|info|debug|unleveled`.
    pub fn set_severity(&mut self, id: u32, mode: &str) -> Result<(), String> {
        let parsed = noviewlog_terminal::types::SeverityFilter::parse(mode)
            .ok_or_else(|| format!("unknown severity mode '{mode}'"))?;
        self.session_mut(id)?.set_severity(parsed);
        Ok(())
    }

    pub fn set_follow(&mut self, id: u32, on: bool) -> Result<(), String> {
        self.session_mut(id)?.set_follow(on);
        Ok(())
    }

    pub fn set_wrap(&mut self, id: u32, on: bool) -> Result<(), String> {
        self.session_mut(id)?.set_wrap(on);
        Ok(())
    }

    pub fn toggle_collapse(&mut self, id: u32, record_id: f64) -> Result<(), String> {
        self.session_mut(id)?.toggle_collapse(record_id as u64);
        Ok(())
    }

    pub fn expand_all(&mut self, id: u32) -> Result<(), String> {
        self.session_mut(id)?.expand_all();
        Ok(())
    }

    pub fn collapse_all(&mut self, id: u32) -> Result<(), String> {
        self.session_mut(id)?.collapse_all();
        Ok(())
    }

    /// Set the active tab's search. An invalid regex surfaces in the
    /// snapshot's `search.error`, not as an exception (mid-typing UX).
    pub fn search_set(
        &mut self,
        id: u32,
        query: &str,
        regex: bool,
        case_sensitive: bool,
        whole_word: bool,
    ) -> Result<(), String> {
        self.session_mut(id)?
            .search_set(query.to_string(), regex, case_sensitive, whole_word);
        Ok(())
    }

    pub fn search_next(&mut self, id: u32) -> Result<(), String> {
        self.session_mut(id)?.search_navigate(1);
        Ok(())
    }

    pub fn search_prev(&mut self, id: u32) -> Result<(), String> {
        self.session_mut(id)?.search_navigate(-1);
        Ok(())
    }

    /// Replace the active tab's filter rules (JSON array of FilterRule
    /// objects; the Terminal tab refuses). The invalid-regex notice, if any,
    /// is stored on the session and travels to the webview in the next
    /// snapshot's `view.notice`.
    pub fn filter_set(&mut self, id: u32, rules: JsValue) -> Result<JsValue, String> {
        let rules: Vec<noviewlog_terminal::types::FilterRule> =
            serde_wasm_bindgen::from_value(rules)
                .map_err(|e| format!("invalid filter rules: {e}"))?;
        let session = self.session_mut(id)?;
        let notice = session.filter_set(rules)?;
        serde_wasm_bindgen::to_value(&notice)
            .map_err(|e| format!("notice serialization failed: {e}"))
    }

    /// Bundled filter presets from the shared `presets/defaults.yaml`
    /// (desktop parity): `[{ id, filters: [FilterRule...] }]`.
    pub fn builtin_presets(&self) -> Result<JsValue, String> {
        serde_wasm_bindgen::to_value(&crate::presets::builtin_presets()?)
            .map_err(|e| format!("presets serialization failed: {e}"))
    }

    pub fn tab_add(&mut self, id: u32) -> Result<(), String> {
        self.session_mut(id)?.tab_add();
        Ok(())
    }

    pub fn tab_close(&mut self, id: u32, index: u32) -> Result<(), String> {
        self.session_mut(id)?.tab_close(index as usize)
    }

    pub fn tab_switch(&mut self, id: u32, index: u32) -> Result<(), String> {
        self.session_mut(id)?.tab_switch(index as usize)
    }

    pub fn tab_rename(&mut self, id: u32, index: u32, name: &str) -> Result<(), String> {
        self.session_mut(id)?
            .tab_rename(index as usize, name.to_string())
    }

    pub fn tab_restore(&mut self, id: u32) -> Result<(), String> {
        self.session_mut(id)?.tab_restore()
    }

    pub fn can_restore_tab(&mut self, id: u32) -> Result<bool, String> {
        Ok(self.session_mut(id)?.can_restore_tab())
    }

    /// Serialize the session's full tab configuration for workspace
    /// persistence (`{ tabs: [...], active_tab: n }`).
    pub fn tabs_export(&mut self, id: u32) -> Result<JsValue, String> {
        let session = self.session_mut(id)?;
        let config = WorkspaceConfig {
            tabs: session.tabs_to_config(),
            active_tab: session.active_view,
        };
        serde_wasm_bindgen::to_value(&config).map_err(|e| format!("serialize tabs: {e}"))
    }

    /// Restore a previously exported tab configuration; returns
    /// `Err` on a config without tabs.
    pub fn tabs_import(&mut self, id: u32, config: JsValue) -> Result<(), String> {
        let parsed: WorkspaceConfig = serde_wasm_bindgen::from_value(config)
            .map_err(|e| format!("invalid tab config: {e}"))?;
        let active = parsed.active_tab;
        let session = self.session_mut(id)?;
        session.tabs_apply_config(parsed.tabs, active)
    }
}

impl WebEngine {
    fn session_mut(&mut self, id: u32) -> Result<&mut Session, String> {
        self.sessions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("no session {id}"))
    }
}

impl Default for WebEngine {
    fn default() -> Self {
        Self::new()
    }
}
