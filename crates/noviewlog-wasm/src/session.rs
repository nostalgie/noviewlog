//! One Terminal session inside the wasm engine: ingest loop + views.
//!
//! Mirrors the desktop split (`noviewlog-core`): `TerminalIngest` owns the
//! VTE grid and commits scrolled-off rows, `RecordParser` groups them into
//! Records, `RecordBuffer` is the ring, and every Tab/View runs the pipeline
//! FilterEngine → SeverityFilter → collapse → flat lines → search
//! (see docs/architecture.md). No wasm-bindgen types here so the logic is
//! unit-testable on any host target.

use std::collections::HashSet;

use noviewlog_terminal::buffer::RecordBuffer;
use noviewlog_terminal::filter::FilterEngine;
use noviewlog_terminal::parser::RecordParser;
use noviewlog_terminal::terminal::TerminalIngest;
use noviewlog_terminal::types::{
    compile_filter_checked, FilterRule, FlatLine, LogLevel, SearchMatch, SeverityFilter, TabConfig,
    DEFAULT_MAX_SCROLLBACK_LINES,
};
use noviewlog_terminal::visible::{
    append_search_matches, collect_search_matches, compile_search_pattern, rebuild_flat_lines,
    rebuild_flat_lines_for_records, record_ids_needing_expand_for_search, SearchPattern,
};

/// Display name of the pinned first tab (index 0), as on the desktop.
pub const TERMINAL_TAB_NAME: &str = "Terminal";

/// Restore-stack cap; mirrors `MAX_CLOSED_TABS` on the desktop engine.
const MAX_CLOSED_TABS: usize = 15;

/// Where a session's bytes come from (the PTY itself lives in the host).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionSource {
    Pty,
    File,
}

impl SessionSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pty => "pty",
            Self::File => "file",
        }
    }

    pub fn parse(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "pty" => Ok(Self::Pty),
            "file" => Ok(Self::File),
            other => Err(format!(
                "unknown session source '{other}' (expected \"pty\" or \"file\")"
            )),
        }
    }
}

pub(crate) fn level_name(level: Option<LogLevel>) -> Option<&'static str> {
    match level {
        Some(LogLevel::Error) => Some("error"),
        Some(LogLevel::Warn) => Some("warn"),
        Some(LogLevel::Info) => Some("info"),
        Some(LogLevel::Debug) => Some("debug"),
        None => None,
    }
}

/// Per-session Tab/View: filters, severity, collapse set, search, flat lines.
///
/// A simplified `noviewlog-core::LogView`: no viewport wrap index (the
/// webview wraps in TypeScript), no file match window (the whole file is
/// streamed through ingest), and no live-overlay patching fast paths —
/// the Terminal tab recomposes its overlay tail on every refresh.
pub struct SessionView {
    pub name: String,
    filter_engine: FilterEngine,
    pub severity: SeverityFilter,
    pub expanded_record_ids: HashSet<u64>,
    pub auto_follow: bool,
    pub wrap_lines: bool,
    pub search_query: String,
    pub search_regex: bool,
    pub search_case_sensitive: bool,
    pub search_whole_word: bool,
    pub search_error: Option<String>,
    search_pattern: Option<SearchPattern>,
    search_matches: Vec<SearchMatch>,
    search_match_index: usize,
    search_scan_end: usize,
    search_full_rescan: bool,
    search_dirty: bool,
    search_jump_to_last: bool,
    search_scroll_pending: bool,
    /// Bumped when the view asks the renderer to scroll (search navigation).
    pub scroll_request: u32,
    pub flat_lines: Vec<FlatLine>,
    flat_lines_dirty: bool,
    flat_lines_record_cursor: usize,
    /// Live VT overlay line count at the end of `flat_lines`.
    overlay_len: usize,
    /// `buffer.dropped_count()` at the last rebuild — a change means the ring
    /// shifted and every line index moved.
    last_dropped: usize,
}

impl SessionView {
    fn new_terminal() -> Self {
        Self::from_tab_config(TabConfig {
            name: TERMINAL_TAB_NAME.to_string(),
            filters: Vec::new(),
            search_query: String::new(),
            search_regex: false,
            search_case_sensitive: false,
            search_whole_word: false,
            auto_follow: true,
            wrap_lines: true,
            severity: SeverityFilter::default(),
        })
    }

    pub fn from_tab_config(tab: TabConfig) -> Self {
        Self {
            name: tab.name,
            filter_engine: FilterEngine::new(tab.filters),
            severity: tab.severity,
            expanded_record_ids: HashSet::new(),
            auto_follow: tab.auto_follow,
            wrap_lines: tab.wrap_lines,
            search_query: tab.search_query,
            search_regex: tab.search_regex,
            search_case_sensitive: tab.search_case_sensitive,
            search_whole_word: tab.search_whole_word,
            search_error: None,
            search_pattern: None,
            search_matches: Vec::new(),
            search_match_index: 0,
            search_scan_end: 0,
            search_full_rescan: true,
            search_dirty: true,
            search_jump_to_last: false,
            search_scroll_pending: false,
            scroll_request: 0,
            flat_lines: Vec::new(),
            flat_lines_dirty: true,
            flat_lines_record_cursor: 0,
            overlay_len: 0,
            last_dropped: 0,
        }
    }

    pub fn to_tab_config(&self) -> TabConfig {
        TabConfig {
            name: self.name.clone(),
            filters: self.filter_engine.filters().to_vec(),
            search_query: self.search_query.clone(),
            search_regex: self.search_regex,
            search_case_sensitive: self.search_case_sensitive,
            search_whole_word: self.search_whole_word,
            auto_follow: self.auto_follow,
            wrap_lines: self.wrap_lines,
            severity: self.severity,
        }
    }

    pub fn filters(&self) -> &[FilterRule] {
        self.filter_engine.filters()
    }

    pub fn set_severity(&mut self, mode: SeverityFilter) {
        if self.severity != mode {
            self.severity = mode;
            self.flat_lines_dirty = true;
        }
    }

    /// Compile and replace all filter rules. Returns a human-readable notice
    /// when a regex rule was invalid and fell back to literal matching
    /// (desktop `compile_filter_checked` semantics).
    pub fn set_filters(&mut self, rules: Vec<FilterRule>) -> Option<String> {
        let mut notice = None;
        let compiled: Vec<FilterRule> = rules
            .into_iter()
            .map(|rule| {
                let (rule, n) = compile_filter_checked(rule);
                if n.is_some() {
                    notice = n;
                }
                rule
            })
            .collect();
        self.filter_engine.set_filters(compiled);
        self.flat_lines_dirty = true;
        notice
    }

    pub fn toggle_record_collapse(&mut self, record_id: u64) {
        if self.expanded_record_ids.contains(&record_id) {
            self.expanded_record_ids.remove(&record_id);
        } else {
            self.expanded_record_ids.insert(record_id);
        }
        self.flat_lines_dirty = true;
    }

    pub fn expand_all_multiline(&mut self, buffer: &mut RecordBuffer) {
        for record in self.filter_engine.filter_records(buffer.records()) {
            if !self.severity.allows(record.effective_level()) {
                continue;
            }
            if record.lines.len() >= 2 {
                self.expanded_record_ids.insert(record.id);
            }
        }
        self.flat_lines_dirty = true;
    }

    pub fn collapse_all_multiline(&mut self) {
        self.expanded_record_ids.clear();
        self.flat_lines_dirty = true;
    }

    pub fn set_search(
        &mut self,
        query: String,
        regex: bool,
        case_sensitive: bool,
        whole_word: bool,
    ) {
        let changed = self.search_query != query
            || self.search_regex != regex
            || self.search_case_sensitive != case_sensitive
            || self.search_whole_word != whole_word;
        self.search_query = query;
        self.search_regex = regex;
        self.search_case_sensitive = case_sensitive;
        self.search_whole_word = whole_word;
        if changed {
            self.mark_search_changed();
        }
    }

    /// Desktop `mark_search_changed`: a new search jumps to the last match
    /// and scrolls there once matches are scanned.
    pub fn mark_search_changed(&mut self) {
        self.search_jump_to_last = true;
        self.search_dirty = true;
        self.search_scroll_pending = true;
        self.search_full_rescan = true;
        self.search_scan_end = 0;
        self.search_pattern = None;
    }

    /// Navigate matches with wrap-around (`delta` typically ±1). Consumes
    /// its own scroll request — matches already exist, no rescan needed.
    pub fn search_navigate(&mut self, delta: i64) {
        if self.search_matches.is_empty() {
            return;
        }
        let len = self.search_matches.len() as i64;
        let current = self.search_match_index as i64;
        let next = (current + delta).rem_euclid(len);
        self.search_match_index = next as usize;
        if self.search_active_line().is_some() {
            self.search_scroll_pending = false;
            self.scroll_request += 1;
        } else {
            self.search_scroll_pending = true;
        }
    }

    pub fn search_matches_len(&self) -> usize {
        self.search_matches.len()
    }

    pub fn search_active_line(&self) -> Option<usize> {
        self.search_matches
            .get(self.search_match_index)
            .map(|m| m.line_index)
    }

    /// Desktop counter label: `index+1/total`, or `0/0` when nothing matches.
    pub fn search_counter_label(&self) -> String {
        if self.search_query.is_empty() || self.search_error.is_some() {
            return String::new();
        }
        let total = self.search_matches.len();
        if total == 0 {
            "0/0".to_string()
        } else {
            format!("{}/{}", self.search_match_index + 1, total)
        }
    }

    /// Bring flat lines + search in sync with the buffer; `overlay` replaces
    /// the live VT tail (Terminal tab only, empty for filter tabs).
    pub fn refresh(&mut self, buffer: &mut RecordBuffer, overlay: &[FlatLine]) {
        let dropped = buffer.dropped_count();
        if self.flat_lines_dirty || dropped != self.last_dropped {
            self.flat_lines = rebuild_flat_lines(
                buffer,
                &self.filter_engine,
                self.severity,
                &self.expanded_record_ids,
            );
            self.flat_lines_record_cursor = buffer.records_len();
            self.flat_lines_dirty = false;
            self.overlay_len = 0;
            self.last_dropped = dropped;
            self.search_dirty = true;
            self.search_full_rescan = true;
            self.search_scan_end = 0;
        } else if self.flat_lines_record_cursor < buffer.records_len() {
            let cursor = self.flat_lines_record_cursor;
            let appended = rebuild_flat_lines_for_records(
                &buffer.records()[cursor..],
                &self.filter_engine,
                self.severity,
                &self.expanded_record_ids,
            );
            self.flat_lines_record_cursor = buffer.records_len();
            if !appended.is_empty() {
                // Committed lines belong BEFORE the live overlay tail.
                let split = self.flat_lines.len() - self.overlay_len;
                self.flat_lines.splice(split..split, appended);
                self.search_dirty = true;
                if self.overlay_len > 0 {
                    // The splice shifts the overlay range and may overwrite
                    // previously scanned overlay content; incremental scan
                    // offsets cannot describe that, so rescan in full.
                    self.search_full_rescan = true;
                    self.search_scan_end = 0;
                }
            }
        }
        self.replace_overlay(overlay);
        if self.search_dirty {
            self.search_dirty = false;
            self.refresh_search_with_buffer(buffer);
        }
    }

    /// Replace the live VT overlay tail, keeping only lines that pass this
    /// view's include/exclude and severity (desktop `set_filtered_live_overlay`).
    fn replace_overlay(&mut self, overlay: &[FlatLine]) {
        let filtered: Vec<FlatLine> = overlay
            .iter()
            .filter(|line| {
                self.filter_engine.is_visible_text(&line.raw) && self.severity.allows(line.level)
            })
            .cloned()
            .collect();
        if self.overlay_len > 0 {
            let keep = self.flat_lines.len().saturating_sub(self.overlay_len);
            self.flat_lines.truncate(keep);
            self.overlay_len = 0;
            // Overlay content mutates in place (progress lines, rewrites), so
            // previously scanned matches may be stale; while a search is
            // active, only a full rescan gives correct results.
            if self.search_scan_end > self.flat_lines.len() || !self.search_query.is_empty() {
                self.search_full_rescan = true;
                self.search_scan_end = 0;
            }
            self.search_dirty = true;
        }
        if filtered.is_empty() {
            return;
        }
        self.overlay_len = filtered.len();
        self.flat_lines.extend(filtered);
        if !self.search_query.is_empty() {
            self.search_dirty = true;
        }
    }

    fn compiled_pattern(&mut self) -> Option<SearchPattern> {
        if let Some(p) = self.search_pattern.clone() {
            return Some(p);
        }
        match compile_search_pattern(
            &self.search_query,
            self.search_regex,
            self.search_case_sensitive,
            self.search_whole_word,
        ) {
            Ok(p) => {
                self.search_error = None;
                self.search_pattern = Some(p.clone());
                Some(p)
            }
            Err(e) => {
                self.search_error = Some(e);
                self.search_matches.clear();
                self.search_pattern = None;
                self.search_match_index = 0;
                self.search_scan_end = 0;
                self.search_full_rescan = true;
                None
            }
        }
    }

    fn refresh_search_with_buffer(&mut self, buffer: &mut RecordBuffer) {
        if self.search_query.is_empty() {
            self.search_matches.clear();
            self.search_pattern = None;
            self.search_error = None;
            self.search_match_index = 0;
            self.search_scan_end = 0;
            self.search_full_rescan = true;
            self.search_scroll_pending = false;
            self.search_jump_to_last = false;
            return;
        }

        // Auto-expand collapsed Records whose match lies on a hidden line
        // (desktop parity: a match must never stay stranded).
        if let Some(pattern) = self.compiled_pattern() {
            let need = record_ids_needing_expand_for_search(
                buffer.records(),
                &self.filter_engine,
                self.severity,
                &self.expanded_record_ids,
                &pattern,
            );
            if !need.is_empty() {
                for id in need {
                    self.expanded_record_ids.insert(id);
                }
                self.flat_lines = rebuild_flat_lines(
                    buffer,
                    &self.filter_engine,
                    self.severity,
                    &self.expanded_record_ids,
                );
                self.flat_lines_record_cursor = buffer.records_len();
                self.overlay_len = 0;
                self.last_dropped = buffer.dropped_count();
                self.search_full_rescan = true;
                self.search_scan_end = 0;
            }
        }
        self.refresh_search();
    }

    fn refresh_search(&mut self) {
        let Some(pattern) = self.compiled_pattern() else {
            return;
        };

        if self.search_full_rescan {
            self.search_matches = collect_search_matches(&self.flat_lines, &pattern);
            self.search_scan_end = self.flat_lines.len();
            self.search_full_rescan = false;
            if self.search_jump_to_last {
                self.search_jump_to_last = false;
                self.search_match_index = self.search_matches.len().saturating_sub(1);
            } else if self.search_match_index >= self.search_matches.len() {
                self.search_match_index = self.search_matches.len().saturating_sub(1);
            }
        } else if self.search_scan_end < self.flat_lines.len() {
            let offset = self.search_scan_end;
            append_search_matches(
                &mut self.search_matches,
                &self.flat_lines[offset..],
                offset,
                &pattern,
            );
            self.search_scan_end = self.flat_lines.len();
            if self.search_jump_to_last {
                self.search_jump_to_last = false;
                self.search_match_index = self.search_matches.len().saturating_sub(1);
            }
        } else if self.search_jump_to_last {
            self.search_jump_to_last = false;
            self.search_match_index = self.search_matches.len().saturating_sub(1);
        }

        if self.search_scroll_pending {
            self.search_scroll_pending = false;
            if self.search_active_line().is_some() {
                self.scroll_request += 1;
            }
        }
    }

    /// The compiled pattern plus the active match range on `line_index`, for
    /// snapshot highlighting (`None` = no highlighting on this line).
    pub(crate) fn search_highlight_for(
        &self,
        line_index: usize,
    ) -> Option<(&SearchPattern, Option<(usize, usize)>)> {
        let pattern = self.search_pattern.as_ref()?;
        if self.search_error.is_some() {
            return None;
        }
        let active = self
            .search_matches
            .get(self.search_match_index)
            .filter(|m| m.line_index == line_index)
            .map(|m| (m.start, m.end));
        Some((pattern, active))
    }
}

/// One Terminal (PTY command or log file) with its Tab/Views.
pub struct Session {
    pub id: u32,
    pub name: String,
    pub source: SessionSource,
    /// Bumped by every content change that is not a pure append; hosts use
    /// it to decide between incremental appends and full snapshots.
    pub epoch: u32,
    pub ingest: TerminalIngest,
    pub buffer: RecordBuffer,
    pub parser: RecordParser,
    pub views: Vec<SessionView>,
    pub active_view: usize,
    closed_tabs: Vec<TabConfig>,
    pub finished: bool,
    /// `buffer.dropped_count()` at the last refresh — the ring trims inside
    /// ingest, before `refresh_active` can observe it, so the session
    /// remembers the previous value to detect shifts (epoch invalidation).
    last_seen_dropped: usize,
    /// Last invalid-regex notice from a filter edit on the active tab; the
    /// snapshot carries it as `view.notice` (cleared on tab changes).
    pub filter_notice: Option<String>,
}

impl Session {
    /// Default ring capacity, shared with the desktop config default.
    pub const DEFAULT_MAX_RECORDS: usize = DEFAULT_MAX_SCROLLBACK_LINES;

    pub fn new(id: u32, name: String, source: SessionSource, max_records: usize) -> Self {
        Self {
            id,
            name,
            source,
            epoch: 0,
            ingest: TerminalIngest::new(),
            buffer: RecordBuffer::new(max_records),
            parser: RecordParser::new(noviewlog_terminal::formats::get_builtin_format(
                "node-default",
            )),
            views: vec![SessionView::new_terminal()],
            active_view: 0,
            closed_tabs: Vec::new(),
            finished: false,
            last_seen_dropped: 0,
            filter_notice: None,
        }
    }

    pub fn active(&self) -> &SessionView {
        &self.views[self.active_view]
    }

    pub fn active_mut(&mut self) -> &mut SessionView {
        let idx = self.active_view;
        &mut self.views[idx]
    }

    pub(crate) fn touch(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Ingest raw bytes (PTY chunk or file chunk) and refresh the active view.
    pub fn feed(&mut self, bytes: &[u8]) {
        {
            let ingest = &mut self.ingest;
            let buffer = &mut self.buffer;
            let parser = &mut self.parser;
            ingest.feed(bytes, buffer, parser);
        }
        self.refresh_active();
    }

    /// Flush a still-pending parser record (host idle tick).
    pub fn flush_pending(&mut self) -> bool {
        let flushed = {
            let ingest = &mut self.ingest;
            let buffer = &mut self.buffer;
            let parser = &mut self.parser;
            ingest.idle_flush(buffer, parser)
        };
        if flushed {
            self.refresh_active();
        }
        flushed
    }

    /// Finalize at process exit / end of file: commit the whole screen.
    pub fn finish(&mut self) {
        {
            let ingest = &mut self.ingest;
            let buffer = &mut self.buffer;
            let parser = &mut self.parser;
            ingest.finish(buffer, parser);
        }
        self.finished = true;
        self.refresh_active();
    }

    /// Match emulator geometry to the viewport / PTY size.
    pub fn resize(&mut self, cols: usize, rows: usize) {
        {
            let ingest = &mut self.ingest;
            let buffer = &mut self.buffer;
            let parser = &mut self.parser;
            ingest.resize(cols, rows, buffer, parser);
        }
        self.refresh_active();
    }

    /// Refresh the active view: committed flat lines + Terminal-tab overlay.
    /// A ring shift (scrollback trim) touches the epoch — every flat-line
    /// index moves, so pending incremental appends must be invalidated.
    pub fn refresh_active(&mut self) {
        let idx = self.active_view;
        // Desktop parity (engine/mod.rs): the active view — Terminal tab or
        // a filter tab — shows the live VT tail, filtered per view.
        let mut overlay = if self.finished {
            // After finish the screen is flushed.
            Vec::new()
        } else {
            self.ingest.overlay_flat_lines()
        };
        if idx == 0 {
            // Blank grid rows below the last output are caret space on the
            // desktop, not log lines — the webview has no caret to draw.
            while overlay.last().is_some_and(|l| l.raw.is_empty()) {
                overlay.pop();
            }
        }
        self.views[idx].refresh(&mut self.buffer, &overlay);
        if self.buffer.dropped_count() != self.last_seen_dropped {
            self.last_seen_dropped = self.buffer.dropped_count();
            self.touch();
        }
    }

    // ----- commands -----

    pub fn set_severity(&mut self, mode: SeverityFilter) {
        self.active_mut().set_severity(mode);
        self.touch();
        self.refresh_active();
    }

    pub fn set_follow(&mut self, on: bool) {
        self.active_mut().auto_follow = on;
        self.touch();
    }

    pub fn set_wrap(&mut self, on: bool) {
        self.active_mut().wrap_lines = on;
        self.touch();
    }

    pub fn toggle_collapse(&mut self, record_id: u64) {
        self.active_mut().toggle_record_collapse(record_id);
        self.touch();
        self.refresh_active();
    }

    pub fn expand_all(&mut self) {
        let idx = self.active_view;
        self.views[idx].expand_all_multiline(&mut self.buffer);
        self.touch();
        self.refresh_active();
    }

    pub fn collapse_all(&mut self) {
        self.active_mut().collapse_all_multiline();
        self.touch();
        self.refresh_active();
    }

    pub fn search_set(
        &mut self,
        query: String,
        regex: bool,
        case_sensitive: bool,
        whole_word: bool,
    ) {
        self.active_mut()
            .set_search(query, regex, case_sensitive, whole_word);
        self.touch();
        self.refresh_active();
    }

    pub fn search_navigate(&mut self, delta: i64) {
        self.active_mut().search_navigate(delta);
        self.refresh_active();
    }

    /// Replace the active (non-Terminal) view's filter rules. The Terminal
    /// tab refuses filter edits, as on the desktop. A fallback notice from
    /// an invalid regex is kept for the next snapshot.
    pub fn filter_set(&mut self, rules: Vec<FilterRule>) -> Result<Option<String>, String> {
        if self.active_view == 0 {
            return Err("the Terminal tab is not filter-editable".to_string());
        }
        let notice = self.active_mut().set_filters(rules);
        self.filter_notice = notice.clone();
        self.touch();
        self.refresh_active();
        Ok(notice)
    }

    pub fn tab_add(&mut self) {
        let name = format!("Tab {}", self.views.len() + 1);
        // Desktop parity: new filter tabs of file sessions start pinned to
        // the file head, not the (static) tail.
        let auto_follow = self.source != SessionSource::File;
        self.views.push(SessionView::from_tab_config(TabConfig {
            name,
            filters: Vec::new(),
            search_query: String::new(),
            search_regex: false,
            search_case_sensitive: false,
            search_whole_word: false,
            auto_follow,
            wrap_lines: true,
            severity: SeverityFilter::default(),
        }));
        self.active_view = self.views.len() - 1;
        self.filter_notice = None;
        self.touch();
        self.refresh_active();
    }

    pub fn tab_close(&mut self, index: usize) -> Result<(), String> {
        if index == 0 {
            return Err("the Terminal tab cannot be closed".to_string());
        }
        if index >= self.views.len() {
            return Err(format!("no tab at index {index}"));
        }
        let removed = self.views.remove(index);
        self.closed_tabs.push(removed.to_tab_config());
        // Desktop parity: keep the restore stack bounded.
        while self.closed_tabs.len() > MAX_CLOSED_TABS {
            self.closed_tabs.remove(0);
        }
        if self.active_view >= self.views.len() {
            self.active_view = self.views.len() - 1;
        } else if self.active_view > index {
            self.active_view -= 1;
        }
        self.filter_notice = None;
        self.touch();
        self.refresh_active();
        Ok(())
    }

    pub fn tab_switch(&mut self, index: usize) -> Result<(), String> {
        if index >= self.views.len() {
            return Err(format!("no tab at index {index}"));
        }
        if index != self.active_view {
            self.active_view = index;
            self.filter_notice = None;
            self.touch();
        }
        self.refresh_active();
        Ok(())
    }

    pub fn tab_rename(&mut self, index: usize, name: String) -> Result<(), String> {
        if index == 0 {
            return Err("the Terminal tab cannot be renamed".to_string());
        }
        let Some(view) = self.views.get_mut(index) else {
            return Err(format!("no tab at index {index}"));
        };
        let trimmed = name.trim().to_string();
        if trimmed.is_empty() {
            return Err("tab name must not be empty".to_string());
        }
        view.name = trimmed;
        self.touch();
        Ok(())
    }

    pub fn tab_restore(&mut self) -> Result<(), String> {
        let Some(tab) = self.closed_tabs.pop() else {
            return Err("no closed tab to restore".to_string());
        };
        self.views.push(SessionView::from_tab_config(tab));
        self.active_view = self.views.len() - 1;
        self.filter_notice = None;
        self.touch();
        self.refresh_active();
        Ok(())
    }

    pub fn can_restore_tab(&self) -> bool {
        !self.closed_tabs.is_empty()
    }

    /// Full tab configuration (names, filter rules, severity, wrap) for
    /// workspace persistence.
    pub fn tabs_to_config(&self) -> Vec<TabConfig> {
        self.views.iter().map(|v| v.to_tab_config()).collect()
    }

    /// Replace all tabs with `tabs` (workspace persistence restore); at
    /// least the Terminal tab must remain. Marks the buffer changed so the
    /// host publishes a fresh snapshot.
    pub fn tabs_apply_config(&mut self, tabs: Vec<TabConfig>, active: usize) -> Result<(), String> {
        if tabs.is_empty() {
            return Err("tab config must keep at least the Terminal tab".to_string());
        }
        // Index 0 is the Terminal tab by position everywhere in this crate
        // (live overlay, filter lock, close/rename refusal), so reject a
        // config that would put anything else there.
        if tabs[0].name != TERMINAL_TAB_NAME || !tabs[0].filters.is_empty() {
            return Err("tab config must start with the Terminal tab".to_string());
        }
        self.views = tabs.into_iter().map(SessionView::from_tab_config).collect();
        self.active_view = active.min(self.views.len() - 1);
        self.filter_notice = None;
        self.touch();
        self.refresh_active();
        Ok(())
    }
}

#[cfg(test)]
mod tests;
