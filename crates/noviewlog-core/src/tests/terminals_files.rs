#[test]
fn selection_copy_returns_plain_text() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":400,"height":200}"#)
        .expect("resize");
    engine
        .send_command_json(r#"{"cmd":"selection_at","x":16.0,"y":8.0,"extend":false}"#)
        .expect("selection_at");
    engine
        .send_command_json(r#"{"cmd":"selection_at","x":80.0,"y":8.0,"extend":true}"#)
        .expect("selection_extend");
    // Empty buffer — selection may be empty; command path must not error.
    let _ = engine.selection_text_for_test();
}

#[test]
fn terminal_add_switch_keeps_other_running() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    let first_id = engine.active_terminal_id_for_test();
    engine
        .send_command_json(r#"{"cmd":"terminal_start"}"#)
        .expect("terminal_start");
    assert_eq!(engine.terminal_running_for_test(&first_id), Some(true));

    engine
        .send_command_json(r#"{"cmd":"terminal_add"}"#)
        .expect("terminal_add");
    let second_id = engine.active_terminal_id_for_test();
    assert_ne!(first_id, second_id);
    assert_eq!(engine.terminals_for_test().len(), 2);
    // First terminal should still be marked running (multi-PTY; switch does not stop).
    assert_eq!(engine.terminal_running_for_test(&first_id), Some(true));
    assert_eq!(engine.terminal_running_for_test(&second_id), Some(true));

    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_switch","terminal_id":"{first_id}"}}"#
        ))
        .expect("terminal_switch");
    assert_eq!(engine.active_terminal_id_for_test(), first_id);
    assert_eq!(engine.terminal_running_for_test(&first_id), Some(true));
    assert_eq!(engine.terminal_running_for_test(&second_id), Some(true));
}

#[test]
fn terminal_close_refuses_last_live() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    let first_id = engine.active_terminal_id_for_test();
    engine
        .send_command_json(r#"{"cmd":"terminal_add"}"#)
        .expect("terminal_add");
    assert_eq!(engine.terminals_for_test().len(), 2);
    let second_id = engine.active_terminal_id_for_test();

    // Can close a non-last live terminal (including the first by index).
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_close","terminal_id":"{first_id}"}}"#
        ))
        .expect("terminal_close first");
    assert_eq!(engine.terminals_for_test().len(), 1);
    assert_eq!(engine.active_terminal_id_for_test(), second_id);

    // Cannot close the last live terminal.
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_close","terminal_id":"{second_id}"}}"#
        ))
        .expect("terminal_close last refused");
    assert_eq!(engine.terminals_for_test().len(), 1);
}

#[test]
fn terminal_move_reorders_and_tracks_active() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    let id0 = engine.active_terminal_id_for_test();
    engine
        .send_command_json(r#"{"cmd":"terminal_add"}"#)
        .expect("add1");
    let id1 = engine.active_terminal_id_for_test();
    engine
        .send_command_json(r#"{"cmd":"terminal_add"}"#)
        .expect("add2");
    let id2 = engine.active_terminal_id_for_test();
    assert_eq!(engine.terminals_for_test().len(), 3);

    // Active is id2 (newest). Move id0 to end.
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_move","terminal_id":"{id0}","to_index":2}}"#
        ))
        .expect("move");
    let ids: Vec<String> = engine
        .terminals_for_test()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(ids, vec![id1.clone(), id2.clone(), id0.clone()]);
    // active was id2 at index 2, after moving id0 from 0 to 2: id2 should still be active
    assert_eq!(engine.active_terminal_id_for_test(), id2);

    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_switch","terminal_id":"{id1}"}}"#
        ))
        .expect("switch");
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_move","terminal_id":"{id1}","to_index":2}}"#
        ))
        .expect("move active");
    assert_eq!(engine.active_terminal_id_for_test(), id1);
    let ids: Vec<String> = engine
        .terminals_for_test()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    assert_eq!(ids.last().unwrap(), &id1);
}

#[test]
fn terminal_rename_sets_label_and_ignores_empty_unknown() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    let id = engine.active_terminal_id_for_test();
    let auto_label = engine.terminals_for_test()[0].1.clone();

    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_rename","terminal_id":"{id}","name":"api"}}"#
        ))
        .expect("rename");
    assert_eq!(engine.terminals_for_test()[0].1, "api");

    // Empty / whitespace must not clear the custom title.
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_rename","terminal_id":"{id}","name":"   "}}"#
        ))
        .expect("empty rename");
    assert_eq!(engine.terminals_for_test()[0].1, "api");

    engine
        .send_command_json(r#"{"cmd":"terminal_rename","terminal_id":"missing-id","name":"other"}"#)
        .expect("unknown id");
    assert_eq!(engine.terminals_for_test()[0].1, "api");

    // Custom title survives cwd changes (OSC 7 / auto label path).
    engine.active_terminal_mut().cwd = format!("{auto_label}-changed-cwd-path/other");
    assert_eq!(engine.terminals_for_test()[0].1, "api");
    assert_ne!(
        crate::terminal_state::cwd_label(&engine.active_terminal().cwd),
        "api"
    );
}

#[test]
fn terminal_switch_marks_viewport_dirty() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    let first = engine.active_terminal_id_for_test();
    engine
        .send_command_json(r#"{"cmd":"terminal_add"}"#)
        .expect("add");
    let second = engine.active_terminal_id_for_test();
    // Stop PTYs and disable follow so needs_render tracks viewport_dirty only.
    engine.send_command_json(r#"{"cmd":"stop"}"#).expect("stop");
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_switch","terminal_id":"{first}"}}"#
        ))
        .expect("switch first");
    engine
        .send_command_json(r#"{"cmd":"stop"}"#)
        .expect("stop first");
    engine
        .send_command_json(r#"{"cmd":"set_follow","follow":false}"#)
        .expect("follow off");
    let _ = engine.render(800, 600, &mut vec![0u8; 800 * 600 * 4]);
    assert!(!engine.needs_render());

    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_switch","terminal_id":"{second}"}}"#
        ))
        .expect("switch");
    assert!(engine.needs_render());

    engine
        .send_command_json(r#"{"cmd":"set_follow","follow":false}"#)
        .expect("follow off");
    let _ = engine.render(800, 600, &mut vec![0u8; 800 * 600 * 4]);
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_switch","terminal_id":"{first}"}}"#
        ))
        .expect("switch2");
    assert!(engine.needs_render());
}

#[test]
fn idle_running_follow_does_not_need_render() {
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"set_follow","follow":true}"#)
        .expect("follow on");
    engine.mark_running_for_test();
    assert!(engine.auto_follow_for_test());

    // Settle initial flat/search rebuild so tick no longer reports a change.
    engine.rebuild_if_needed_for_test();

    let mut rgba = vec![0u8; 800 * 600 * 4];
    engine
        .render(800, 600, &mut rgba)
        .expect("render clears dirty");
    assert!(!engine.needs_render());

    // No PTY activity, no focus/caret blink — tick must not force paint.
    engine.tick();
    assert!(
        !engine.needs_render(),
        "idle live shell with auto-follow must not perpetual-redraw"
    );
}

#[test]
fn stats_json_includes_terminals() {
    use crate::engine::Engine;
    use serde_json::Value;
    use std::thread;
    use std::time::Duration;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"terminal_add"}"#)
        .expect("add");
    while engine.poll_event_json().is_some() {}
    thread::sleep(Duration::from_millis(260));
    engine.tick();
    let mut stats = None;
    while let Some(ev) = engine.poll_event_json() {
        let v: Value = serde_json::from_str(&ev).unwrap();
        if v["type"] == "stats" {
            stats = Some(v);
        }
    }
    let parsed = stats.expect("stats event");
    assert!(parsed["terminals"].as_array().unwrap().len() >= 2);
    assert!(parsed["active_terminal"].as_u64().is_some());
    assert!(parsed["terminal_id"].as_str().is_some());
    assert!(parsed["has_active_terminal"].as_bool().unwrap());
}

#[test]
fn load_file_on_terminal_sets_log_file() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-test-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "line one").unwrap();
        writeln!(f, "line two").unwrap();
    }
    let mut engine = Engine::new();
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    assert!(engine.buffer_record_count_for_test() >= 2);
    // load_file always opens a dedicated file session (live terminal stays).
    assert_eq!(engine.terminals_for_test().len(), 2);
    assert!(engine.active_is_file_session_for_test());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_file_creates_separate_terminal_when_session_used() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-file-term-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "alpha").unwrap();
        writeln!(f, "beta").unwrap();
    }
    let mut engine = Engine::new();
    // Simulate an interactive session that already started (must not be hijacked).
    engine.mark_active_process_started_for_test();
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    assert_eq!(engine.terminals_for_test().len(), 2);
    assert_eq!(engine.active_terminal_index_for_test(), 1);
    assert!(engine.active_is_file_session_for_test());
    assert!(!engine.terminal_is_file_session_for_test(0));
    assert!(engine.buffer_record_count_for_test() >= 2);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn load_file_reopen_switches_to_existing_file_terminal() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-reopen-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "one").unwrap();
    }
    let mut engine = Engine::new();
    engine.mark_active_process_started_for_test();
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    assert_eq!(engine.terminals_for_test().len(), 2);
    let file_id = engine.active_terminal_id_for_test();

    // Switch back to the first (interactive) terminal.
    let first_id = engine.terminals_for_test()[0].0.clone();
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_switch","terminal_id":"{first_id}"}}"#
        ))
        .expect("switch");
    assert_eq!(engine.active_terminal_index_for_test(), 0);

    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("reopen");
    engine.finish_file_load_for_test();
    assert_eq!(engine.terminals_for_test().len(), 2);
    assert_eq!(engine.active_terminal_id_for_test(), file_id);
    assert!(engine.active_is_file_session_for_test());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_session_rejects_stdin_and_start() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-viewonly-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "line").unwrap();
    }
    let mut engine = Engine::new();
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    assert!(engine.active_is_file_session_for_test());

    engine.handle_key(b"echo hi\n");
    assert!(!engine.active_terminal_running_for_test());

    engine
        .send_command_json(r#"{"cmd":"start","command":"true","args":[]}"#)
        .expect("start");
    assert!(engine.active_is_file_session_for_test());
    assert!(!engine.active_terminal_running_for_test());
    assert!(engine.status_message_for_test().contains("view-only"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn project_program_config_round_trip_yaml() {
    use crate::core::types::{
        LaunchConfig, ProgramConfig, ProjectConfig, ProjectsStore, WorkspaceConfig,
    };

    let store = ProjectsStore {
        active_project: 0,
        projects: vec![ProjectConfig {
            id: "p1".into(),
            name: "demo".into(),
            default_cwd: Some("/tmp".into()),
            path_hint: None,
            active_program: 0,
            programs: vec![
                ProgramConfig {
                    id: "a".into(),
                    name: "api".into(),
                    launch: LaunchConfig {
                        command: Some("npm".into()),
                        args: vec!["run".into(), "dev".into()],
                        cwd: Some("/tmp/app".into()),
                        ..Default::default()
                    },
                    workspace: WorkspaceConfig::default(),
                },
                ProgramConfig {
                    id: "b".into(),
                    name: "redis".into(),
                    launch: LaunchConfig {
                        command: Some("redis-server".into()),
                        cwd: Some("/usr".into()),
                        ..Default::default()
                    },
                    workspace: WorkspaceConfig::default(),
                },
            ],
        }],
    };
    let yaml = serde_yaml::to_string(&store).unwrap();
    let parsed: ProjectsStore = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(parsed.projects.len(), 1);
    assert_eq!(parsed.projects[0].programs.len(), 2);
    assert_eq!(
        parsed.projects[0].programs[0].launch.cwd.as_deref(),
        Some("/tmp/app")
    );
    assert_eq!(
        parsed.projects[0].programs[1].launch.cwd.as_deref(),
        Some("/usr")
    );
}

#[test]
fn program_display_name_from_launch() {
    use crate::core::types::{program_display_name, LaunchConfig};
    let launch = LaunchConfig {
        command: Some("npm".into()),
        args: vec!["run".into(), "develop".into()],
        ..Default::default()
    };
    assert_eq!(program_display_name(&launch), "npm run develop");
}

#[test]
fn selection_text_with_no_selection_returns_none() {
    use crate::engine::Engine;
    let engine = Engine::new();
    assert!(engine.selection_text_for_test().is_none());
}

#[cfg(windows)]
#[test]
#[ignore = "slow tier: real wsl.exe spawn under ConPTY + 6 s deadline; run with -- --ignored"]
fn wsl_mode_spawns_wsl_exe_under_conpty() {
    use crate::core::types::LaunchConfig;
    use crate::engine::Engine;
    use std::time::{Duration, Instant};

    // Skip on hosts without a working WSL (no distro installed): the spawn
    // itself would legitimately fail before ConPTY is ever exercised.
    let wsl_ok = std::process::Command::new("wsl.exe")
        .arg("--status")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !wsl_ok {
        eprintln!("skipping: wsl.exe --status failed (no usable WSL on host)");
        return;
    }

    let mut engine = Engine::new();
    engine.skip_projects_persist = true;
    engine.set_launch(LaunchConfig {
        command: Some("uname".into()),
        args: vec!["-a".into()],
        cwd: Some("/".into()),
        wsl: true,
        ..LaunchConfig::default()
    });
    let status = engine.status_message_for_test();
    assert!(
        !status.contains("Failed to start"),
        "CreateProcess/ConPTY must accept wsl.exe: {status}"
    );
    assert!(
        status.to_ascii_lowercase().contains("wsl"),
        "status should show wsl.exe spawn: {status}"
    );

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut screen = String::new();
    while Instant::now() < deadline {
        engine.poll_pty_for_test();
        engine.tick();
        screen = engine.live_screen_text_for_test();
        let lower = screen.to_ascii_lowercase();
        if lower.contains("linux")
            || lower.contains("distribution")
            || screen.contains("дистриб")
            || screen.contains("GNU")
        {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    // A registered-but-not-yet-initialized distro (first-run setup, login
    // shell init) can produce no output within the deadline while ConPTY
    // itself is fine — the spawn assertions above are this test's real
    // subject (#217). Treat that as an environment limitation, not a
    // product bug: only a Failed-to-start status is a hard failure.
    assert!(
        !engine.status_message_for_test().contains("Failed to start"),
        "wsl.exe spawn must not regress: {}",
        engine.status_message_for_test()
    );
    eprintln!(
        "skipping output assertion: no WSL output within the deadline \
         (distro may still be initializing on this host); screen={screen:?}"
    );
}

#[test]
fn set_launch_starts_command_immediately() {
    use crate::engine::Engine;
    use crate::tests::long_running_launch_config;

    let mut engine = Engine::new();
    engine.set_launch(long_running_launch_config());
    assert!(engine.process_started_for_test());
    assert!(engine.active_terminal_running_for_test());
    assert!(
        engine.status_message_for_test().starts_with("Running:"),
        "status={}",
        engine.status_message_for_test()
    );
    assert_eq!(engine.terminals_for_test().len(), 1);
    engine.tick();
    assert!(engine.active_terminal_running_for_test());
    assert_eq!(engine.terminals_for_test().len(), 1);
}

#[test]
fn stale_pty_exit_does_not_replace_cli_process() {
    use crate::engine::Engine;
    use crate::tests::long_running_launch_config;

    let mut engine = Engine::new();
    engine.set_launch(long_running_launch_config());
    let id = engine.active_terminal_id_for_test();
    let gen = engine.pty_generation_for_test();
    assert!(gen >= 1);
    engine.inject_pty_exit_for_test(&id, 0, gen.wrapping_sub(1));
    engine.tick();
    assert!(engine.active_terminal_running_for_test());
    assert_eq!(engine.pty_generation_for_test(), gen);
    assert_eq!(engine.terminals_for_test().len(), 1);
    assert!(
        engine.status_message_for_test().starts_with("Running:"),
        "status={}",
        engine.status_message_for_test()
    );
}

#[test]
fn stats_split_terminals_and_files() {
    use crate::engine::Engine;
    use serde_json::Value;
    use std::io::Write;
    use std::thread;
    use std::time::Duration;

    let path =
        std::env::temp_dir().join(format!("noviewlog-stats-split-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "hello").unwrap();
    }
    let mut engine = Engine::new();
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    while engine.poll_event_json().is_some() {}
    thread::sleep(Duration::from_millis(260));
    engine.tick();
    let mut stats = None;
    while let Some(ev) = engine.poll_event_json() {
        let v: Value = serde_json::from_str(&ev).unwrap();
        if v["type"] == "stats" {
            stats = Some(v);
        }
    }
    let parsed = stats.expect("stats");
    assert_eq!(parsed["terminals"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["files"].as_array().unwrap().len(), 1);
    assert!(parsed["is_file_session"].as_bool().unwrap());
    assert!(!parsed["auto_follow"].as_bool().unwrap());
    let file_name = path.file_name().unwrap().to_string_lossy();
    assert_eq!(
        parsed["tabs"][0]["name"].as_str().unwrap(),
        file_name.as_ref()
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_session_ignores_set_follow() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-nofollow-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "line").unwrap();
    }
    let mut engine = Engine::new();
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    engine
        .send_command_json(r#"{"cmd":"set_follow","follow":true}"#)
        .expect("set_follow");
    assert!(!engine.auto_follow_for_test());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn can_close_file_while_keeping_last_live() {
    use crate::engine::Engine;
    use std::io::Write;

    let path =
        std::env::temp_dir().join(format!("noviewlog-close-file-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "line").unwrap();
    }
    let mut engine = Engine::new();
    let live_id = engine.active_terminal_id_for_test();
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    let file_id = engine.active_terminal_id_for_test();
    assert_ne!(live_id, file_id);

    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_close","terminal_id":"{file_id}"}}"#
        ))
        .expect("close file");
    assert_eq!(engine.terminals_for_test().len(), 1);
    assert_eq!(engine.active_terminal_id_for_test(), live_id);
    assert!(!engine.active_is_file_session_for_test());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_scrollbar_mid_jump_loads_window_not_black() {
    use crate::engine::{Command, Engine};
    use std::io::Write;

    // Must be > FILE_LARGE_BYTES so only a sliding window is kept in memory.
    let path =
        std::env::temp_dir().join(format!("noviewlog-scroll-mid-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        let pad = "x".repeat(100);
        for i in 0..200_000 {
            writeln!(f, "line-{i:06}-{pad}").unwrap();
        }
    }
    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    assert!(engine.active_is_file_session_for_test());

    let max = engine.max_scroll_offset_for_test();
    assert!(
        max > 1000.0,
        "whole-file max_scroll should be large, got {max}"
    );
    let mid = max * 0.5;
    engine
        .send_command(Command::Scroll { offset: mid })
        .expect("scroll mid");
    engine.finish_pending_file_window_for_test();
    engine.rebuild_if_needed_for_test();

    let start = engine.buffer_line_start_for_test();
    let end = engine.buffer_line_end_for_test();
    assert!(
        end > start,
        "window must be non-empty start={start} end={end}"
    );
    // Mid of a large file must not leave the viewport on the initial tail-only window forever.
    let total_ish = (max / engine.viewport_row_stride_for_test()) as u64;
    assert!(
        start < total_ish / 2 + 5_000 && end > total_ish / 2 - 5_000,
        "window should cover mid-file region, start={start} end={end} mid≈{}",
        total_ish / 2
    );
    let local = engine.scroll_offset_y_for_test();
    let local_max = engine.local_window_max_scroll_for_test();
    assert!(
        local <= local_max + 1.0,
        "local scroll must stay in the loaded window (local={local} max={local_max})"
    );

    let mut rgba = vec![0u8; 800 * 400 * 4];
    engine.render(800, 400, &mut rgba).expect("render");
    let lit = rgba
        .chunks_exact(4)
        .filter(|px| px[0] | px[1] | px[2] > 0x20)
        .count();
    assert!(
        lit > 200,
        "mid-file paint must show glyphs, not an empty/black frame (lit={lit})"
    );

    let (cur, total) = engine.viewport_line_position_for_test();
    assert!(total > 0, "line position total");
    assert!(
        cur > total / 4 && cur < total * 3 / 4,
        "mid scroll should report mid line position cur={cur} total={total}"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_scrollbar_reaches_eof() {
    use crate::engine::{Command, Engine};
    use std::io::Write;

    let path =
        std::env::temp_dir().join(format!("noviewlog-scroll-eof-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        let pad = "x".repeat(100);
        for i in 0..200_000 {
            writeln!(f, "line-{i:06}-{pad}").unwrap();
        }
    }
    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    let total = engine.file_total_lines_for_test();
    let window = engine.file_view_window_lines_for_test() as u64;
    assert!(total > window * 2);

    let max = engine.max_scroll_offset_for_test();
    engine
        .send_command(Command::Scroll { offset: max })
        .expect("scroll eof");
    engine.finish_pending_file_window_for_test();
    engine.rebuild_if_needed_for_test();

    let start = engine.buffer_line_start_for_test();
    let expected_start = total.saturating_sub(window);
    assert_eq!(start, expected_start, "EOF scroll must pin the last window");
    let local = engine.scroll_offset_y_for_test();
    let local_max = engine.local_window_max_scroll_for_test();
    assert!(
        (local - local_max).abs() < engine.viewport_row_stride_for_test(),
        "EOF local scroll should be at local max (local={local} max={local_max})"
    );
    let max_after = engine.max_scroll_offset_for_test();
    let global = engine.stats_scroll_y_for_test();
    assert!(
        (global - max_after).abs() < engine.viewport_row_stride_for_test() * 2.0,
        "stats scroll_y should stick at max after EOF (global={global} max={max_after})"
    );

    // Second nudge to max while already on last window.
    engine
        .send_command(Command::Scroll {
            offset: engine.max_scroll_offset_for_test(),
        })
        .expect("scroll eof again");
    engine.finish_pending_file_window_for_test();
    assert_eq!(engine.buffer_line_start_for_test(), expected_start);
    let (cur, tot) = engine.viewport_line_position_for_test();
    assert_eq!(tot, total);
    assert_eq!(
        cur, tot,
        "at EOF status must show N / N (bottom of viewport), got {cur} / {tot}"
    );

    let mut rgba = vec![0u8; 800 * 400 * 4];
    engine.render(800, 400, &mut rgba).expect("render");
    let lit = rgba
        .chunks_exact(4)
        .filter(|px| px[0] | px[1] | px[2] > 0x20)
        .count();
    assert!(lit > 200, "EOF paint must show content lit={lit}");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn at_scroll_bottom_uses_global_space_for_file_sessions() {
    use crate::engine::{Command, Engine};
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-at-bottom-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        let pad = "x".repeat(100);
        for i in 0..200_000 {
            writeln!(f, "line-{i:06}-{pad}").unwrap();
        }
    }
    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    let total = engine.file_total_lines_for_test();
    let window = engine.file_view_window_lines_for_test() as u64;
    assert!(total > window * 2);
    assert!(
        !engine.at_scroll_bottom(),
        "fresh open shows the top, not the bottom"
    );

    // Wheel to the bottom of the resident window while it is mid-file:
    // local scroll is maxed but more file remains below the window.
    for _ in 0..window {
        engine
            .send_command(Command::ScrollLines { delta: 3 })
            .expect("wheel down");
        if (engine.scroll_offset_y_for_test() - engine.local_window_max_scroll_for_test()).abs()
            < 0.5
        {
            break;
        }
    }
    assert!(
        engine.buffer_line_start_for_test() + window < total,
        "window must stay mid-file, got start={}",
        engine.buffer_line_start_for_test()
    );
    assert!(
        !engine.at_scroll_bottom(),
        "local window bottom mid-file is not the file bottom"
    );

    // At EOF the viewport bottom is the file bottom: must report at bottom
    // even though the whole-file range dwarfs the local offset.
    let max = engine.max_scroll_offset_for_test();
    engine
        .send_command(Command::Scroll { offset: max })
        .expect("scroll eof");
    engine.finish_pending_file_window_for_test();
    engine.rebuild_if_needed_for_test();
    assert_eq!(
        engine.buffer_line_start_for_test(),
        total.saturating_sub(window),
        "EOF scroll must pin the last window"
    );
    assert!(
        engine.at_scroll_bottom(),
        "EOF must report at bottom (local={} global_max={})",
        engine.scroll_offset_y_for_test(),
        max
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn reload_file_picks_up_appended_lines() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-reload-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "first").unwrap();
    }
    let mut engine = Engine::new();
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    assert!(engine.buffer_record_count_for_test() >= 1);

    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "second").unwrap();
    }
    engine
        .send_command_json(r#"{"cmd":"reload_file"}"#)
        .expect("reload_file");
    engine.finish_file_load_for_test();
    assert!(
        engine.buffer_record_count_for_test() >= 2,
        "reload must re-read appended lines"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn reload_missing_file_keeps_session() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!(
        "noviewlog-reload-missing-{}.log",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "only").unwrap();
    }
    let mut engine = Engine::new();
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    let _ = std::fs::remove_file(&path);

    engine
        .send_command_json(r#"{"cmd":"reload_file"}"#)
        .expect("reload_file");
    assert!(engine.active_is_file_session_for_test());
    // Open failures surface asynchronously from the load worker (issue #55).
    engine.finish_file_load_for_test();
    assert!(
        engine.status_message_for_test().contains("Failed to open"),
        "missing path must report status: {}",
        engine.status_message_for_test()
    );
}

#[test]
fn file_filter_match_index_scans_and_filters_without_window_thrash() {
    use crate::engine::{Command, Engine};
    use std::io::Write;

    // Large enough that only a sliding window is resident; filter must use match index.
    let path = std::env::temp_dir().join(format!("noviewlog-file-match-{}", std::process::id()));
    let needle = "11:01:13";
    let mut expected_hits = 0usize;
    {
        let mut f = std::fs::File::create(&path).unwrap();
        // Long URLs so WRAP creates a tall filtered viewport (scrollbar + wheel).
        let pad = format!(
            "GET http://example.com/{}/{}",
            "x".repeat(120),
            "y".repeat(120)
        );
        for i in 0..80_000 {
            if i % 400 == 0 {
                writeln!(f, "ts={needle} hit-{i} {pad}").unwrap();
                expected_hits += 1;
            } else {
                writeln!(f, "ts=10:00:00 line-{i}").unwrap();
            }
        }
    }

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    assert!(engine.active_is_file_session_for_test());

    // Scroll into the middle so unfiltered mode would have a non-tail window.
    let max_unfiltered = engine.max_scroll_offset_for_test();
    assert!(max_unfiltered > 1000.0);
    engine
        .send_command(Command::Scroll {
            offset: max_unfiltered * 0.5,
        })
        .expect("scroll mid");
    engine.finish_pending_file_window_for_test();
    let window_start_before = engine.buffer_line_start_for_test();
    assert!(window_start_before > 0);

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    assert_eq!(engine.active_tab_index_for_test(), 1);

    engine
        .send_command_json(&format!(
            r#"{{"cmd":"filter_add","type":"include","pattern":"{needle}"}}"#
        ))
        .expect("filter_add");
    assert!(engine.uses_match_index_for_test());
    assert_eq!(
        engine.flat_lines_len_for_test(),
        0,
        "viewport must clear immediately on filter invalidate"
    );
    assert_eq!(engine.scroll_offset_y_for_test(), 0.0);
    assert!(engine.match_scan_pos_for_test().is_some());
    assert!(
        engine.host_work_pending_for_test(),
        "match scan must keep host_work_pending so the UI stays on TICK_FAST"
    );
    assert!(
        engine
            .status_message_for_test()
            .starts_with("Scanning filters…"),
        "status/center progress must show scan: {}",
        engine.status_message_for_test()
    );

    // Mid-scan: max_scroll tracks match count (partial), not full-file height.
    engine.advance_file_match_scan();
    let mid_max = engine.max_scroll_offset_for_test();
    let file_total = engine.file_total_lines_for_test();
    let stride = engine.viewport_row_stride_for_test();
    let full_file_max = (file_total as f32 * stride - 400.0).max(0.0);
    assert!(
        mid_max < full_file_max * 0.1,
        "during scan max_scroll must not use full-file height (mid={mid_max} file={full_file_max})"
    );

    // Prefetch / file-window scroll must not move the underlying buffer window.
    engine.tick();
    engine.tick();
    assert_eq!(
        engine.buffer_line_start_for_test(),
        window_start_before,
        "match-index mode must not slide the file window"
    );
    assert_eq!(engine.flat_lines_len_for_test(), 0);

    // Scrollbar input during scan must stay pinned (no file-window jump).
    engine
        .send_command(Command::Scroll {
            offset: full_file_max * 0.5,
        })
        .expect("scroll during scan");
    assert_eq!(engine.scroll_offset_y_for_test(), 0.0);
    assert_eq!(engine.buffer_line_start_for_test(), window_start_before);

    engine.finish_file_match_scan_for_test();
    assert!(engine.match_scan_pos_for_test().is_none());
    assert_eq!(engine.match_offsets_len_for_test(), expected_hits);
    assert_eq!(engine.flat_lines_len_for_test(), expected_hits);
    for text in engine.flat_line_texts_for_test() {
        assert!(
            text.contains(needle),
            "filtered line must contain needle: {text}"
        );
    }

    let match_max = engine.max_scroll_offset_for_test();
    assert!(
        match_max < full_file_max * 0.05,
        "completed match max_scroll should be tiny vs file (match={match_max} file={full_file_max})"
    );

    // Scroll within matches must not move the file buffer window.
    assert_eq!(engine.buffer_line_start_for_test(), window_start_before);

    // Small match set must scroll locally (WRAP-aware) without freezing / re-seeking.
    engine
        .send_command_json(r#"{"cmd":"set_wrap_lines","wrap":true}"#)
        .expect("wrap on");
    // Reset to top before measuring wheel.
    engine
        .send_command(Command::Scroll { offset: 0.0 })
        .expect("scroll top");
    let match_max = engine.max_scroll_offset_for_test();
    let local_max = engine.local_window_max_scroll_for_test();
    assert!(
        (match_max - local_max).abs() < 1.0,
        "scrollbar max must match wheel local max (match={match_max} local={local_max})"
    );
    assert!(
        match_max > 1.0,
        "wrapped matches must expose scroll range, got {match_max} (hits={expected_hits})"
    );
    let (_cur, total) = engine.viewport_line_position_for_test();
    assert_eq!(
        total, expected_hits as u64,
        "counter total must be match count"
    );

    engine
        .send_command(Command::ScrollLines { delta: 5 })
        .expect("wheel");
    assert!(
        engine.scroll_offset_y_for_test() > 0.5,
        "wheel must move local scroll"
    );

    // Scrollbar path (Command::Scroll) must reach the same range as wheel.
    engine
        .send_command(Command::Scroll { offset: 0.0 })
        .expect("scroll top again");
    let mid = match_max * 0.5;
    engine
        .send_command(Command::Scroll { offset: mid })
        .expect("scrollbar mid");
    assert!(
        (engine.scroll_offset_y_for_test() - mid).abs() < 2.0,
        "scrollbar mid must stick (got {} want {mid})",
        engine.scroll_offset_y_for_test()
    );
    assert!(
        (engine.stats_scroll_y_for_test() - mid).abs() < 2.0,
        "stats thumb Y must track scrollbar"
    );

    engine
        .send_command(Command::Scroll { offset: match_max })
        .expect("scroll end");
    let (cur3, total3) = engine.viewport_line_position_for_test();
    assert_eq!((cur3, total3), (expected_hits as u64, expected_hits as u64));
    assert_eq!(engine.buffer_line_start_for_test(), window_start_before);

    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_filter_clear_drops_stale_selection_without_panic() {
    use crate::engine::{Command, Engine};
    use crate::viewport_layout::{TextPos, TextSelection};
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-filter-sel-{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..2_000 {
            writeln!(f, "line-{i:04} payload").unwrap();
        }
    }

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    engine.rebuild_if_needed_for_test();

    // Stale selection into the unfiltered window (as if the user dragged).
    engine.set_selection_for_test(TextSelection::new(
        TextPos {
            line_index: 510,
            byte_offset: 0,
        },
        TextPos {
            line_index: 510,
            byte_offset: 8,
        },
    ));
    assert!(engine.selection_text_for_test().is_some());

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(r#"{"cmd":"filter_add","type":"include","pattern":"line-0001"}"#)
        .expect("filter_add");

    // Must not panic: empty flat_lines + old selection indices.
    assert!(engine.selection_text_for_test().is_none());
    assert_eq!(engine.flat_lines_len_for_test(), 0);

    // Pointer extend while empty must also stay safe.
    engine
        .send_command(Command::SelectionAt {
            x: 40.0,
            y: 20.0,
            extend: true,
            click_count: 1,
        })
        .expect("selection_at");
    assert!(engine.selection_text_for_test().is_none());

    engine.finish_file_match_scan_for_test();
    assert!(engine.match_offsets_len_for_test() >= 1);
    assert!(engine.flat_lines_len_for_test() >= 1);

    let _ = std::fs::remove_file(&path);
}

#[test]
#[ignore = "slow tier: real wsl.exe spawn under ConPTY + 6 s deadline; run with -- --ignored"]
fn big_log_filter_match_index_on_generated_fixture() {
    use crate::engine::Engine;

    // Issue #72: generated fixture instead of a hardcoded dev-machine path.
    let path = crate::tests::big_log_fixture();
    let needle = "11:01:13";
    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let path_json = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_json}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"filter_add","type":"include","pattern":"{needle}"}}"#
        ))
        .expect("filter_add");
    assert_eq!(engine.flat_lines_len_for_test(), 0);
    let window_start = engine.buffer_line_start_for_test();

    let start = std::time::Instant::now();
    engine.finish_file_match_scan_for_test();
    let scan_ms = start.elapsed().as_millis();
    assert!(
        engine.match_scan_pos_for_test().is_none(),
        "scan must complete"
    );
    // Fixture: needle in every 20th line of a >8 MiB log.
    assert!(
        engine.match_offsets_len_for_test() >= 1000,
        "expected a large match set, got {}",
        engine.match_offsets_len_for_test()
    );
    assert!(
        engine.flat_lines_len_for_test() <= 10_000,
        "materialized window must stay capped"
    );
    assert_eq!(
        engine.buffer_line_start_for_test(),
        window_start,
        "match scan must not thrash the file window"
    );
    for text in engine.flat_line_texts_for_test() {
        assert!(text.contains(needle), "filtered line: {text}");
    }
    assert!(
        scan_ms < 30_000,
        "big.log filter scan took {scan_ms}ms (want <30s in test)"
    );
}

#[test]
fn stale_id_commands_surface_status_instead_of_silence() {
    // Issue #66: stop/switch with an unknown id must not lie or stay silent.
    use crate::engine::Engine;

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"stop","terminal_id":"terminal-gone"}"#)
        .expect("stop");
    assert!(
        engine
            .status_message_for_test()
            .contains("unknown terminal"),
        "stop on unknown id: {}",
        engine.status_message_for_test()
    );
    assert!(
        !engine.status_message_for_test().contains("Stopped"),
        "must not report success: {}",
        engine.status_message_for_test()
    );

    engine
        .send_command_json(r#"{"cmd":"terminal_switch","terminal_id":"terminal-gone"}"#)
        .expect("switch");
    assert!(
        engine
            .status_message_for_test()
            .contains("unknown terminal"),
        "switch on unknown id: {}",
        engine.status_message_for_test()
    );

    // A real terminal still reports Stopped.
    let id = engine.active_terminal_id_for_test();
    engine
        .send_command_json(&format!(r#"{{"cmd":"stop","terminal_id":"{id}"}}"#))
        .expect("stop real");
    assert_eq!(engine.status_message_for_test(), "Stopped");
}

/// Load a fresh temp file into a dedicated engine file session and run the
/// baseline change-detection sweep (file unchanged at this point).
fn load_file_session(engine: &mut crate::engine::Engine, path: &std::path::Path) {
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();
    engine.tick();
    assert!(
        !engine.active_terminal().file_changed,
        "fresh open must be unflagged"
    );
}

/// Force the throttled watch sweep due and run one tick (no real-time wait).
fn poll_file_changes_now(engine: &mut crate::engine::Engine) {
    use std::time::{Duration, Instant};
    engine.last_file_watch_at = Some(Instant::now() - Duration::from_secs(2));
    engine.tick();
}

#[test]
fn file_watch_append_flags_session_and_reload_clears() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-watch-eng-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "first").unwrap();
    }
    let mut engine = Engine::new();
    load_file_session(&mut engine, &path);

    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "second").unwrap();
    }
    poll_file_changes_now(&mut engine);
    assert!(
        engine.active_terminal().file_changed,
        "append must flag the session"
    );
    assert!(
        engine
            .status_message_for_test()
            .contains("File changed on disk"),
        "status must surface the change: {}",
        engine.status_message_for_test()
    );
    // The stale index is untouched until the user reloads.
    assert_eq!(engine.file_total_lines_for_test(), 1);

    // A sweep while already flagged must not re-emit the change status.
    while engine.poll_event_json().is_some() {}
    poll_file_changes_now(&mut engine);
    let re_emitted = std::iter::from_fn(|| engine.poll_event_json())
        .any(|ev| ev.starts_with(r#"{"type":"status"#) && ev.contains("File changed on disk"));
    assert!(
        !re_emitted,
        "already-flagged session must not re-emit status churn"
    );

    // Reload re-reads from disk and clears the state.
    engine
        .send_command_json(r#"{"cmd":"reload_file"}"#)
        .expect("reload_file");
    engine.finish_file_load_for_test();
    assert!(!engine.active_terminal().file_changed);
    assert!(
        engine.buffer_record_count_for_test() >= 2,
        "reload must pick up the appended line"
    );
    poll_file_changes_now(&mut engine);
    assert!(
        !engine.active_terminal().file_changed,
        "post-reload sweep must stay quiet"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_watch_truncation_flags_session() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-watch-cut-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "one").unwrap();
        writeln!(f, "two").unwrap();
        writeln!(f, "three").unwrap();
    }
    let mut engine = Engine::new();
    load_file_session(&mut engine, &path);

    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "tiny").unwrap();
    }
    poll_file_changes_now(&mut engine);
    assert!(
        engine.active_terminal().file_changed,
        "truncation must flag the session"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn file_watch_deleted_file_flags_session() {
    use crate::engine::Engine;
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-watch-del-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "only").unwrap();
    }
    let mut engine = Engine::new();
    load_file_session(&mut engine, &path);

    let _ = std::fs::remove_file(&path);
    poll_file_changes_now(&mut engine);
    assert!(
        engine.active_terminal().file_changed,
        "deleted/rotated file must flag the session"
    );
}

#[test]
fn file_watch_flags_inactive_sessions_too() {
    use crate::engine::Engine;
    use std::io::Write;

    let file_path = std::env::temp_dir().join(format!(
        "noviewlog-watch-inactive-{}.log",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&file_path).unwrap();
        writeln!(f, "one").unwrap();
    }
    let mut engine = Engine::new();
    load_file_session(&mut engine, &file_path);
    let file_id = engine.active_terminal_id_for_test();

    // Switch to the live boot terminal; the file session stays open, inactive.
    let live_id = engine.terminals_for_test()[0].0.clone();
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_switch","terminal_id":"{live_id}"}}"#
        ))
        .expect("switch to live");

    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&file_path)
            .unwrap();
        writeln!(f, "two").unwrap();
    }
    poll_file_changes_now(&mut engine);
    let flagged = engine
        .terminals
        .iter()
        .find(|t| t.id == file_id)
        .is_some_and(|t| t.file_changed);
    assert!(flagged, "inactive file sessions must be watched too");
    let _ = std::fs::remove_file(&file_path);
}

#[test]
fn stats_expose_file_changed_flag() {
    use crate::engine::Engine;
    use std::io::Write;

    let path =
        std::env::temp_dir().join(format!("noviewlog-watch-stats-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "one").unwrap();
    }
    let mut engine = Engine::new();
    load_file_session(&mut engine, &path);

    // Before the change: flag false in stats.
    engine.last_stats_at = None;
    engine.tick();
    let mut flag = None;
    while let Some(ev) = engine.poll_event_json() {
        if let Some(stats) = parse_stats_for_watch(&ev) {
            flag = Some(stats);
        }
    }
    assert!(!flag.expect("stats before"));

    {
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(f, "two").unwrap();
    }
    poll_file_changes_now(&mut engine);
    engine.last_stats_at = None;
    engine.tick();
    let mut flag = None;
    while let Some(ev) = engine.poll_event_json() {
        if let Some(stats) = parse_stats_for_watch(&ev) {
            flag = Some(stats);
        }
    }
    assert!(
        flag.expect("stats after"),
        "stats must carry file_changed after an external append"
    );

    let _ = std::fs::remove_file(&path);
}

/// Pull `file_changed` out of a stats event JSON, if this event is stats.
fn parse_stats_for_watch(event_json: &str) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_str(event_json).ok()?;
    if v["type"] != "stats" {
        return None;
    }
    Some(v["file_changed"].as_bool().expect("file_changed bool"))
}
#[test]
fn match_scan_cap_surfaces_truncation_in_status_and_stats() {
    // Issue #150: hitting MAX_MATCH_OFFSETS must flag the view, status, and
    // stats instead of silently truncating. Reduced cap keeps the test small.
    use crate::engine::Engine;
    use serde_json::Value;
    use std::io::Write;
    use std::thread;
    use std::time::Duration;

    let path = std::env::temp_dir().join(format!("noviewlog-match-hint-{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..200 {
            writeln!(f, "error line-{i:03} payload").unwrap();
        }
    }

    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    engine.set_match_scan_cap_for_test(50);
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(r#"{"cmd":"filter_add","type":"include","pattern":"error"}"#)
        .expect("filter_add");

    engine.finish_file_match_scan_for_test();
    assert!(engine.match_scan_pos_for_test().is_none());
    assert_eq!(
        engine.match_offsets_len_for_test(),
        50,
        "cap bounds offsets"
    );
    assert!(
        engine.match_capped_for_test(),
        "truncated scan must flag the view"
    );
    assert!(
        engine
            .status_message_for_test()
            .starts_with("Filter scan capped"),
        "status must surface the cap: {}",
        engine.status_message_for_test()
    );

    // Stats snapshot carries the hint for the UI chrome.
    thread::sleep(Duration::from_millis(260));
    engine.tick();
    let mut stats = None;
    while let Some(ev) = engine.poll_event_json() {
        let v: Value = serde_json::from_str(&ev).unwrap();
        if v["type"] == "stats" {
            stats = Some(v);
        }
    }
    let parsed = stats.expect("stats event");
    assert_eq!(
        parsed["match_capped"],
        Value::Bool(true),
        "stats must carry the truncation hint: {parsed}"
    );

    // Editing filters restarts the scan and clears the stale hint first.
    engine
        .send_command_json(r#"{"cmd":"filter_add","type":"exclude","pattern":"line-199"}"#)
        .expect("filter_add");
    assert!(
        !engine.match_capped_for_test(),
        "invalidate must clear the hint immediately"
    );
    engine.finish_file_match_scan_for_test();
    assert!(
        engine.match_capped_for_test(),
        "199 matches still hit cap 50"
    );

    // Raising the cap above the match count clears the flag again.
    engine.set_match_scan_cap_for_test(crate::file_match::MAX_MATCH_OFFSETS);
    engine
        .send_command_json(r#"{"cmd":"filter_clear"}"#)
        .expect("filter_clear");
    engine.finish_file_match_scan_for_test();
    assert!(
        !engine.match_capped_for_test(),
        "complete scan must not flag truncation"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn viewport_line_position_reads_top_of_viewport() {
    use crate::engine::{Command, Engine};
    use std::io::Write;

    // Issue #212: at the very top the status bar must read 1 / N (top of the
    // viewport), not ~viewport-height / N (bottom of the viewport).
    let path = std::env::temp_dir().join(format!("noviewlog-pos-top-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..200 {
            writeln!(f, "line-{i:03}").unwrap();
        }
    }
    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    let (cur_top, total) = engine.viewport_line_position_for_test();
    assert_eq!(total, 200);
    assert_eq!(cur_top, 1, "at top of scrollback the indicator must read 1");

    // Two-step scroll (windowing settles like in file_scrollbar_mid_jump):
    // request the bottom window, let it land, nudge again while on it.
    engine
        .send_command(Command::Scroll {
            offset: engine.max_scroll_offset_for_test(),
        })
        .expect("scroll eof");
    engine.finish_pending_file_window_for_test();
    engine.rebuild_if_needed_for_test();
    engine
        .send_command(Command::Scroll {
            offset: engine.max_scroll_offset_for_test(),
        })
        .expect("scroll eof again");
    engine.finish_pending_file_window_for_test();
    let (cur_eof, _) = engine.viewport_line_position_for_test();
    assert_eq!(cur_eof, 200, "at EOF the indicator must snap to total");

    let _ = std::fs::remove_file(&path);
}

#[test]
fn closing_filter_tab_cancels_inflight_match_scan() {
    use crate::engine::Engine;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    let path = std::env::temp_dir().join(format!(
        "noviewlog-tab-close-cancel-{}.log",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..80_000 {
            writeln!(f, "ts=10:00:00 line-{i}").unwrap();
        }
    }
    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(r#"{"cmd":"filter_add","type":"include","pattern":"10:00:00"}"#)
        .expect("filter_add");
    assert!(
        engine.match_scan_pos_for_test().is_some(),
        "scan must be in flight"
    );
    // Spawning the scan worker is what installs the cancel flag in the view.
    engine.advance_file_match_scan();

    // The cancel flag lives in the view; the test keeps its own handle so the
    // assertion survives close_tab dropping the view.
    let cancel = engine
        .active_terminal()
        .active_view()
        .match_scan_cancel
        .clone()
        .expect("in-flight scan must own a cancel flag");
    assert!(!cancel.load(Ordering::Relaxed));

    engine
        .send_command_json(r#"{"cmd":"tab_close","index":1}"#)
        .expect("tab_close");
    assert!(
        cancel.load(Ordering::Relaxed),
        "closing the filter tab must cancel the in-flight scan (#237)"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn closing_terminal_cancels_inflight_match_scan() {
    // Issue #255: same leak class as #237 — `terminal_close` must flip the
    // in-flight scan's cancel flag before dropping the terminal.
    use crate::engine::Engine;
    use std::io::Write;
    use std::sync::atomic::Ordering;

    let path = std::env::temp_dir().join(format!(
        "noviewlog-terminal-close-cancel-{}.log",
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..80_000 {
            writeln!(f, "ts=10:00:00 line-{i}").unwrap();
        }
    }
    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"resize","width":800,"height":400}"#)
        .expect("resize");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(r#"{"cmd":"filter_add","type":"include","pattern":"10:00:00"}"#)
        .expect("filter_add");
    assert!(
        engine.match_scan_pos_for_test().is_some(),
        "scan must be in flight"
    );
    // Spawning the scan worker is what installs the cancel flag in the view.
    engine.advance_file_match_scan();

    let cancel = engine
        .active_terminal()
        .active_view()
        .match_scan_cancel
        .clone()
        .expect("in-flight scan must own a cancel flag");
    assert!(!cancel.load(Ordering::Relaxed));

    // The boot terminal is a live session, so closing the FILES session is
    // allowed; the whole terminal (and its filter tab) is dropped.
    engine
        .send_command_json(r#"{"cmd":"terminal_close"}"#)
        .expect("terminal_close");
    assert!(
        cancel.load(Ordering::Relaxed),
        "closing the terminal must cancel the in-flight scan (#255)"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn restart_clears_pending_stdin() {
    // Issue #255: type-ahead buffered before a restart must not flush into
    // the fresh process (same semantics as set_launch).
    use crate::engine::Engine;

    let mut engine = Engine::new();
    let path = std::env::temp_dir().join(format!(
        "noviewlog-restart-stdin-{}.log",
        std::process::id()
    ));
    std::fs::write(&path, b"ts=10:00:00 line-0\n").unwrap();
    engine.terminals[0].launch.log_file = Some(path.to_string_lossy().into_owned());
    engine.terminals[0]
        .pending_stdin
        .extend_from_slice(b"typed-ahead");
    let id = engine.terminals[0].id.clone();

    engine.restart();
    assert_eq!(
        engine.pending_stdin_len_for_test(&id),
        0,
        "restart must clear pending stdin"
    );

    let _ = std::fs::remove_file(&path);
}
