use crate::core::types::{compile_filter, FilterRule, FilterType};

#[cfg(unix)]
#[test]
fn workspace_key_uses_canonical_path() {
    use crate::core::config::workspace_key;
    use std::fs;
    use std::os::unix::fs::symlink;

    let base = std::env::temp_dir().join(format!("noviewlog-ws-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    fs::create_dir_all(&base).unwrap();
    let link = base.join("link");
    symlink(&base, &link).unwrap();

    let direct = workspace_key(Some(base.to_str().unwrap()));
    let via_link = workspace_key(Some(link.to_str().unwrap()));
    assert_eq!(direct, via_link);

    let _ = fs::remove_dir_all(&base);
}

#[test]
fn tab_move_reorders_filters_and_pins_terminal_tab() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add a");
    engine
        .send_command_json(r#"{"cmd":"tab_rename","index":1,"name":"A"}"#)
        .expect("rename a");
    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add b");
    engine
        .send_command_json(r#"{"cmd":"tab_rename","index":2,"name":"B"}"#)
        .expect("rename b");
    assert_eq!(engine.active_tab_index_for_test(), 2);

    // Terminal, A, B — move A (1) to index 2 → Terminal, B, A; active was B@2 → stays on B@1
    engine
        .send_command_json(r#"{"cmd":"tab_move","from_index":1,"to_index":2}"#)
        .expect("tab_move");
    let names: Vec<String> = engine
        .tab_configs_for_test()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, vec!["Terminal", "B", "A"]);
    assert_eq!(engine.active_tab_index_for_test(), 1);

    engine
        .send_command_json(r#"{"cmd":"tab_move","from_index":0,"to_index":2}"#)
        .expect("refuse terminal tab from");
    let names: Vec<String> = engine
        .tab_configs_for_test()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, vec!["Terminal", "B", "A"]);

    engine
        .send_command_json(r#"{"cmd":"tab_move","from_index":2,"to_index":0}"#)
        .expect("refuse terminal tab to");
    let names: Vec<String> = engine
        .tab_configs_for_test()
        .into_iter()
        .map(|t| t.name)
        .collect();
    assert_eq!(names, vec!["Terminal", "B", "A"]);
}

#[test]
fn tab_rename_rejects_terminal_tab_index_zero() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    let before = engine.tab_configs_for_test()[0].name.clone();
    assert_eq!(before, "Terminal");

    engine
        .send_command_json(r#"{"cmd":"tab_rename","index":0,"name":"Shell"}"#)
        .expect("tab_rename terminal tab");
    assert_eq!(engine.tab_configs_for_test()[0].name, before);

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(r#"{"cmd":"tab_rename","index":1,"name":"Errors"}"#)
        .expect("tab_rename filter");
    assert_eq!(engine.tab_configs_for_test()[1].name, "Errors");
    assert_eq!(engine.tab_configs_for_test()[0].name, "Terminal");
}

#[test]
fn tab_close_restore_preserves_settings() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    assert_eq!(engine.tab_configs_for_test().len(), 1);

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    assert_eq!(engine.tab_configs_for_test().len(), 2);
    assert_eq!(engine.active_tab_index_for_test(), 1);

    engine
        .send_command_json(r#"{"cmd":"filter_add","type":"include","pattern":"Error"}"#)
        .expect("filter_add");
    engine
        .send_command_json(r#"{"cmd":"search_set","query":"boom","regex":false}"#)
        .expect("search_set");
    engine
        .send_command_json(r#"{"cmd":"set_follow","follow":false}"#)
        .expect("set_follow");
    engine
        .send_command_json(r#"{"cmd":"tab_rename","index":1,"name":"Errors"}"#)
        .expect("tab_rename");

    let closed = engine.tab_configs_for_test()[1].clone();
    assert_eq!(closed.name, "Errors");
    assert_eq!(closed.search_query, "boom");
    assert!(!closed.auto_follow);
    assert_eq!(closed.filters.len(), 1);
    assert_eq!(closed.filters[0].pattern, "Error");

    engine
        .send_command_json(r#"{"cmd":"tab_close","index":1}"#)
        .expect("tab_close");
    assert_eq!(engine.tab_configs_for_test().len(), 1);
    assert!(engine.can_restore_closed_tab_for_test());

    engine
        .send_command_json(r#"{"cmd":"tab_restore"}"#)
        .expect("tab_restore");
    assert_eq!(engine.tab_configs_for_test().len(), 2);
    assert_eq!(engine.active_tab_index_for_test(), 1);
    assert!(!engine.can_restore_closed_tab_for_test());

    let restored = &engine.tab_configs_for_test()[1];
    assert_eq!(restored.name, closed.name);
    assert_eq!(restored.search_query, closed.search_query);
    assert_eq!(restored.search_regex, closed.search_regex);
    assert_eq!(restored.auto_follow, closed.auto_follow);
    assert_eq!(restored.filters.len(), closed.filters.len());
    assert_eq!(restored.filters[0].pattern, "Error");
    assert_eq!(
        restored.filters[0].filter_type,
        closed.filters[0].filter_type
    );
}

#[test]
fn tab_restore_noop_when_stack_empty() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"tab_restore"}"#)
        .expect("tab_restore");
    assert_eq!(engine.tab_configs_for_test().len(), 1);
    assert!(!engine.can_restore_closed_tab_for_test());
}

#[test]
fn inactive_tab_stays_stale_until_selected() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"set_format","format_id":"raw"}"#)
        .expect("set_format raw");
    // Raw format keeps the latest line pending until the next start line arrives.
    engine.push_lines_for_test(["line-a".into(), "line-b".into(), "line-c".into()]);
    let initial = engine.buffer_record_count_for_test();
    assert!(initial >= 2, "expected committed records, got {initial}");
    assert_eq!(engine.view_record_cursor_for_test(0), Some(initial));

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine.rebuild_if_needed_for_test();
    assert_eq!(engine.active_tab_index_for_test(), 1);
    assert_eq!(engine.view_record_cursor_for_test(1), Some(initial));
    let synced_lines = engine.view_flat_line_count_for_test(1).unwrap();

    engine
        .send_command_json(r#"{"cmd":"tab_switch","index":0}"#)
        .expect("tab_switch");
    engine.push_streaming_lines_for_test(["line-d".into(), "line-e".into(), "line-f".into()]);
    engine.rebuild_if_needed_for_test();

    let after = engine.buffer_record_count_for_test();
    assert!(after > initial);
    assert_eq!(engine.view_record_cursor_for_test(0), Some(after));
    // Inactive tab must not rebuild on the active tab's tick.
    assert_eq!(engine.view_record_cursor_for_test(1), Some(initial));
    assert_eq!(engine.view_flat_line_count_for_test(1), Some(synced_lines));

    engine
        .send_command_json(r#"{"cmd":"tab_switch","index":1}"#)
        .expect("tab_switch back");
    engine.rebuild_if_needed_for_test();
    assert_eq!(engine.view_record_cursor_for_test(1), Some(after));
    assert!(
        engine.view_flat_line_count_for_test(1).unwrap() > synced_lines,
        "selected tab should catch up missing records"
    );
}

#[test]
fn workspace_config_round_trip_yaml() {
    use crate::core::config::views_to_workspace;
    use crate::core::types::{TabConfig, WorkspaceConfig};

    let ws = views_to_workspace(
        &[TabConfig {
            name: "Errors".to_string(),
            filters: vec![compile_filter(FilterRule {
                id: "e".to_string(),
                name: None,
                filter_type: FilterType::Include,
                pattern: "Error".to_string(),
                enabled: true,
                use_regex: true,
                regex: None,
            })],
            search_query: "boom".to_string(),
            search_regex: false,
            search_case_sensitive: false,
            search_whole_word: false,
            auto_follow: false,
            wrap_lines: true,
            severity: Default::default(),
        }],
        0,
    );

    let yaml = serde_yaml::to_string(&ws).unwrap();
    let parsed: WorkspaceConfig = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(parsed.active_tab, 0);
    assert_eq!(parsed.tabs.len(), 1);
    assert_eq!(parsed.tabs[0].name, "Errors");
    assert_eq!(parsed.tabs[0].search_query, "boom");
    assert!(!parsed.tabs[0].auto_follow);
    assert_eq!(parsed.tabs[0].filters[0].pattern, "Error");
}

#[test]
fn search_literal_is_case_insensitive() {
    use crate::core::types::FlatLine;
    use crate::core::visible::{collect_search_matches, compile_search_pattern};

    let lines = vec![
        FlatLine {
            record_id: 1,
            line_index: 0,
            segments: vec![],
            raw: "Error: BOOM".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        },
        FlatLine {
            record_id: 2,
            line_index: 0,
            segments: vec![],
            raw: "info: ok".to_string(),
            level: None,
            collapsible: false,
            collapsed: false,
            hidden_line_count: 0,
        },
    ];
    let pattern = compile_search_pattern("boom", false, false, false).unwrap();
    let matches = collect_search_matches(&lines, &pattern);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].line_index, 0);
    assert_eq!(matches[0].start, 7);
}

#[test]
fn search_case_sensitive_excludes_mismatched_case() {
    use crate::core::types::FlatLine;
    use crate::core::visible::{collect_search_matches, compile_search_pattern};

    let lines = vec![FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![],
        raw: "Error: BOOM boom".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    }];
    let ci = compile_search_pattern("boom", false, false, false).unwrap();
    let cs = compile_search_pattern("boom", false, true, false).unwrap();
    assert_eq!(collect_search_matches(&lines, &ci).len(), 2);
    let cs_matches = collect_search_matches(&lines, &cs);
    assert_eq!(cs_matches.len(), 1);
    assert_eq!(cs_matches[0].start, 12);
}

#[test]
fn search_whole_word_excludes_substrings() {
    use crate::core::types::FlatLine;
    use crate::core::visible::{collect_search_matches, compile_search_pattern};

    let lines = vec![FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![],
        raw: "err error err".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    }];
    let any = compile_search_pattern("err", false, false, false).unwrap();
    let whole = compile_search_pattern("err", false, false, true).unwrap();
    assert_eq!(collect_search_matches(&lines, &any).len(), 3); // err, err in error, err
    let whole_matches = collect_search_matches(&lines, &whole);
    assert_eq!(whole_matches.len(), 2);
    assert_eq!(whole_matches[0].start, 0);
    assert_eq!(whole_matches[1].start, 10);
}

#[test]
fn search_set_persists_case_and_whole_word_flags() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(
            r#"{"cmd":"search_set","query":"err","regex":false,"case_sensitive":true,"whole_word":true}"#,
        )
        .expect("search_set");
    let cfg = engine.active_tab_config_for_test();
    assert_eq!(cfg.search_query, "err");
    assert!(cfg.search_case_sensitive);
    assert!(cfg.search_whole_word);
    assert!(!cfg.search_regex);
}

#[test]
fn search_regex_mode_matches_pattern() {
    use crate::core::types::FlatLine;
    use crate::core::visible::{collect_search_matches, compile_search_pattern};

    let lines = vec![FlatLine {
        record_id: 1,
        line_index: 0,
        segments: vec![],
        raw: "GET /api/users 200".to_string(),
        level: None,
        collapsible: false,
        collapsed: false,
        hidden_line_count: 0,
    }];
    let pattern = compile_search_pattern(r"GET /api/\w+", true, false, false).unwrap();
    let matches = collect_search_matches(&lines, &pattern);
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].start, 0);
    assert_eq!(matches[0].end, 14);
}

#[test]
fn search_invalid_regex_returns_error() {
    use crate::core::visible::compile_search_pattern;

    assert!(compile_search_pattern("[unclosed", true, false, false).is_err());
}

#[test]
fn search_highlight_marks_active_match() {
    use crate::core::types::TextSegment;
    use crate::core::visible::{compile_search_pattern, highlight_search_in_segments};

    let segments = vec![TextSegment {
        text: "foo bar foo".to_string(),
        style: None,
    }];
    let pattern = compile_search_pattern("foo", false, false, false).unwrap();
    let highlighted = highlight_search_in_segments(&segments, &pattern, Some((8, 11)));
    let styled: Vec<_> = highlighted.iter().filter_map(|s| s.style.clone()).collect();
    assert_eq!(styled.len(), 2);
    assert!(styled[0].search);
    assert!(!styled[0].search_current);
    assert!(styled[1].search_current);
}

#[test]
fn filter_draft_literal_highlights_case_insensitive() {
    use crate::core::types::TextSegment;
    use crate::core::visible::{compile_filter_draft_pattern, highlight_search_in_segments};

    assert!(compile_filter_draft_pattern("", false).is_none());

    let pattern = compile_filter_draft_pattern("err", false).expect("literal draft");
    let segments = vec![TextSegment {
        text: "WARN Error: boom".to_string(),
        style: None,
    }];
    let highlighted = highlight_search_in_segments(&segments, &pattern, None);
    let matches: Vec<_> = highlighted
        .iter()
        .filter(|s| s.style.as_ref().is_some_and(|st| st.search))
        .map(|s| s.text.as_str())
        .collect();
    // Literal "err" matches the "Err" prefix of "Error" (case-insensitive).
    assert_eq!(matches, vec!["Err"]);
}

#[test]
fn filter_draft_invalid_regex_falls_back_to_literal() {
    use crate::core::types::TextSegment;
    use crate::core::visible::{compile_filter_draft_pattern, highlight_search_in_segments};

    // Same escape-fallback as compile_filter — mid-typing "[" still previews.
    let pattern = compile_filter_draft_pattern("[err", true).expect("invalid regex draft");
    let segments = vec![TextSegment {
        text: "got [err here".to_string(),
        style: None,
    }];
    let highlighted = highlight_search_in_segments(&segments, &pattern, None);
    let matches: Vec<_> = highlighted
        .iter()
        .filter(|s| s.style.as_ref().is_some_and(|st| st.search))
        .map(|s| s.text.as_str())
        .collect();
    assert_eq!(matches, vec!["[err"]);
}

#[test]
fn filter_draft_set_compiles_and_clears() {
    use crate::Command;

    let mut engine = crate::Engine::new();
    engine
        .send_command(Command::FilterDraftSet {
            pattern: "boom".into(),
            use_regex: false,
        })
        .unwrap();
    assert!(engine.filter_draft_pattern.is_some());
    assert_eq!(engine.filter_draft_query, "boom");

    engine
        .send_command(Command::FilterDraftSet {
            pattern: String::new(),
            use_regex: false,
        })
        .unwrap();
    assert!(engine.filter_draft_pattern.is_none());
    assert!(engine.filter_draft_query.is_empty());
}

#[test]
fn search_incremental_append_extends_matches() {
    use chrono::Utc;

    use crate::core::buffer::RecordBuffer;
    use crate::core::types::{LogLevel, LogRecord};
    use crate::log_view::LogView;

    let mut buffer = RecordBuffer::new(1000);
    buffer.add(LogRecord {
        id: 1,
        lines: vec!["alpha boom".to_string()],
        text: "alpha boom".to_string(),
        received_at: Utc::now(),
        level: None::<LogLevel>,
        overwrite: false,
    });

    let mut view = LogView::from_runtime("All", Vec::new());
    view.search_query = "boom".to_string();
    view.mark_search_changed();
    view.rebuild(&mut buffer);
    assert_eq!(view.search_matches.len(), 1);
    assert_eq!(view.search_match_scan_end_for_test(), 1);

    buffer.add(LogRecord {
        id: 2,
        lines: vec!["second boom here".to_string()],
        text: "second boom here".to_string(),
        received_at: Utc::now(),
        level: None,
        overwrite: false,
    });
    view.rebuild(&mut buffer);
    assert_eq!(view.search_matches.len(), 2);
    assert_eq!(view.search_matches[1].line_index, 1);
    assert_eq!(view.search_match_scan_end_for_test(), 2);
}

/// End-to-end: real interactive Strapi capture through the terminal
/// emulator preserves every finalized line and colours, with filters
/// applied over the committed records.
#[test]
fn search_goto_advances_match_and_dirties_viewport() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    // Clear initial viewport dirtiness so needs_render tracks search_goto.
    let mut rgba = vec![0u8; 800 * 400 * 4];
    let _ = engine.render(800, 400, &mut rgba);
    assert!(!engine.needs_render());

    // Several hits so next/prev can move; include a non-match separator.
    engine.push_lines_for_test([
        "hit-one unique-token".into(),
        "nope".into(),
        "hit-two unique-token".into(),
        "hit-three unique-token".into(),
        "hit-four unique-token".into(),
    ]);
    engine
        .send_command_json(r#"{"cmd":"search_set","query":"unique-token","regex":false}"#)
        .expect("search_set");
    engine.rebuild_if_needed_for_test();
    let n = engine.search_match_count_for_test();
    assert!(n >= 2, "expected multiple matches, got {n}");
    // New search jumps to the last match.
    assert_eq!(engine.search_match_index_for_test(), n - 1);
    assert_eq!(engine.search_counter_for_test(), format!("{n}/{n}"));

    // Redundant SearchSet (UI may flush before next/prev) must not reset index.
    engine
        .send_command_json(r#"{"cmd":"search_set","query":"unique-token","regex":false}"#)
        .expect("search_set identical");
    engine
        .send_command_json(r#"{"cmd":"search_goto","delta":-1}"#)
        .expect("search_goto prev");
    engine.rebuild_if_needed_for_test();
    assert_eq!(engine.search_match_index_for_test(), n - 2);
    assert_eq!(engine.search_counter_for_test(), format!("{}/{n}", n - 1));
    assert!(
        engine.needs_render(),
        "search_goto must dirty viewport so the UI scrolls/highlights"
    );

    engine
        .send_command_json(r#"{"cmd":"search_goto","delta":1}"#)
        .expect("search_goto next");
    assert_eq!(engine.search_match_index_for_test(), n - 1);
    assert_eq!(engine.search_counter_for_test(), format!("{n}/{n}"));
}

#[test]
fn search_set_empty_clears_matches_so_follow_can_stick() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let mut rgba = vec![0u8; 800 * 400 * 4];
    let _ = engine.render(800, 400, &mut rgba);

    engine.push_lines_for_test([
        "hit-one unique-token".into(),
        "nope".into(),
        "hit-two unique-token".into(),
        "hit-three unique-token".into(),
        "hit-four unique-token".into(),
    ]);
    engine
        .send_command_json(r#"{"cmd":"search_set","query":"unique-token","regex":false}"#)
        .expect("search_set");
    engine.rebuild_if_needed_for_test();
    let n = engine.search_match_count_for_test();
    assert!(n >= 2, "expected matches before clear, got {n}");
    assert_eq!(
        engine.active_tab_config_for_test().search_query,
        "unique-token"
    );

    engine
        .send_command_json(r#"{"cmd":"set_follow","follow":true}"#)
        .expect("set_follow");
    assert!(engine.auto_follow_for_test());

    engine
        .send_command_json(r#"{"cmd":"search_set","query":"","regex":false}"#)
        .expect("search_set empty");
    engine.rebuild_if_needed_for_test();
    assert!(
        engine.active_tab_config_for_test().search_query.is_empty(),
        "closing Find must drop the engine query so highlights and Follow freeze stop"
    );
    assert_eq!(engine.search_match_count_for_test(), 0);
    assert!(
        engine.search_counter_for_test().is_empty(),
        "empty query has no match counter"
    );
    assert!(
        engine.auto_follow_for_test(),
        "clearing search must not turn Follow off"
    );
}
