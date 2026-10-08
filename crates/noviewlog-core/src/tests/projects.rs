//! Project / Program restore and Run/Stop lifecycle.

use crate::core::types::{
    FilterRule, FilterType, LaunchConfig, ProgramConfig, ProjectConfig, ProjectsStore, TabConfig,
    WorkspaceConfig,
};
use crate::engine::Engine;

fn engine_isolated() -> Engine {
    let mut engine = Engine::new();
    engine.skip_projects_persist = true;
    engine.projects = ProjectsStore::default();
    engine.active_project = None;
    engine
}

#[test]
fn project_open_selects_terminal_tab_and_stays_stopped() {
    let mut engine = engine_isolated();
    let mut project = sample_project("project-1", "Demo");
    project.programs[0].launch.cwd = Some("/tmp".into());
    project.programs[0].workspace = WorkspaceConfig {
        tabs: vec![
            TabConfig {
                name: "Terminal".into(),
                filters: vec![],
                search_query: String::new(),
                search_regex: false,
                search_case_sensitive: false,
                search_whole_word: false,
                auto_follow: true,
                wrap_lines: true,
                severity: Default::default(),
            },
            TabConfig {
                name: "Errors".into(),
                filters: vec![FilterRule {
                    id: "f1".into(),
                    name: None,
                    filter_type: FilterType::Include,
                    pattern: "error".into(),
                    enabled: true,
                    use_regex: false,
                    regex: None,
                }],
                search_query: String::new(),
                search_regex: false,
                search_case_sensitive: false,
                search_whole_word: false,
                auto_follow: true,
                wrap_lines: true,
                severity: Default::default(),
            },
        ],
        active_tab: 1,
    };
    engine.projects = ProjectsStore {
        projects: vec![project],
        active_project: 0,
    };

    engine
        .send_command_json(r#"{"cmd":"project_open","project_id":"project-1"}"#)
        .expect("open");

    let live = engine.terminals_for_test();
    assert_eq!(live.len(), 2);
    assert_eq!(live[0].1, "API");
    assert_eq!(live[1].1, "Worker");
    assert_eq!(engine.tab_configs_for_test().len(), 2);
    // Live TERMINALS restore always lands on Terminal (view 0), even when the
    // saved workspace had a filter tab active.
    assert_eq!(engine.active_tab_index_for_test(), 0);
    assert_eq!(engine.active_view_name_for_test(), "Terminal");
    for (id, _, running) in &live {
        assert!(
            !*running,
            "project open must leave live Programs stopped: {id}"
        );
        assert!(
            !engine.has_pty_for_test(id),
            "project open must not spawn a PTY: {id}"
        );
    }
    assert!(
        !engine.process_started_for_test(),
        "project open must not start the saved command"
    );
    assert!(!engine.active_terminal_running_for_test());
    engine.tick();
    assert!(
        !engine.process_started_for_test() && !engine.active_terminal_running_for_test(),
        "tick after project open must not auto-start"
    );
    assert!(engine.active_project.is_some());

    engine
        .send_command_json(r#"{"cmd":"terminal_start"}"#)
        .expect("start");
    assert!(
        engine.process_started_for_test(),
        "manual Start must run the saved command"
    );
}

#[test]
fn stopped_empty_viewport_messages_are_ascii_without_play_glyph() {
    use crate::engine::{EMPTY_FILTER_TAB_STOPPED, EMPTY_TERMINAL_TAB_STOPPED};
    for msg in [EMPTY_TERMINAL_TAB_STOPPED, EMPTY_FILTER_TAB_STOPPED] {
        assert!(
            !msg.contains('▶') && !msg.contains("Press ▶"),
            "empty hint must not use play glyph: {msg}"
        );
    }
    assert!(
        EMPTY_FILTER_TAB_STOPPED.contains("TERMINALS"),
        "filter-tab hint must point at the TERMINALS Start control"
    );
}

#[test]
fn project_create_starts_empty_and_does_not_copy_previous() {
    let mut engine = engine_isolated();
    engine
        .send_command_json(
            r#"{"cmd":"program_set_launch","command":"echo","args":["a"],"cwd":"/tmp"}"#,
        )
        .expect("launch");
    let tid = engine.active_terminal_id_for_test();
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_rename","terminal_id":"{tid}","name":"One"}}"#
        ))
        .expect("rename");
    engine.terminal_add_blank_for_test();

    engine
        .send_command_json(r#"{"cmd":"project_create","name":"MyProj"}"#)
        .expect("create");
    assert_eq!(engine.projects.projects.len(), 1);
    assert!(
        engine.projects.projects[0].programs.is_empty(),
        "new Project must not snapshot live TERMINALS"
    );
    assert_eq!(engine.active_project, Some(0));

    let live = engine.terminals_for_test();
    assert_eq!(live.len(), 1, "empty Project opens as one Terminal");
    assert_ne!(live[0].1, "One");
    // Empty Project: one stopped Terminal; no shell until the user types or Starts.
    assert!(!live[0].2, "empty Project Terminal must stay stopped");
    assert!(
        !engine.has_pty_for_test(&live[0].0),
        "empty Project must not spawn a PTY"
    );
    assert!(!engine.process_started_for_test());
    assert!(engine
        .status_message_for_test()
        .contains("Created project: MyProj"));
}

#[test]
fn exit_with_launch_command_does_not_respawn_shell() {
    let mut engine = engine_isolated();
    engine
        .send_command_json(r#"{"cmd":"program_set_launch","command":"true"}"#)
        .expect("launch");
    let id = engine.active_terminal_id_for_test();
    // Pretend a PTY was running with generation 1.
    engine.set_pty_generation_for_test(1);
    engine.mark_running_for_test();
    engine.inject_pty_exit_for_test(&id, 0, 1);
    engine.poll_pty_for_test();

    assert!(!engine.active_terminal_running_for_test());
    assert!(
        !engine.has_pty_for_test(&id),
        "must not respawn interactive shell when launch.command is set"
    );
}

/// Real-PTY regression for Windows ConPTY: typing `exit` in a plain shell used
/// to hang the session because the reader blocked on the pipe forever (conhost
/// holds the write end until the master is dropped), so `Exit` was never sent
/// and the shell never respawned. Must also stay green on Linux.
#[test]
#[ignore = "slow tier: real shell spawn + 6 s wall-clock deadline; run with -- --ignored"]
fn typing_exit_respawns_interactive_shell() {
    use std::time::{Duration, Instant};

    let mut engine = engine_isolated();
    engine.start_interactive_shell();

    // Generous deadlines: a cold shell spawn (powershell + profile) can take
    // seconds and the suite runs under parallel load; this is a respawn
    // regression test, not a latency one (flake seen at 6s).
    const SHELL_DEADLINE: Duration = Duration::from_secs(30);

    // First shell must be running and show prompt/banner output.
    let deadline = Instant::now() + SHELL_DEADLINE;
    while Instant::now() < deadline {
        engine.poll_pty_for_test();
        engine.tick();
        if engine.active_terminal_running_for_test()
            && !engine.live_screen_text_for_test().is_empty()
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(
        engine.active_terminal_running_for_test(),
        "first interactive shell must be running"
    );
    assert!(
        !engine.live_screen_text_for_test().is_empty(),
        "first interactive shell must show prompt output"
    );

    let id = engine.active_terminal_id_for_test();
    let gen_before = engine.pty_generation_for_test();
    engine.handle_key(b"exit\r\n");

    // Respawn: running again, exit_code reset, generation bumped.
    let deadline = Instant::now() + SHELL_DEADLINE;
    while Instant::now() < deadline {
        engine.poll_pty_for_test();
        engine.tick();
        if engine.active_terminal_running_for_test()
            && engine.pty_generation_for_test() == gen_before.wrapping_add(1)
        {
            assert!(
                engine.has_pty_for_test(&id),
                "respawned shell must keep a live PTY"
            );
            assert_eq!(
                engine.exit_code_for_test(),
                None,
                "respawned shell must reset exit_code"
            );
            assert!(
                engine.active_terminal_running_for_test(),
                "shell must be running again after exit"
            );
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    panic!(
        "typing exit must respawn the interactive shell (running={}, generation={} want {})",
        engine.active_terminal_running_for_test(),
        engine.pty_generation_for_test(),
        gen_before.wrapping_add(1)
    );
}

/// Explicit Stop must suppress the waiter's late `Exit`: a stopped plain shell
/// must not auto-respawn (no launch command, generation unchanged), and the
/// "Stopped" status / exit_code must stay intact.
#[ignore = "slow tier: real shell spawn + 6 s wall-clock deadline; run with -- --ignored"]
#[test]
fn stop_keeps_shell_stopped_without_late_exit_respawn() {
    use std::time::{Duration, Instant};

    let mut engine = engine_isolated();
    engine.start_interactive_shell();

    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        engine.poll_pty_for_test();
        engine.tick();
        if engine.active_terminal_running_for_test() {
            break;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    assert!(
        engine.active_terminal_running_for_test(),
        "interactive shell must be running"
    );

    let id = engine.active_terminal_id_for_test();
    let gen = engine.pty_generation_for_test();
    engine.send_command_json(r#"{"cmd":"stop"}"#).expect("stop");
    assert!(!engine.active_terminal_running_for_test());
    assert!(!engine.has_pty_for_test(&id));

    // The killed child's waiter finishes now; an unsuppressed late Exit would
    // respawn the shell right here. Poll past the child exit and assert it
    // stays stopped.
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        engine.poll_pty_for_test();
        engine.tick();
        assert!(
            !engine.active_terminal_running_for_test(),
            "Stop must not respawn the shell"
        );
        assert!(
            !engine.has_pty_for_test(&id),
            "no PTY may reappear after Stop"
        );
        std::thread::sleep(Duration::from_millis(40));
    }
    assert_eq!(
        engine.exit_code_for_test(),
        None,
        "Stop must not overwrite exit_code"
    );
    assert_eq!(engine.pty_generation_for_test(), gen);
    assert!(
        engine.status_message_for_test().contains("Stopped"),
        "status must stay Stopped: {}",
        engine.status_message_for_test()
    );
}

#[test]
fn typing_after_program_exit_does_not_spawn_shell() {
    let mut engine = engine_isolated();
    engine
        .send_command_json(r#"{"cmd":"program_set_launch","command":"uname","args":["-a"]}"#)
        .expect("launch");
    engine.push_lines_for_test([
        "Linux test-host uname-a-output".into(),
        "trailing-line".into(),
    ]);
    let id = engine.active_terminal_id_for_test();
    engine.set_pty_generation_for_test(1);
    engine.mark_running_for_test();
    engine.inject_pty_exit_for_test(&id, 0, 1);
    engine.poll_pty_for_test();
    assert!(!engine.active_terminal_running_for_test());

    engine.handle_key(b"\r");
    engine.poll_pty_for_test();

    assert!(
        !engine.active_terminal_running_for_test(),
        "Enter after a Program exits must not start a shell"
    );
    assert!(
        !engine.has_pty_for_test(&id),
        "Enter after a Program exits must not create a PTY"
    );
    let texts = engine.flat_line_texts_for_test();
    let uname_hits = texts
        .iter()
        .filter(|l| l.contains("uname-a-output"))
        .count();
    assert_eq!(
        uname_hits, 1,
        "finished Program output must not be duplicated, got {texts:?}"
    );
}

#[test]
fn program_start_clears_previous_scrollback() {
    let mut engine = engine_isolated();
    #[cfg(windows)]
    engine
        .send_command_json(
            r#"{"cmd":"program_set_launch","command":"cmd","args":["/c","echo","ok"]}"#,
        )
        .expect("launch");
    #[cfg(not(windows))]
    engine
        .send_command_json(r#"{"cmd":"program_set_launch","command":"true"}"#)
        .expect("launch");
    engine.push_lines_for_test(["OLD-BANNER-LINE".into(), "second-line".into()]);
    assert!(
        engine.buffer_record_count_for_test() > 0,
        "precondition: leftover scrollback in the record buffer"
    );
    engine
        .send_command_json(r#"{"cmd":"terminal_start"}"#)
        .expect("start");
    let leftover = engine
        .active_terminal()
        .buffer
        .raw_lines()
        .iter()
        .any(|l| l.contains("OLD-BANNER-LINE"));
    assert!(
        !leftover,
        "Start must drop the previous session, got {:?}",
        engine.active_terminal().buffer.raw_lines()
    );
}

#[test]
fn projects_store_yaml_round_trip() {
    let store = ProjectsStore {
        projects: vec![ProjectConfig {
            id: "project-rt".into(),
            name: "Round".into(),
            default_cwd: None,
            path_hint: None,
            programs: vec![ProgramConfig {
                id: "program-rt".into(),
                name: "Main".into(),
                launch: LaunchConfig {
                    command: Some("npm".into()),
                    args: vec!["run".into(), "dev".into()],
                    cwd: Some("/home/me/app".into()),
                    ..LaunchConfig::default()
                },
                workspace: WorkspaceConfig {
                    tabs: vec![TabConfig {
                        name: "Terminal".into(),
                        filters: vec![],
                        search_query: String::new(),
                        search_regex: false,
                        search_case_sensitive: false,
                        search_whole_word: false,
                        auto_follow: true,
                        wrap_lines: true,
                        severity: Default::default(),
                    }],
                    active_tab: 0,
                },
            }],
            active_program: 0,
        }],
        active_project: 0,
    };
    let yaml = serde_yaml::to_string(&store).expect("to yaml");
    let parsed: ProjectsStore = serde_yaml::from_str(&yaml).expect("from yaml");
    assert_eq!(parsed.projects.len(), 1);
    assert_eq!(
        parsed.projects[0].programs[0].launch.command.as_deref(),
        Some("npm")
    );
    assert_eq!(
        parsed.projects[0].programs[0].launch.args,
        vec!["run", "dev"]
    );
}

fn sample_project(id: &str, name: &str) -> ProjectConfig {
    ProjectConfig {
        id: id.into(),
        name: name.into(),
        default_cwd: None,
        path_hint: None,
        programs: vec![
            ProgramConfig {
                id: "program-a".into(),
                name: "API".into(),
                launch: LaunchConfig {
                    command: Some("echo".into()),
                    args: vec!["hello".into()],
                    ..LaunchConfig::default()
                },
                workspace: WorkspaceConfig::default(),
            },
            ProgramConfig {
                id: "program-b".into(),
                name: "Worker".into(),
                launch: LaunchConfig {
                    command: Some("sleep".into()),
                    args: vec!["1".into()],
                    ..LaunchConfig::default()
                },
                workspace: WorkspaceConfig::default(),
            },
        ],
        active_program: 0,
    }
}

#[test]
fn startup_restores_last_project_and_stays_stopped() {
    let mut engine = engine_isolated();
    engine.projects = ProjectsStore {
        projects: vec![
            sample_project("project-1", "First"),
            sample_project("project-2", "Second"),
        ],
        active_project: 1,
    };

    let restored = engine.finish_startup(LaunchConfig::default());
    assert!(restored);
    assert_eq!(engine.active_project, Some(1));
    assert_eq!(engine.active_terminal_index_for_test(), 0);
    assert_eq!(engine.active_tab_index_for_test(), 0);

    let live = engine.terminals_for_test();
    assert_eq!(live.len(), 2);
    assert_eq!(live[0].1, "API");
    assert_eq!(live[1].1, "Worker");
    for (id, _, running) in &live {
        assert!(
            !*running,
            "startup restore must leave Programs stopped: {id}"
        );
        assert!(
            !engine.has_pty_for_test(id),
            "startup restore must not spawn a PTY: {id}"
        );
    }
    assert!(
        !engine.process_started_for_test(),
        "restoring last Project must not auto-start"
    );
    engine.tick();
    assert!(
        !engine.process_started_for_test() && !engine.active_terminal_running_for_test(),
        "tick after last-Project restore must not auto-start"
    );
    assert!(engine
        .status_message_for_test()
        .contains("Opened project: Second"));
}

#[test]
fn startup_cli_launch_skips_project_restore() {
    let mut engine = engine_isolated();
    engine.projects = ProjectsStore {
        projects: vec![sample_project("project-1", "Demo")],
        active_project: 0,
    };

    let restored = engine.finish_startup(LaunchConfig {
        log_file: Some("/tmp/access.log".into()),
        ..LaunchConfig::default()
    });
    assert!(!restored);
    assert!(engine.active_project.is_none());
    assert!(engine.active_is_file_session_for_test());
    assert_eq!(engine.terminals_for_test().len(), 1);
}

#[test]
fn startup_no_projects_keeps_boot_terminal() {
    let mut engine = engine_isolated();
    engine.projects = ProjectsStore::default();

    let restored = engine.finish_startup(LaunchConfig::default());
    assert!(!restored);
    assert!(engine.active_project.is_none());
    assert_eq!(engine.terminals_for_test().len(), 1);
    assert!(!engine.active_terminal_running_for_test());
}

#[test]
fn active_project_saves_and_restores_file_sessions() {
    use std::io::Write;

    let path = std::env::temp_dir().join(format!("noviewlog-proj-file-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "nginx line").unwrap();
    }
    let mut engine = engine_isolated();
    engine
        .send_command_json(r#"{"cmd":"project_create","name":"Logs"}"#)
        .expect("create");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    let programs = &engine.projects.projects[0].programs;
    assert!(
        programs.iter().any(|p| {
            p.launch.log_file.as_deref() == Some(path.to_str().unwrap())
                || p.launch.log_file.as_ref().is_some_and(|s| {
                    s.replace('\\', "/") == path.to_string_lossy().replace('\\', "/")
                })
        }),
        "open file must snapshot log_file onto the active Project: {:?}",
        programs
            .iter()
            .map(|p| p.launch.log_file.clone())
            .collect::<Vec<_>>()
    );

    let project_id = engine.projects.projects[0].id.clone();
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"project_open","project_id":"{project_id}"}}"#
        ))
        .expect("reopen");

    let files = engine.file_session_ids_for_test();
    assert_eq!(files.len(), 1);
    assert_eq!(engine.file_session_paths_for_test().len(), 1);
    assert!(!engine.active_is_file_session_for_test());

    engine
        .send_command_json(&format!(
            r#"{{"cmd":"terminal_switch","terminal_id":"{}"}}"#,
            files[0]
        ))
        .expect("switch");
    engine.finish_file_load_for_test();
    assert!(engine.active_is_file_session_for_test());
    assert!(engine.file_backed_for_test());
    assert!(engine.buffer_record_count_for_test() >= 1);

    let _ = std::fs::remove_file(&path);
}

// Issue #234: restored FILE sessions must finish loading on engine ticks while
// a live terminal stays active — the user must not have to click each tab.
#[test]
fn project_open_restored_files_load_without_activation() {
    use std::io::Write;

    let path =
        std::env::temp_dir().join(format!("noviewlog-proj-bgload-{}.log", std::process::id()));
    {
        let mut f = std::fs::File::create(&path).unwrap();
        for i in 0..8 {
            writeln!(f, "bg line {i}").unwrap();
        }
    }
    let mut engine = engine_isolated();
    engine
        .send_command_json(r#"{"cmd":"project_create","name":"BgLogs"}"#)
        .expect("create");
    let path_str = path.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_str}"}}"#))
        .expect("load_file");
    engine.finish_file_load_for_test();

    let project_id = engine.projects.projects[0].id.clone();
    // Live terminal so the resume tab is NOT the file session.
    engine.terminal_add_blank_for_test();
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"project_open","project_id":"{project_id}"}}"#
        ))
        .expect("reopen");

    assert!(
        !engine.active_is_file_session_for_test(),
        "a live terminal must stay active after restore"
    );
    assert!(
        engine.file_loads_pending_any_for_test(),
        "restored file session should have a load in flight"
    );

    // No tab switch: plain ticks must complete the background load.
    engine.finish_all_file_loads_for_test();
    assert_eq!(
        engine.file_backed_count_for_test(),
        1,
        "file session must become file-backed without activation"
    );
    assert!(
        !engine.file_loads_pending_any_for_test(),
        "no load may be left pending after draining"
    );

    let _ = std::fs::remove_file(&path);
}

#[test]
fn project_open_replaces_leftover_files() {
    use std::io::Write;

    let path_a = std::env::temp_dir().join(format!("noviewlog-proj-a-{}.log", std::process::id()));
    let path_b = std::env::temp_dir().join(format!("noviewlog-proj-b-{}.log", std::process::id()));
    for p in [&path_a, &path_b] {
        let mut f = std::fs::File::create(p).unwrap();
        writeln!(f, "x").unwrap();
    }
    let mut engine = engine_isolated();
    engine
        .send_command_json(r#"{"cmd":"project_create","name":"A"}"#)
        .expect("create A");
    let id_a = engine.projects.projects[0].id.clone();
    let path_a_str = path_a.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_a_str}"}}"#))
        .expect("load A");
    engine.finish_file_load_for_test();

    engine
        .send_command_json(r#"{"cmd":"project_create","name":"B"}"#)
        .expect("create B");
    let path_b_str = path_b.to_string_lossy().replace('\\', "\\\\");
    engine
        .send_command_json(&format!(r#"{{"cmd":"load_file","path":"{path_b_str}"}}"#))
        .expect("load B");
    engine.finish_file_load_for_test();
    assert_eq!(engine.file_session_ids_for_test().len(), 1);

    engine
        .send_command_json(&format!(
            r#"{{"cmd":"project_open","project_id":"{id_a}"}}"#
        ))
        .expect("open A");
    let restored = engine.file_session_paths_for_test();
    assert_eq!(restored.len(), 1);
    let restored_norm = restored[0].replace('\\', "/");
    let expect_a = path_a.to_string_lossy().replace('\\', "/");
    assert_eq!(restored_norm, expect_a);

    let _ = std::fs::remove_file(&path_a);
    let _ = std::fs::remove_file(&path_b);
}

#[test]
fn program_set_launch_keeps_wsl_through_project_open() {
    let mut engine = engine_isolated();
    engine
        .send_command_json(r#"{"cmd":"project_create","name":"WslProj"}"#)
        .expect("create");
    engine
        .send_command_json(
            r#"{"cmd":"program_set_launch","command":"uname","args":["-a"],"cwd":"/home/me","wsl":true,"wsl_distro":"Ubuntu"}"#,
        )
        .expect("launch");

    let launch = &engine.active_terminal().launch;
    assert!(launch.wsl, "Edit Launch must keep wsl");
    assert_eq!(launch.command.as_deref(), Some("uname"));
    assert_eq!(launch.args, vec!["-a".to_string()]);
    assert_eq!(launch.cwd.as_deref(), Some("/home/me"));
    assert_eq!(launch.wsl_distro.as_deref(), Some("Ubuntu"));

    let stored = &engine.projects.projects[0].programs[0].launch;
    assert!(stored.wsl);
    assert_eq!(stored.wsl_distro.as_deref(), Some("Ubuntu"));

    let project_id = engine.projects.projects[0].id.clone();
    engine
        .send_command_json(&format!(
            r#"{{"cmd":"project_open","project_id":"{project_id}"}}"#
        ))
        .expect("reopen");
    let restored = &engine.active_terminal().launch;
    assert!(restored.wsl);
    assert_eq!(restored.command.as_deref(), Some("uname"));
    assert_eq!(restored.wsl_distro.as_deref(), Some("Ubuntu"));
    assert_eq!(restored.cwd.as_deref(), Some("/home/me"));
    assert!(
        !engine.active_terminal_running_for_test() && !engine.process_started_for_test(),
        "reopen must keep the WSL Program stopped until Start"
    );
}
