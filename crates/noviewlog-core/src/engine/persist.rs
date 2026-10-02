//! Debounced persistence of projects/config plus presets and settings commands.

use super::*;

/// Backoff for consecutive persist failures: ×4 per failure, capped at
/// [`PERSIST_RETRY_MAX`] (issue #237).
pub(super) fn next_persist_retry_delay(current: Duration) -> Duration {
    (current * 4).min(PERSIST_RETRY_MAX)
}

/// Which persisted store a save result refers to (issue #253: announcement
/// state and recovery are tracked per store).
#[derive(Clone, Copy)]
enum PersistStore {
    Projects,
    Config,
}

impl Engine {
    /// Write dirty `projects.yaml` / `config.yaml` once the gesture burst has
    /// gone quiet (issue #62). Called from [`Self::tick`].
    pub(crate) fn flush_persist_if_due(&mut self) {
        if self
            .persist_changed_at
            .is_some_and(|at| at.elapsed() >= self.persist_retry_delay)
        {
            self.flush_persist();
        }
    }

    /// Flush debounced persistence now (debounce elapsed, or app exit via Drop).
    pub fn flush_persist(&mut self) {
        self.persist_changed_at = None;
        #[cfg(test)]
        if self.skip_projects_persist {
            self.projects_dirty = false;
            self.config_dirty = false;
            return;
        }
        if self.projects_dirty {
            self.projects_dirty = false;
            if let Some(idx) = self.active_project {
                self.projects.active_project = idx;
            }
            match self.save_projects_checked() {
                Ok(()) => self.persist_recovered(PersistStore::Projects),
                Err(err) => {
                    // Stay dirty: the next tick retries after the backoff
                    // instead of silently losing the change (issue #109).
                    self.projects_dirty = true;
                    self.persist_failed(
                        PersistStore::Projects,
                        format!("Failed to save projects: {err}"),
                    );
                }
            }
        }
        if self.config_dirty {
            self.config_dirty = false;
            match self.save_config_checked() {
                Ok(()) => self.persist_recovered(PersistStore::Config),
                Err(err) => {
                    self.config_dirty = true;
                    self.persist_failed(PersistStore::Config, format!("Config save failed: {err}"));
                }
            }
        }
    }

    /// Test seam: `persist_fail_saves` forces the failure path so backoff can
    /// be tested without a read-only config dir (issue #237).
    #[cfg(test)]
    fn save_projects_checked(&self) -> Result<(), String> {
        if self.persist_fail_saves {
            return Err("injected persist failure".into());
        }
        crate::core::config::save_projects_store(&self.projects)
    }

    #[cfg(not(test))]
    fn save_projects_checked(&self) -> Result<(), String> {
        crate::core::config::save_projects_store(&self.projects)
    }

    /// Test seam: see [`Self::save_projects_checked`].
    #[cfg(test)]
    fn save_config_checked(&self) -> Result<(), String> {
        if self.persist_fail_saves {
            return Err("injected persist failure".into());
        }
        save_user_config(&self.config)
    }

    #[cfg(not(test))]
    fn save_config_checked(&self) -> Result<(), String> {
        save_user_config(&self.config)
    }

    /// A store saved cleanly: clear the failure backoff and re-arm status
    /// reporting for the next failure streak (issue #237). Announcement
    /// state is per store (issue #253): a projects success must not re-arm
    /// the config failure announcement (and vice versa).
    fn persist_recovered(&mut self, store: PersistStore) {
        self.persist_retry_delay = PERSIST_DEBOUNCE;
        match store {
            PersistStore::Projects => self.projects_failure_announced = false,
            PersistStore::Config => self.config_failure_announced = false,
        }
    }

    /// A store failed to save: schedule a ×4-delayed retry (capped at
    /// [`PERSIST_RETRY_MAX`]) and surface the status event only on the first
    /// failure of a streak, so a permanently failing save pushes one event
    /// instead of one per debounce period (issue #237).
    fn persist_failed(&mut self, store: PersistStore, message: String) {
        self.persist_changed_at = Some(Instant::now());
        self.persist_retry_delay = next_persist_retry_delay(self.persist_retry_delay);
        let announced = match store {
            PersistStore::Projects => &mut self.projects_failure_announced,
            PersistStore::Config => &mut self.config_failure_announced,
        };
        if !*announced {
            *announced = true;
            self.status_message = message;
            self.push_event(json!({"type":"status","message": self.status_message}));
        }
    }

    /// Mark `config.yaml` changed; the write lands after [`PERSIST_DEBOUNCE`].
    /// A fresh dirty mark re-arms the normal debounce (issue #253): the
    /// failure backoff applies only to consecutive retries of the SAME
    /// unwritten change, never to a new user edit.
    pub(crate) fn mark_config_dirty(&mut self) {
        if self.config_persist_disabled {
            return;
        }
        #[cfg(test)]
        if self.skip_projects_persist {
            return;
        }
        self.config_dirty = true;
        self.persist_changed_at = Some(Instant::now());
        self.persist_retry_delay = PERSIST_DEBOUNCE;
    }

    pub(crate) fn preset_apply(&mut self, name: &str) {
        if !self.config.presets.contains_key(name) {
            self.status_message = format!("Preset not found: {name}");
            self.push_event(json!({"type":"status","message": self.status_message}));
            return;
        }
        let filters = load_preset(&self.config, name);
        self.preset_name = name.to_string();
        let on_terminal_tab = self.active_terminal().active_view == 0;
        if on_terminal_tab {
            // The Terminal tab keeps an unfiltered stream; presets do nothing there.
            self.active_view_mut().clear_filters();
        } else {
            self.active_view_mut().set_filters(filters);
        }
        self.rebuild_if_needed();
        self.status_message = format!("Applied preset: {name}");
        self.push_event(json!({"type":"status","message": self.status_message}));
        // Presets replace the whole filter set — snapshot it (#109).
        self.sync_active_project_from_terminals();
    }

    pub(crate) fn preset_get(&mut self, name: &str) {
        match self.config.presets.get(name) {
            Some(preset) => {
                let filters: Vec<serde_json::Value> = preset
                    .filters
                    .iter()
                    .map(|f| {
                        json!({
                            "id": f.id,
                            "type": f.filter_type,
                            "pattern": f.pattern,
                            "enabled": f.enabled,
                            "use_regex": f.use_regex,
                        })
                    })
                    .collect();
                self.push_event(json!({
                    "type": "preset",
                    "name": name,
                    "filters": filters,
                }));
            }
            None => {
                self.push_event(json!({
                    "type": "preset",
                    "name": name,
                    "filters": [],
                    "error": "not found",
                }));
            }
        }
    }

    pub(crate) fn set_settings(&mut self, max_scrollback_lines: usize) {
        let capped = clamp_max_scrollback_lines(max_scrollback_lines);
        self.config.max_scrollback_lines = capped;
        for terminal in &mut self.terminals {
            let shifted = terminal.buffer.set_max_records(capped);
            if shifted > 0 {
                terminal.buffer_line_start += shifted as u64;
            }
            for view in &mut terminal.views {
                view.mark_flat_lines_dirty();
            }
        }
        self.mark_config_dirty();
        self.status_message = format!("Settings saved (max scrollback: {capped})");
        self.push_event(json!({"type":"status","message": self.status_message}));
        self.mark_viewport_dirty();
    }

    pub(crate) fn set_sidebar_expanded(&mut self, terminals: bool, files: bool) {
        self.config.terminals_section_expanded = terminals;
        self.config.files_section_expanded = files;
        self.mark_config_dirty();
        self.last_stats_at = None;
    }

    pub(crate) fn preset_save(&mut self, name: &str, filters: Vec<FilterRule>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let (filters, notices): (Vec<_>, Vec<_>) =
            filters.into_iter().map(compile_filter_checked).fold(
                (Vec::new(), Vec::new()),
                |(mut rules, mut warns), (r, w)| {
                    rules.push(r);
                    warns.extend(w);
                    (rules, warns)
                },
            );
        for w in notices {
            self.push_event(json!({"type":"status","message": w}));
        }
        self.config
            .presets
            .insert(name.to_string(), PresetConfig { filters });
        self.mark_config_dirty();
        self.status_message = format!("Preset saved: {name}");
        self.push_event(json!({"type":"status","message": self.status_message}));
    }

    pub(crate) fn preset_delete(&mut self, name: &str) {
        if !self.config.presets.contains_key(name) {
            return;
        }
        self.config.presets.remove(name);
        if self.preset_name == name {
            self.preset_name = self.config.default_preset.clone();
        }
        self.mark_config_dirty();
        self.status_message = format!("Preset deleted: {name}");
        self.push_event(json!({"type":"status","message": self.status_message}));
    }

    pub(crate) fn preset_create_from_tab(&mut self, name: &str) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let filters = self.active_view().filters().to_vec();
        self.preset_save(name, filters);
        self.preset_name = name.to_string();
    }
}
