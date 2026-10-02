use super::USER_CONFIG_LOCK;
use crate::core::formats::get_builtin_format;
use crate::core::parser::RecordParser;

#[test]
fn strapi_capture_end_to_end_flat_lines() {
    use crate::core::ansi::strip_ansi;
    use crate::core::buffer::RecordBuffer;
    use crate::core::filter::FilterEngine;
    use crate::core::terminal::TerminalIngest;
    use crate::core::visible::rebuild_flat_lines;

    let bytes = include_bytes!("../../test_data/strapi-portable-pty.bin");

    for size in [1usize, 37, 4096] {
        let mut ingest = TerminalIngest::new();
        let mut buffer = RecordBuffer::new(10_000);
        let mut parser = RecordParser::new(get_builtin_format("node-default"));
        for chunk in bytes.chunks(size) {
            ingest.feed(chunk, &mut buffer, &mut parser);
        }
        ingest.finish(&mut buffer, &mut parser);

        let flat = rebuild_flat_lines(
            &mut buffer,
            &FilterEngine::default(),
            crate::core::types::SeverityFilter::All,
            &std::collections::HashSet::new(),
        );
        let plain: Vec<String> = flat.iter().map(|l| strip_ansi(&l.raw)).collect();

        for needle in [
            "✔ Cleaning dist dir",
            "✔ Compiling TS",
            "Project information",
            "Welcome back!",
            "http://localhost:1337",
        ] {
            assert!(
                plain.iter().any(|l| l.contains(needle)),
                "size={size}: missing {needle:?}"
            );
        }
        assert!(
            !plain.iter().any(|l| {
                let spin = l.chars().any(|c| matches!(c, '⠋' | '⠙' | '⠹' | '⠸' | '⠼'));
                spin && l.contains('✔')
            }),
            "size={size}: glued spinner line"
        );
        // At least one committed line kept its SGR colour segments.
        assert!(
            flat.iter()
                .any(|l| l.segments.iter().any(|s| s.style.is_some())),
            "size={size}: colours lost"
        );
    }
}

#[test]
fn wrap_and_horizontal_scroll_commands() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"load_file","path":"/dev/null"}"#)
        .ok();

    engine
        .send_command_json(r#"{"cmd":"set_wrap_lines","wrap":false}"#)
        .expect("set_wrap_lines");
    assert!(!engine.wrap_lines_for_test());
    assert!(
        engine.needs_render(),
        "toggling wrap must dirty the viewport so lines reflow"
    );

    engine
        .send_command_json(r#"{"cmd":"scroll_horizontal","delta":40.0}"#)
        .expect("scroll_horizontal");
    assert!(engine.scroll_x_for_test() >= 0.0);

    engine
        .send_command_json(r#"{"cmd":"set_wrap_lines","wrap":true}"#)
        .expect("set_wrap_lines");
    assert!(engine.wrap_lines_for_test());
}

#[test]
fn pty_cols_floor_keeps_soft_wrap_independent() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    // Narrow viewport (~24 cols at typical cell width) must still get a
    // wide PTY so child COLUMNS hard-wrap does not cancel soft-wrap.
    engine
        .send_command_json(r#"{"cmd":"resize","width":200,"height":120}"#)
        .expect("resize");
    let cols = engine.pty_cols_for_test();
    assert!(
        cols >= 500,
        "PTY cols must stay above the soft-wrap floor, got {cols}"
    );

    // Wide viewport: PTY follows upward (no stale 120 ceiling).
    engine
        .send_command_json(r#"{"cmd":"resize","width":8000,"height":120}"#)
        .expect("resize wide");
    let wide_cols = engine.pty_cols_for_test();
    assert!(
        wide_cols > cols,
        "wide viewport must grow PTY cols ({wide_cols} <= {cols})"
    );
    assert!(wide_cols > 120, "must not be stuck at the old 120-col PTY");
}

#[test]
fn scroll_to_bottom_enables_follow() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":400,"height":120}"#)
        .expect("resize");
    // Inject enough lines that the viewport can scroll.
    for i in 0..80 {
        engine
            .send_command_json(&format!(r#"{{"cmd":"stdin","text":"line {i:03}\n"}}"#))
            .ok();
    }
    // Without a live PTY, seed flat_lines via load_file is awkward; force scroll APIs.
    engine
        .send_command_json(r#"{"cmd":"set_follow","follow":false}"#)
        .expect("set_follow false");
    engine
        .send_command_json(r#"{"cmd":"scroll_to","pos":"start"}"#)
        .expect("scroll_to start");
    assert!(!engine.auto_follow_for_test());

    engine
        .send_command_json(r#"{"cmd":"scroll_to","pos":"end"}"#)
        .expect("scroll_to end");
    assert!(
        engine.auto_follow_for_test(),
        "scrolling to the bottom must turn Follow on"
    );
}

#[test]
fn typing_while_scrolled_up_enables_follow_and_jumps_to_end() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":400,"height":120}"#)
        .expect("resize");
    engine.push_lines_for_test((0..80).map(|i| format!("line {i:03}")));
    engine.mark_running_for_test();
    engine
        .send_command_json(r#"{"cmd":"set_follow","follow":false}"#)
        .expect("set_follow false");
    engine
        .send_command_json(r#"{"cmd":"scroll_to","pos":"start"}"#)
        .expect("scroll_to start");
    assert!(!engine.auto_follow_for_test());
    let max_scroll = engine.max_scroll_offset_for_test();
    assert!(
        max_scroll > 1.0,
        "fixture must be tall enough to scroll, max={max_scroll}"
    );
    assert!(
        engine.scroll_offset_y_for_test() < max_scroll - 1.0,
        "must start away from the bottom"
    );

    engine.handle_key(b"x");

    assert!(
        engine.auto_follow_for_test(),
        "typing in the Terminal tab must turn Follow on"
    );
    let after = engine.scroll_offset_y_for_test();
    let max_after = engine.max_scroll_offset_for_test();
    assert!(
        (after - max_after).abs() < 1.0,
        "typing must jump to the end (scroll={after}, max={max_after})"
    );
}

#[test]
fn viewport_font_size_clamps_and_defaults() {
    use crate::core::types::{
        clamp_viewport_font_size, DEFAULT_VIEWPORT_FONT_SIZE, MAX_VIEWPORT_FONT_SIZE,
        MIN_VIEWPORT_FONT_SIZE,
    };

    assert_eq!(clamp_viewport_font_size(13.0), DEFAULT_VIEWPORT_FONT_SIZE);
    assert_eq!(clamp_viewport_font_size(7.0), MIN_VIEWPORT_FONT_SIZE);
    assert_eq!(clamp_viewport_font_size(40.0), MAX_VIEWPORT_FONT_SIZE);
    assert_eq!(
        clamp_viewport_font_size(f32::NAN),
        DEFAULT_VIEWPORT_FONT_SIZE
    );
    assert_eq!(
        crate::core::config::load_bundled_config().viewport_font_size,
        DEFAULT_VIEWPORT_FONT_SIZE
    );
    let merged = crate::core::config::load_config_from_yaml("viewport_font_size: 99\n");
    assert_eq!(merged.viewport_font_size, MAX_VIEWPORT_FONT_SIZE);
}

#[test]
fn set_viewport_font_size_updates_metrics_and_dirties() {
    // Persistence writes to a temp config dir so the developer's real
    // config.yaml is never touched, even on a failed assert (issue #239).
    // Lock FIRST, then set the override (issue #253): the override is a
    // process global, so constructing the guard before acquiring
    // USER_CONFIG_LOCK lets a parallel test's guard Drop clear the override
    // while this test runs, redirecting the persistence write to the real
    // `~/.config/noviewlog`.
    let _config_lock = USER_CONFIG_LOCK.lock().expect("user config lock");
    let _guard = super::ConfigDirGuard::new("viewport-font-metrics");
    let mut engine = crate::Engine::new();
    let original = engine.viewport_font_size_for_test();
    let baseline = engine.viewport_row_stride_for_test();
    let bigger = if original <= 24.0 {
        (original + 8.0).min(32.0)
    } else {
        16.0
    };

    engine
        .send_command_json(&format!(
            r#"{{"cmd":"set_viewport_font_size","size":{bigger}}}"#
        ))
        .expect("set_viewport_font_size");
    assert!((engine.viewport_font_size_for_test() - bigger).abs() < 0.01);
    assert!(engine.viewport_dirty_for_test());
    assert!(
        (engine.viewport_row_stride_for_test() - baseline).abs() > 0.01,
        "row stride should change when font size changes"
    );

    engine
        .send_command_json(r#"{"cmd":"set_viewport_font_size","size":4}"#)
        .expect("clamp low");
    assert!((engine.viewport_font_size_for_test() - 8.0).abs() < 0.01);

    engine
        .send_command_json(r#"{"cmd":"set_viewport_font_size","size":50}"#)
        .expect("clamp high");
    assert!((engine.viewport_font_size_for_test() - 32.0).abs() < 0.01);
}

#[test]
fn viewport_font_size_persists_across_engine_restart() {
    // Persistence writes to a temp config dir so the developer's real
    // config.yaml is never touched, even on a failed assert (issue #239).
    // Lock FIRST, then set the override (issue #253) — see the metrics test.
    let _config_lock = USER_CONFIG_LOCK.lock().expect("user config lock");
    let guard = super::ConfigDirGuard::new("viewport-font-persist");
    let mut engine = crate::Engine::new();
    // Persistence is debounced and skipped by default in tests; opt in and
    // flush explicitly so the restart below reads the saved value.
    engine.skip_projects_persist = false;
    engine
        .send_command_json(r#"{"cmd":"set_viewport_font_size","size":18}"#)
        .expect("set 18");
    assert!((engine.viewport_font_size_for_test() - 18.0).abs() < 0.01);
    engine.flush_persist();

    let reloaded = crate::Engine::new();
    assert!(
        (reloaded.viewport_font_size_for_test() - 18.0).abs() < 0.01,
        "new engine should load saved viewport_font_size"
    );
    drop(reloaded);
    drop(engine);
    drop(guard);
}
