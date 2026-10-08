//! Semantic tests ported from `noviewlog-core/src/tests/` plus facade-level
//! ingest/snapshot/epoch cases. They pin the wasm facade to the desktop
//! pipeline behavior (parser grouping, filter order, collapse, search).

use crate::snapshot::{session_append_since, session_snapshot, LineDto, SessionSnapshot};

use super::*;

/// A 2-row grid so the first LF commits the top line — no long preamble
/// needed before lines reach the Record buffer.
fn pty_session(id: u32) -> Session {
    pty_session_rows(id, 2)
}

fn pty_session_rows(id: u32, rows: u16) -> Session {
    let mut session = Session::new(id, "test".into(), SessionSource::Pty, 10_000);
    session.ingest = TerminalIngest::new_with_size(80, rows as usize);
    session
}

fn file_session(id: u32) -> Session {
    Session::new(id, "app.log".into(), SessionSource::File, 10_000)
}

fn feed_committed(session: &mut Session, lines: &[&str]) {
    // One chunk per call, like a real PTY read: Record grouping happens
    // within a chunk (each commit boundary flushes parser pending, so a
    // multiline Record split across chunks would split — desktop parity).
    let mut bytes = Vec::new();
    for line in lines {
        bytes.extend_from_slice(line.as_bytes());
        bytes.extend_from_slice(b"\r\n");
    }
    session.feed(&bytes);
    // Commit the last screen line (desktop finish-at-exit semantics).
    session.finish();
}

fn line_text(line: &LineDto) -> String {
    line.segments.iter().map(|s| s.text.as_str()).collect()
}

fn texts(snapshot: &SessionSnapshot) -> Vec<String> {
    snapshot.view.lines.iter().map(line_text).collect()
}

fn rules_from(json: &str) -> Vec<FilterRule> {
    serde_json::from_str(json).expect("valid filter rule JSON")
}

// ----- ingest / records -----

#[test]
fn pty_bytes_become_visible_records() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["one", "two", "three"]);
    let snapshot = session_snapshot(&mut session);
    assert_eq!(texts(&snapshot), vec!["one", "two", "three"]);
    assert_eq!(snapshot.buffer_records, 3);
    assert_eq!(snapshot.source, "pty");
}

#[test]
fn stack_trace_groups_into_single_collapsed_record() {
    // 4 rows: all three lines stay on screen until finish, so the whole
    // stack flushes in one commit batch and groups into a single Record.
    let mut session = pty_session_rows(1, 4);
    feed_committed(
        &mut session,
        &[
            "Error: something failed",
            "    at Object.<anonymous> (/app/index.js:10:5)",
            "    at Module._compile (node:internal/modules/cjs/loader:1376:14)",
        ],
    );
    let snapshot = session_snapshot(&mut session);
    assert_eq!(
        snapshot.buffer_records, 1,
        "stack frames group into one Record"
    );
    assert_eq!(
        snapshot.view.lines.len(),
        1,
        "multiline Record defaults collapsed"
    );
    let line = &snapshot.view.lines[0];
    assert!(line.collapsed);
    assert!(line.collapsible);
    assert_eq!(line.hidden_line_count, 2);
    assert_eq!(line.level, Some("error"));
}

#[test]
fn file_session_streams_without_pty() {
    let mut session = file_session(1);
    session.feed(b"2024-01-01T10:00:00.000Z info: first\r\n");
    session.feed(b"2024-01-01T10:00:01.000Z info: second\r\n");
    session.finish();
    let snapshot = session_snapshot(&mut session);
    assert_eq!(snapshot.source, "file");
    assert_eq!(snapshot.buffer_records, 2);
    assert!(snapshot.finished);
}

#[test]
fn finish_commits_last_screen_line() {
    let mut session = pty_session(1);
    session.feed(b"only line\r\n");
    let before = session_snapshot(&mut session);
    assert_eq!(before.buffer_records, 0, "line still lives on the grid");
    assert!(
        before
            .view
            .lines
            .iter()
            .any(|l| line_text(l).contains("only line")),
        "live screen line shows as overlay"
    );
    session.finish();
    let after = session_snapshot(&mut session);
    assert_eq!(after.buffer_records, 1);
    assert_eq!(texts(&after), vec!["only line"]);
}

#[test]
fn second_session_is_isolated() {
    let mut a = pty_session(1);
    let mut b = pty_session(2);
    feed_committed(&mut a, &["alpha"]);
    feed_committed(&mut b, &["beta"]);
    let snap_a = session_snapshot(&mut a);
    let snap_b = session_snapshot(&mut b);
    assert_eq!(texts(&snap_a), vec!["alpha"]);
    assert_eq!(texts(&snap_b), vec!["beta"]);
}

#[test]
fn resize_commits_scrolled_lines() {
    let mut session = pty_session(1);
    session.feed(b"first\r\nsecond\r\n");
    session.resize(80, 1);
    session.finish();
    let snapshot = session_snapshot(&mut session);
    assert!(
        texts(&snapshot).contains(&"first".to_string()),
        "narrowed grid committed earlier lines: {:?}",
        texts(&snapshot)
    );
}

// ----- filter pipeline -----

#[test]
fn severity_narrows_after_include_exclude() {
    let mut session = pty_session(1);
    session.tab_add();
    session
        .filter_set(rules_from(
            r#"[
                {"id":"i","type":"include","pattern":"Error|info","use_regex":true},
                {"id":"e","type":"exclude","pattern":"deprecated","use_regex":true}
            ]"#,
        ))
        .expect("filter_set");
    feed_committed(
        &mut session,
        &["warn: deprecated", "Error: boom", "info: ok"],
    );

    let all = session_snapshot(&mut session);
    assert_eq!(texts(&all), vec!["Error: boom", "info: ok"]);

    session.set_severity(SeverityFilter::Error);
    let errors = session_snapshot(&mut session);
    assert_eq!(texts(&errors), vec!["Error: boom"]);
    assert_eq!(errors.view.severity, "error");
}

#[test]
fn severity_errors_and_unleveled() {
    let mut session = pty_session(1);
    feed_committed(
        &mut session,
        &[
            "Error: boom",
            "warn: deprecated",
            "info: ok",
            "plain output",
        ],
    );

    session.set_severity(SeverityFilter::Error);
    let errors = session_snapshot(&mut session);
    assert_eq!(texts(&errors), vec!["Error: boom"]);

    session.set_severity(SeverityFilter::Unleveled);
    let unleveled = session_snapshot(&mut session);
    assert_eq!(texts(&unleveled), vec!["plain output"]);
    assert!(unleveled.view.lines[0].level.is_none());
}

#[test]
fn literal_filter_does_not_treat_dot_as_wildcard() {
    let mut session = pty_session(1);
    session.tab_add();
    session
        .filter_set(rules_from(
            r#"[{"id":"lit","type":"include","pattern":"foo.bar","use_regex":false}]"#,
        ))
        .expect("filter_set");
    feed_committed(&mut session, &["foo.bar", "fooxbar"]);
    let snapshot = session_snapshot(&mut session);
    assert_eq!(texts(&snapshot), vec!["foo.bar"]);
}

#[test]
fn exclude_wins_over_include() {
    let mut session = pty_session(1);
    session.tab_add();
    session
        .filter_set(rules_from(
            r#"[
                {"id":"i","type":"include","pattern":"Error|warn","use_regex":true},
                {"id":"e","type":"exclude","pattern":"deprecated","use_regex":true}
            ]"#,
        ))
        .expect("filter_set");
    feed_committed(
        &mut session,
        &["warn: deprecated", "Error: boom", "info: ok"],
    );
    let snapshot = session_snapshot(&mut session);
    assert_eq!(texts(&snapshot), vec!["Error: boom"]);
}

#[test]
fn invalid_regex_filter_surfaces_literal_fallback_notice() {
    let mut session = pty_session(1);
    session.tab_add();
    let notice = session
        .filter_set(rules_from(
            r#"[{"id":"bad","type":"include","pattern":"[[error","use_regex":true}]"#,
        ))
        .expect("filter_set accepted with fallback");
    let notice = notice.expect("invalid regex produces a notice");
    assert!(notice.contains("Invalid regex"), "notice: {notice}");

    feed_committed(&mut session, &["[[error hits", "unrelated"]);
    let snapshot = session_snapshot(&mut session);
    assert_eq!(
        texts(&snapshot),
        vec!["[[error hits"],
        "pattern matches literally after fallback"
    );
}

#[test]
fn snapshot_carries_filter_notice_until_it_resolves_or_tab_switches() {
    let mut session = pty_session(1);
    session.tab_add();
    session
        .filter_set(rules_from(
            r#"[{"id":"bad","type":"include","pattern":"([[error","use_regex":true}]"#,
        ))
        .expect("filter_set accepted with fallback");
    let notice = session_snapshot(&mut session)
        .view
        .notice
        .expect("snapshot carries the invalid-regex notice");
    assert!(notice.contains("Invalid regex"), "notice: {notice}");

    // A valid edit replaces the stale notice.
    session
        .filter_set(rules_from(
            r#"[{"id":"good","type":"include","pattern":"error","use_regex":false}]"#,
        ))
        .expect("valid filter_set");
    assert!(session_snapshot(&mut session).view.notice.is_none());

    // A tab switch drops the notice of the previous tab.
    session
        .filter_set(rules_from(
            r#"[{"id":"bad","type":"include","pattern":"([[error","use_regex":true}]"#,
        ))
        .expect("filter_set accepted with fallback");
    session.tab_switch(0).expect("switch to Terminal");
    assert!(session_snapshot(&mut session).view.notice.is_none());
}

#[test]
fn terminal_tab_refuses_filters_and_edits() {
    let mut session = pty_session(1);
    let err = session
        .filter_set(rules_from("[]"))
        .expect_err("Terminal tab refuses filters");
    assert!(err.contains("not filter-editable"));
    assert!(session.tab_rename(0, "Shell".into()).is_err());
    assert!(session.tab_close(0).is_err());
    assert_eq!(session.views[0].name, TERMINAL_TAB_NAME);
}

#[test]
fn terminal_tab_shows_overlay_until_committed() {
    let mut session = pty_session(1);
    session.feed(b"[LOG] live-one\r\n");
    let live = session_snapshot(&mut session);
    assert_eq!(live.buffer_records, 0, "not committed yet");
    assert!(
        texts(&live).iter().any(|t| t.contains("live-one")),
        "overlay line visible on Terminal tab"
    );

    // Desktop parity (engine/mod.rs): the active filter tab also shows the
    // live overlay, filtered by its rules (none here — full stream).
    session.tab_add();
    session.feed(b"[LOG] live-two\r\n");
    let filter_tab = session_snapshot(&mut session);
    assert!(
        texts(&filter_tab).iter().any(|t| t.contains("live-two")),
        "filter tab shows the filtered live overlay"
    );

    session.tab_switch(0).expect("tab_switch");
    session.finish();
    let committed = session_snapshot(&mut session);
    assert!(
        texts(&committed).iter().any(|t| t.contains("live-two")),
        "committed line visible on Terminal tab"
    );
}

// ----- collapse -----

#[test]
fn collapse_toggle_and_expand_collapse_all() {
    let mut session = pty_session(1);
    feed_committed(
        &mut session,
        &[
            "Error: boom",
            "    at foo.js:1:1",
            "    at bar.js:2:2",
            "info: done",
        ],
    );
    let collapsed = session_snapshot(&mut session);
    assert_eq!(collapsed.view.lines.len(), 2, "collapsed stack + info line");

    let record_id = collapsed.view.lines[0].record_id;
    session.toggle_collapse(u64::from(record_id));
    let expanded = session_snapshot(&mut session);
    assert_eq!(expanded.view.lines.len(), 4, "stack fully expanded");
    assert!(!expanded.view.lines[0].collapsed);
    assert!(expanded.view.lines[0].collapsible);

    session.collapse_all();
    let again = session_snapshot(&mut session);
    assert_eq!(again.view.lines.len(), collapsed.view.lines.len());

    session.expand_all();
    let all_open = session_snapshot(&mut session);
    assert_eq!(all_open.view.lines.len(), 4);
}

#[test]
fn collapse_respects_exclude_on_full_text() {
    let mut session = pty_session_rows(1, 4);
    session.tab_add();
    session
        .filter_set(rules_from(
            r#"[{"id":"e","type":"exclude","pattern":"deprecated","use_regex":true}]"#,
        ))
        .expect("filter_set");
    feed_committed(&mut session, &["Error: boom", "    at deprecated.js:1"]);
    let snapshot = session_snapshot(&mut session);
    assert!(
        texts(&snapshot).is_empty(),
        "exclude matches full Record text even when the hit is a hidden line"
    );
}

// ----- search -----

#[test]
fn search_literal_is_case_insensitive() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["Error: BOOM", "info: ok"]);
    session.search_set("boom".into(), false, false, false);
    let snapshot = session_snapshot(&mut session);
    assert_eq!(snapshot.view.search.match_count, 1);
    assert_eq!(snapshot.view.search.label, "1/1");
    assert_eq!(snapshot.view.search.active_line, Some(0));
    assert_eq!(snapshot.view.search.scroll_request, 1, "scroll requested");
}

#[test]
fn search_case_sensitive_and_whole_word() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["Error: BOOM boom", "err error err"]);
    session.search_set("boom".into(), false, true, false);
    let snapshot = session_snapshot(&mut session);
    assert_eq!(
        snapshot.view.search.match_count, 1,
        "case-sensitive skips the uppercase BOOM"
    );

    session.search_set("err".into(), false, false, true);
    let snapshot = session_snapshot(&mut session);
    assert_eq!(
        snapshot.view.search.match_count, 2,
        "whole-word matches standalone err, not the prefix in Error"
    );
}

#[test]
fn search_regex_mode_matches_pattern() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["GET /api/users 200"]);
    session.search_set(r"GET /api/\w+".into(), true, false, false);
    let snapshot = session_snapshot(&mut session);
    assert_eq!(snapshot.view.search.match_count, 1);
}

#[test]
fn search_invalid_regex_surfaces_error() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["some line"]);
    session.search_set("[unclosed".into(), true, false, false);
    let snapshot = session_snapshot(&mut session);
    assert!(snapshot.view.search.error.is_some());
    assert_eq!(snapshot.view.search.match_count, 0);
    assert!(snapshot.view.search.label.is_empty());
}

#[test]
fn search_navigates_with_wrap_and_requests_scroll() {
    let mut session = pty_session(1);
    feed_committed(
        &mut session,
        &["hit-one unique-token", "nope", "hit-two unique-token"],
    );
    session.search_set("unique-token".into(), false, false, false);
    let snapshot = session_snapshot(&mut session);
    assert_eq!(snapshot.view.search.match_count, 2);
    assert_eq!(
        snapshot.view.search.active_line,
        Some(2),
        "new search jumps to the last match"
    );

    session.search_navigate(-1);
    let prev = session_snapshot(&mut session);
    assert_eq!(prev.view.search.active_line, Some(0));
    assert_eq!(prev.view.search.label, "1/2");

    session.search_navigate(-1);
    let wrapped = session_snapshot(&mut session);
    assert_eq!(wrapped.view.search.active_line, Some(2), "wraps around");
    assert!(wrapped.view.search.scroll_request > snapshot.view.search.scroll_request);
}

#[test]
fn search_highlights_segments_in_snapshot() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["foo bar foo"]);
    session.search_set("foo".into(), false, false, false);
    let snapshot = session_snapshot(&mut session);
    let highlighted: Vec<&crate::snapshot::SegDto> = snapshot.view.lines[0]
        .segments
        .iter()
        .filter(|s| s.search || s.search_current)
        .collect();
    assert_eq!(highlighted.len(), 2);
    assert!(
        highlighted.iter().any(|s| s.search_current),
        "active match flagged"
    );
}

#[test]
fn search_hidden_line_expands_collapsed_record() {
    let mut session = pty_session_rows(1, 4);
    feed_committed(
        &mut session,
        &["Error: boom", "    at foo.js:1:1", "    at bar.js:2:2"],
    );
    session.search_set("bar.js".into(), false, false, false);
    let snapshot = session_snapshot(&mut session);
    assert_eq!(
        snapshot.view.lines.len(),
        3,
        "match on a hidden line expands the Record"
    );
}

#[test]
fn search_set_empty_clears_matches() {
    let mut session = pty_session(1);
    feed_committed(
        &mut session,
        &["hit-one unique-token", "hit-two unique-token"],
    );
    session.search_set("unique-token".into(), false, false, false);
    session.set_follow(true);
    session.search_set(String::new(), false, false, false);
    let snapshot = session_snapshot(&mut session);
    assert!(snapshot.view.search.query.is_empty());
    assert_eq!(snapshot.view.search.match_count, 0);
    assert!(
        snapshot.view.follow,
        "clearing search must not turn Follow off"
    );
}

// ----- snapshots: epoch / append -----

#[test]
fn append_since_returns_only_new_lines_after_pure_ingest() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["one"]);
    let full = session_snapshot(&mut session);
    assert_eq!(full.view.total_lines, 1);

    session.feed(b"two\r\n");
    session.finish();
    let append = session_append_since(&mut session, full.view.epoch, full.view.total_lines);
    assert!(append.ok);
    assert_eq!(append.base, 1);
    assert_eq!(append.total_lines, 2);
    assert_eq!(append.lines.len(), 1);
    assert_eq!(line_text(&append.lines[0]), "two");
}

#[test]
fn append_since_rejected_after_content_change() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["Error: boom", "info: ok"]);
    let full = session_snapshot(&mut session);

    session.set_severity(SeverityFilter::Error);
    let append = session_append_since(&mut session, full.view.epoch, full.view.total_lines);
    assert!(!append.ok, "severity change must invalidate the append");
    assert!(append.lines.is_empty());
}

#[test]
fn append_since_rejected_after_ring_shift() {
    let mut session = Session::new(1, "tiny".into(), SessionSource::Pty, 2);
    session.ingest = TerminalIngest::new_with_size(80, 2);
    feed_committed(&mut session, &["a", "b"]);
    let full = session_snapshot(&mut session);
    assert_eq!(full.view.total_lines, 2);

    feed_committed(&mut session, &["c"]);
    let append = session_append_since(&mut session, full.view.epoch, full.view.total_lines);
    assert!(
        !append.ok,
        "ring shift moves every line index and must invalidate appends"
    );
    let refreshed = session_snapshot(&mut session);
    assert_eq!(refreshed.view.total_lines, 2);
    assert_eq!(refreshed.dropped_records, 1);
}

#[test]
fn append_since_rejected_for_stale_base() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["one"]);
    let full = session_snapshot(&mut session);
    let append = session_append_since(&mut session, full.view.epoch, 99);
    assert!(!append.ok);
}

// ----- tabs -----

#[test]
fn tab_lifecycle_add_close_restore() {
    let mut session = pty_session(1);
    assert_eq!(session.views.len(), 1);

    session.tab_add();
    assert_eq!(session.views[1].name, "Tab 2");
    session.tab_rename(1, "Errors".into()).expect("rename");
    session
        .filter_set(rules_from(
            r#"[{"id":"i","type":"include","pattern":"Error","use_regex":true}]"#,
        ))
        .expect("filter_set");
    session.search_set("boom".into(), false, false, false);

    session.tab_close(1).expect("close");
    assert_eq!(session.views.len(), 1);
    assert!(session.can_restore_tab());

    session.tab_restore().expect("restore");
    assert_eq!(session.views.len(), 2);
    assert_eq!(session.active_view, 1);
    assert_eq!(session.views[1].name, "Errors");
    assert_eq!(session.views[1].filters().len(), 1);
    assert_eq!(session.views[1].search_query, "boom");
    assert!(!session.can_restore_tab());
}

#[test]
fn inactive_tab_catches_up_on_switch() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["one"]);
    let terminal_lines = session_snapshot(&mut session).view.total_lines;

    session.tab_add();
    session.search_set("nothing-here".into(), false, false, false);
    feed_committed(&mut session, &["two", "three"]);

    session.tab_switch(0).expect("tab_switch");
    let snapshot = session_snapshot(&mut session);
    assert_eq!(
        snapshot.view.total_lines,
        terminal_lines + 2,
        "Terminal tab catches up after being inactive"
    );
}

#[test]
fn tab_switch_touches_epoch_for_append_invalidation() {
    let mut session = pty_session(1);
    feed_committed(&mut session, &["one"]);
    session.tab_add();
    let full = session_snapshot(&mut session);

    session.tab_switch(0).expect("tab_switch");
    session.tab_switch(1).expect("tab_switch");
    let append = session_append_since(&mut session, full.view.epoch, full.view.total_lines);
    assert!(!append.ok, "tab switches change the visible line set");
}

#[test]
fn filter_add_regex_false_and_omitted_default() {
    let rules: Vec<FilterRule> = serde_json::from_str(
        r#"[
            {"id":"a","type":"include","pattern":"foo.bar","use_regex":false},
            {"id":"b","type":"exclude","pattern":"warn"}
        ]"#,
    )
    .expect("rules");
    assert!(!rules[0].use_regex);
    assert!(rules[1].use_regex, "omitted use_regex defaults to true");
}

#[test]
fn search_tracks_in_place_overlay_rewrites() {
    // Progress lines rewrite the screen in place; a live search must see
    // the new text even though no record was committed.
    let mut session = pty_session(1);
    session.feed(b"progress 10%\r\n");
    session.search_set("90%".into(), false, false, false);
    let before = session_snapshot(&mut session);
    assert_eq!(before.view.search.match_count, 0, "no 90% yet");

    session.feed(b"\rprogress 90%");
    let after = session_snapshot(&mut session);
    assert_eq!(
        after.view.search.match_count, 1,
        "in-place overlay rewrite is picked up"
    );
}
