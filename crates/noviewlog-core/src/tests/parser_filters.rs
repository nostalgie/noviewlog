use super::sample_records;
use crate::core::filter::FilterEngine;
use crate::core::formats::get_builtin_format;
use crate::core::parser::{reparse_lines, RecordParser};
use crate::core::types::{compile_filter, FilterRule, FilterType, LogLevel, LogRecord};

#[test]
fn groups_stack_trace_into_single_record() {
    let mut parser = RecordParser::new(get_builtin_format("node-default"));
    assert!(parser
        .push_line("Error: something failed".to_string())
        .is_empty());
    assert!(parser
        .push_line("    at Object.<anonymous> (/app/index.js:10:5)".to_string())
        .is_empty());
    assert!(parser
        .push_line("    at Module._compile (node:internal/modules/cjs/loader:1376:14)".to_string())
        .is_empty());

    let last = parser.flush_pending().expect("pending record");
    assert_eq!(last.lines.len(), 3);
    assert!(last.text.contains("Error: something failed"));
    assert!(
        last.effective_level() == Some(LogLevel::Error),
        "severity is classified at display/rebuild, not stored on ingest"
    );
}

#[test]
fn starts_new_record_on_timestamp_line() {
    let mut parser = RecordParser::new(get_builtin_format("node-default"));
    parser.push_line("2024-01-01T10:00:00.000Z info: first".to_string());
    let records = parser.push_line("2024-01-01T10:00:01.000Z info: second".to_string());
    assert_eq!(records.len(), 1);
    assert!(records[0].lines[0].contains("first"));
}

#[test]
fn parses_pino_json_one_line_per_record() {
    let mut parser = RecordParser::new(get_builtin_format("pino"));
    assert!(parser
        .push_line(r#"{"level":30,"msg":"hello"}"#.to_string())
        .is_empty());
    let records = parser.push_line(r#"{"level":50,"msg":"error"}"#.to_string());
    assert_eq!(records.len(), 1);
    assert!(records[0].text.contains("hello"));
}

#[test]
fn reparse_lines_rebuilds_buffer() {
    let lines = vec![
        "Error: fail".to_string(),
        "    at foo.js:1:1".to_string(),
        "[Nest] INFO started".to_string(),
    ];
    let records = reparse_lines(&lines, get_builtin_format("node-default"));
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].lines.len(), 2);
    assert!(records[1].lines[0].contains("[Nest]"));
}

#[test]
fn excludes_matching_records() {
    let engine = FilterEngine::new(vec![compile_filter(FilterRule {
        id: "x".to_string(),
        name: None,
        filter_type: FilterType::Exclude,
        pattern: "deprecated".to_string(),
        enabled: true,
        use_regex: true,
        regex: None,
    })]);
    let records = sample_records();
    let visible: Vec<u64> = engine
        .filter_records(&records)
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(visible, vec![2, 3]);
}

#[test]
fn includes_only_matching_when_include_active() {
    let engine = FilterEngine::new(vec![compile_filter(FilterRule {
        id: "x".to_string(),
        name: None,
        filter_type: FilterType::Include,
        pattern: "Error".to_string(),
        enabled: true,
        use_regex: true,
        regex: None,
    })]);
    let records = sample_records();
    let visible: Vec<u64> = engine
        .filter_records(&records)
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(visible, vec![2]);
}

#[test]
fn exclude_wins_over_include() {
    let engine = FilterEngine::new(vec![
        compile_filter(FilterRule {
            id: "i".to_string(),
            name: None,
            filter_type: FilterType::Include,
            pattern: "Error|warn".to_string(),
            enabled: true,
            use_regex: true,
            regex: None,
        }),
        compile_filter(FilterRule {
            id: "e".to_string(),
            name: None,
            filter_type: FilterType::Exclude,
            pattern: "deprecated".to_string(),
            enabled: true,
            use_regex: true,
            regex: None,
        }),
    ]);
    let records = sample_records();
    let visible: Vec<u64> = engine
        .filter_records(&records)
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(visible, vec![2]);
}

#[test]
fn filter_literal_mode_does_not_treat_dot_as_wildcard() {
    let literal = FilterEngine::new(vec![compile_filter(FilterRule {
        id: "lit".to_string(),
        name: None,
        filter_type: FilterType::Include,
        pattern: "foo.bar".to_string(),
        enabled: true,
        use_regex: false,
        regex: None,
    })]);
    let regex = FilterEngine::new(vec![compile_filter(FilterRule {
        id: "re".to_string(),
        name: None,
        filter_type: FilterType::Include,
        pattern: "foo.bar".to_string(),
        enabled: true,
        use_regex: true,
        regex: None,
    })]);
    let records = vec![
        LogRecord {
            id: 1,
            lines: vec!["foo.bar".into()],
            text: "foo.bar".into(),
            received_at: chrono::Utc::now(),
            level: None,
            overwrite: false,
        },
        LogRecord {
            id: 2,
            lines: vec!["fooxbar".into()],
            text: "fooxbar".into(),
            received_at: chrono::Utc::now(),
            level: None,
            overwrite: false,
        },
    ];
    let lit_ids: Vec<u64> = literal
        .filter_records(&records)
        .into_iter()
        .map(|r| r.id)
        .collect();
    let re_ids: Vec<u64> = regex
        .filter_records(&records)
        .into_iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(lit_ids, vec![1]);
    assert_eq!(re_ids, vec![1, 2]);
}

#[test]
fn filter_add_regex_false_and_omitted_default() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(
            r#"{"cmd":"filter_add","type":"include","pattern":"foo.bar","regex":false}"#,
        )
        .expect("filter_add literal");
    engine
        .send_command_json(r#"{"cmd":"filter_add","type":"exclude","pattern":"warn"}"#)
        .expect("filter_add default regex");

    let cfg = engine.active_tab_config_for_test();
    assert_eq!(cfg.filters.len(), 2);
    assert!(!cfg.filters[0].use_regex);
    assert_eq!(cfg.filters[0].pattern, "foo.bar");
    assert!(cfg.filters[1].use_regex);

    // Legacy YAML without use_regex → true
    let yaml = r#"
id: legacy
name: null
type: include
pattern: Error
enabled: true
"#;
    let parsed: FilterRule = serde_yaml::from_str(yaml).unwrap();
    assert!(parsed.use_regex);
}

#[test]
fn filter_update_keeps_id_type_regex() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(
            r#"{"cmd":"filter_add","type":"include","pattern":"foo.bar","regex":false}"#,
        )
        .expect("filter_add");

    let id = engine.active_tab_config_for_test().filters[0].id.clone();
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"filter_update","id":"{id}","pattern":"baz.qux"}}"#
        ))
        .expect("filter_update");

    let cfg = engine.active_tab_config_for_test();
    assert_eq!(cfg.filters.len(), 1);
    assert_eq!(cfg.filters[0].id, id);
    assert_eq!(cfg.filters[0].pattern, "baz.qux");
    assert!(!cfg.filters[0].use_regex);
    assert_eq!(cfg.filters[0].filter_type, FilterType::Include);
    assert!(cfg.filters[0].enabled);
}

#[test]
fn filter_update_terminal_tab_and_empty_are_noop() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"filter_update","id":"missing","pattern":"foo"}"#)
        .expect("filter_update on terminal tab");
    assert!(
        engine.active_tab_config_for_test().filters.is_empty(),
        "Terminal tab must not accept filter_update"
    );

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(r#"{"cmd":"filter_add","type":"exclude","pattern":"warn","regex":true}"#)
        .expect("filter_add");

    let before = engine.active_tab_config_for_test();
    let id = before.filters[0].id.clone();
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"filter_update","id":"{id}","pattern":""}}"#
        ))
        .expect("filter_update empty");
    engine
        .send_command_json(r#"{"cmd":"filter_update","id":"no-such","pattern":"other"}"#)
        .expect("filter_update unknown id");

    let after = engine.active_tab_config_for_test();
    assert_eq!(after.filters.len(), 1);
    assert_eq!(after.filters[0].id, id);
    assert_eq!(after.filters[0].pattern, "warn");
    assert_eq!(after.filters[0].filter_type, FilterType::Exclude);
    assert!(after.filters[0].use_regex);
}

#[test]
fn set_format_reparses_entire_buffer() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine.push_lines_for_test([
        "Error: fail".into(),
        "    at foo.js:1:1".into(),
        "[Nest] INFO started".into(),
    ]);

    engine
        .send_command_json(r#"{"cmd":"set_format","format_id":"raw"}"#)
        .expect("set_format raw");
    assert_eq!(
        engine.buffer_record_count_for_test(),
        3,
        "raw format must treat each line as its own record"
    );

    engine
        .send_command_json(r#"{"cmd":"set_format","format_id":"node-default"}"#)
        .expect("set_format node-default");
    assert_eq!(
        engine.buffer_record_count_for_test(),
        2,
        "node-default must regroup stack frames into one record"
    );
}

#[test]
fn severity_filter_errors_and_unleveled() {
    use crate::core::filter::FilterEngine;
    use crate::core::types::SeverityFilter;
    use crate::core::visible::rebuild_flat_lines_for_records;
    use chrono::Utc;

    let mut records = sample_records();
    records.push(LogRecord {
        id: 4,
        lines: vec!["plain output".to_string()],
        text: "plain output".to_string(),
        received_at: Utc::now(),
        level: None,
        overwrite: false,
    });

    let engine = FilterEngine::default();
    let empty = std::collections::HashSet::new();
    let errors = rebuild_flat_lines_for_records(&records, &engine, SeverityFilter::Error, &empty);
    assert_eq!(errors.len(), 1);
    assert!(errors[0].raw.contains("Error: boom"));
    assert_eq!(errors[0].level, Some(LogLevel::Error));

    let unleveled =
        rebuild_flat_lines_for_records(&records, &engine, SeverityFilter::Unleveled, &empty);
    assert_eq!(unleveled.len(), 1);
    assert_eq!(unleveled[0].raw, "plain output");
    assert!(unleveled[0].level.is_none());
}

#[test]
fn severity_applies_after_include_exclude() {
    use crate::core::filter::FilterEngine;
    use crate::core::types::{compile_filter, FilterRule, FilterType, SeverityFilter};
    use crate::core::visible::rebuild_flat_lines_for_records;

    let records = sample_records();
    // Exclude hides the warn record; include keeps Error|info; severity Errors narrows further.
    let filter = FilterEngine::new(vec![
        compile_filter(FilterRule {
            id: "i".into(),
            name: None,
            filter_type: FilterType::Include,
            pattern: "Error|info".into(),
            enabled: true,
            use_regex: true,
            regex: None,
        }),
        compile_filter(FilterRule {
            id: "e".into(),
            name: None,
            filter_type: FilterType::Exclude,
            pattern: "deprecated".into(),
            enabled: true,
            use_regex: true,
            regex: None,
        }),
    ]);
    let empty = std::collections::HashSet::new();
    let all_sev = rebuild_flat_lines_for_records(&records, &filter, SeverityFilter::All, &empty);
    assert_eq!(all_sev.len(), 2); // Error + info
    let only_err = rebuild_flat_lines_for_records(&records, &filter, SeverityFilter::Error, &empty);
    assert_eq!(only_err.len(), 1);
    assert!(only_err[0].raw.contains("Error"));
}

#[test]
fn severity_set_command_updates_view() {
    use crate::engine::Engine;
    use crate::Command;

    let mut engine = Engine::new();
    engine
        .send_command(Command::SeveritySet {
            mode: "error".into(),
        })
        .expect("severity set");
    assert_eq!(engine.active_view_severity_for_test(), "error");
    engine
        .send_command_json(r#"{"cmd":"severity_set","mode":"unleveled"}"#)
        .expect("json severity");
    assert_eq!(engine.active_view_severity_for_test(), "unleveled");
}

#[test]
fn multiline_records_default_collapsed_and_toggle() {
    use crate::core::filter::FilterEngine;
    use crate::core::types::SeverityFilter;
    use crate::core::visible::rebuild_flat_lines_for_records;
    use chrono::Utc;
    use std::collections::HashSet;

    let records = vec![LogRecord {
        id: 10,
        lines: vec![
            "Error: boom".into(),
            "    at foo.js:1:1".into(),
            "    at bar.js:2:2".into(),
        ],
        text: "Error: boom\n    at foo.js:1:1\n    at bar.js:2:2".into(),
        received_at: Utc::now(),
        level: Some(LogLevel::Error),
        overwrite: false,
    }];
    let engine = FilterEngine::default();
    let empty = HashSet::new();
    let collapsed = rebuild_flat_lines_for_records(&records, &engine, SeverityFilter::All, &empty);
    assert_eq!(collapsed.len(), 1);
    assert!(collapsed[0].collapsed);
    assert_eq!(collapsed[0].hidden_line_count, 2);
    assert!(collapsed[0].collapsible);

    let mut expanded = HashSet::new();
    expanded.insert(10);
    let open = rebuild_flat_lines_for_records(&records, &engine, SeverityFilter::All, &expanded);
    assert_eq!(open.len(), 3);
    assert!(!open[0].collapsed);
    assert!(open[0].collapsible);
}

#[test]
fn collapse_respects_exclude_on_full_text() {
    use crate::core::filter::FilterEngine;
    use crate::core::types::{compile_filter, FilterRule, FilterType, SeverityFilter};
    use crate::core::visible::rebuild_flat_lines_for_records;
    use chrono::Utc;
    use std::collections::HashSet;

    let records = vec![LogRecord {
        id: 11,
        lines: vec!["Error: boom".into(), "    at deprecated.js:1".into()],
        text: "Error: boom\n    at deprecated.js:1".into(),
        received_at: Utc::now(),
        level: Some(LogLevel::Error),
        overwrite: false,
    }];
    let filter = FilterEngine::new(vec![compile_filter(FilterRule {
        id: "e".into(),
        name: None,
        filter_type: FilterType::Exclude,
        pattern: "deprecated".into(),
        enabled: true,
        use_regex: true,
        regex: None,
    })]);
    let flat =
        rebuild_flat_lines_for_records(&records, &filter, SeverityFilter::All, &HashSet::new());
    assert!(flat.is_empty(), "exclude matches full record text");
}

#[test]
fn expand_collapse_all_commands() {
    use crate::engine::Engine;
    use crate::Command;

    let mut engine = Engine::new();
    engine.push_lines_for_test([
        "Error: boom".into(),
        "    at foo.js:1:1".into(),
        "    at bar.js:2:2".into(),
        "info: done".into(),
        "info: again".into(),
    ]);
    // After second info line, first info commits; Error stack is one collapsed row + one info.
    assert!(
        engine.active_flat_line_count_for_test() >= 2,
        "expected collapsed stack + at least one info line, got {}",
        engine.active_flat_line_count_for_test()
    );
    let collapsed_count = engine.active_flat_line_count_for_test();
    engine
        .send_command(Command::RecordsExpandAll)
        .expect("expand");
    engine.rebuild_active_for_test();
    let expanded_count = engine.active_flat_line_count_for_test();
    assert!(
        expanded_count > collapsed_count,
        "expand-all should reveal stack frames ({expanded_count} vs {collapsed_count})"
    );
    engine
        .send_command(Command::RecordsCollapseAll)
        .expect("collapse");
    engine.rebuild_active_for_test();
    assert_eq!(engine.active_flat_line_count_for_test(), collapsed_count);
}

#[test]
fn invalid_regex_filter_surfaces_literal_fallback_notice() {
    // Issue #75: a typo like `[[error` must not silently become a
    // matches-nothing literal — the user gets a visible status notice.
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(
            r#"{"cmd":"filter_add","type":"include","pattern":"[[error","regex":true}"#,
        )
        .expect("filter_add invalid regex");

    let cfg = engine.active_tab_config_for_test();
    assert_eq!(
        cfg.filters.len(),
        1,
        "rule is still added (literal fallback)"
    );
    assert!(
        engine.status_message_for_test().contains("Invalid regex"),
        "status: {}",
        engine.status_message_for_test()
    );

    // Update to another invalid regex keeps the notice flowing.
    let id = cfg.filters[0].id.clone();
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"filter_update","id":"{id}","pattern":"(unclosed"}}"#
        ))
        .expect("filter_update invalid regex");
    assert!(engine.status_message_for_test().contains("Invalid regex"));

    // A valid regex update replaces the rule with a working regex. The
    // status text persists until the next event — the notice is only
    // emitted on the invalid path (verified above).
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"filter_update","id":"{id}","pattern":"^ok$"}}"#
        ))
        .expect("filter_update valid regex");
    let updated = engine.active_tab_config_for_test();
    let rule = updated
        .filters
        .iter()
        .find(|f| f.id == id)
        .expect("rule survives update");
    assert_eq!(
        rule.regex.as_ref().map(|re| re.as_str()),
        Some(r"^ok$"),
        "valid regex must compile as regex"
    );
}
