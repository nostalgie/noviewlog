//! Search and filter rules: find bar, filter draft preview, include/exclude rule CRUD.

use super::*;

impl Engine {
    pub(crate) fn search_set(
        &mut self,
        query: &str,
        regex: bool,
        case_sensitive: bool,
        whole_word: bool,
    ) {
        let view = self.active_view_mut();
        // Identical query: do not mark search dirty. Hybrid UI flushes SearchSet
        // before every next/prev; re-marking would set search_jump_to_last and
        // reset the match index to the last hit on the next rebuild.
        if view.search_query == query
            && view.search_regex == regex
            && view.search_case_sensitive == case_sensitive
            && view.search_whole_word == whole_word
        {
            return;
        }
        view.search_query = query.to_string();
        view.search_regex = regex;
        view.search_case_sensitive = case_sensitive;
        view.search_whole_word = whole_word;
        view.mark_search_changed();
        let need_scrollback = !query.is_empty();
        self.mark_viewport_dirty();
        self.last_stats_at = None;
        if need_scrollback {
            self.materialize_live_terminal_tab();
        }
        // Persist the search query with the workspace snapshot (#109); the
        // debounce collapses repeated keystrokes into one write.
        self.sync_active_project_from_terminals();
    }

    pub(crate) fn filter_draft_set(&mut self, pattern: &str, use_regex: bool) {
        if self.filter_draft_query == pattern && self.filter_draft_regex == use_regex {
            return;
        }
        self.filter_draft_query = pattern.to_string();
        self.filter_draft_regex = use_regex;
        self.filter_draft_pattern =
            crate::core::visible::compile_filter_draft_pattern(pattern, use_regex);
        self.mark_viewport_dirty();
    }

    pub(crate) fn search_goto(&mut self, delta: i32) {
        let scroll_row = {
            let terminal = self.active_terminal_mut();
            let view = &mut terminal.views[terminal.active_view];
            let n = view.search_matches.len();
            if n == 0 {
                return;
            }
            if delta < 0 {
                view.search_match_index = (view.search_match_index + n - 1) % n;
            } else {
                view.search_match_index = (view.search_match_index + 1) % n;
            }
            view.search_matches
                .get(view.search_match_index)
                .map(|m| m.line_index)
        };
        if let Some(row) = scroll_row {
            self.active_terminal_mut().scroll_to_row = Some(row);
            // Must dirty: UI only re-renders when needs_render(), and
            // scroll_to_row is applied inside render().
            self.mark_viewport_dirty();
            self.last_stats_at = None;
        }
    }

    pub(crate) fn add_filter(&mut self, filter_type: FilterType, pattern: &str, use_regex: bool) {
        if pattern.is_empty() || self.active_terminal().active_view == 0 {
            return;
        }
        let kind = match filter_type {
            FilterType::Include => "include",
            FilterType::Exclude => "exclude",
        };
        let next_id = next_filter_id(self.active_view().filters(), kind);
        let (compiled, regex_notice) = compile_filter_checked(FilterRule {
            id: next_id,
            name: None,
            filter_type,
            pattern: pattern.to_string(),
            enabled: true,
            use_regex,
            regex: None,
        });
        self.active_view_mut().filters_mut().push(compiled);
        if let Some(notice) = regex_notice {
            self.status_message = notice;
            self.push_event(json!({"type":"status","message": self.status_message}));
        }
        self.reset_file_match_viewport();
        self.sync_active_project_from_terminals();
    }

    pub(crate) fn filter_toggle(&mut self, id: &str, enabled: bool) {
        if self.active_terminal().active_view == 0 {
            return;
        }
        if let Some(filter) = self
            .active_view_mut()
            .filters_mut()
            .iter_mut()
            .find(|f| f.id == id)
        {
            filter.enabled = enabled;
        }
        self.reset_file_match_viewport();
        self.sync_active_project_from_terminals();
    }

    pub(crate) fn filter_remove(&mut self, id: &str) {
        if self.active_terminal().active_view == 0 {
            return;
        }
        self.active_view_mut().filters_mut().retain(|f| f.id != id);
        self.reset_file_match_viewport();
        self.sync_active_project_from_terminals();
    }

    pub(crate) fn filter_update(&mut self, id: &str, pattern: &str) {
        if pattern.is_empty() || self.active_terminal().active_view == 0 {
            return;
        }
        let Some(existing) = self.active_view().filters().iter().find(|f| f.id == id) else {
            return;
        };
        if existing.pattern == pattern {
            return;
        }
        let mut rule = existing.clone();
        rule.pattern = pattern.to_string();
        rule.regex = None;
        let (compiled, regex_notice) = compile_filter_checked(rule);
        if let Some(slot) = self
            .active_view_mut()
            .filters_mut()
            .iter_mut()
            .find(|f| f.id == id)
        {
            *slot = compiled;
        }
        if let Some(notice) = regex_notice {
            self.status_message = notice;
            self.push_event(json!({"type":"status","message": self.status_message}));
        }
        self.reset_file_match_viewport();
        self.sync_active_project_from_terminals();
    }
}
