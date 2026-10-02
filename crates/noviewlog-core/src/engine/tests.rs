//! Engine unit tests (moved verbatim from mod.rs).
use super::*;

fn drain_events(engine: &mut Engine) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(event) = engine.poll_event_json() {
        out.push(event);
    }
    out
}

#[test]
fn command_needs_active_terminal_classifies_commands() {
    let terminal_free = [
        Command::Resize {
            width: 10,
            height: 10,
        },
        Command::TerminalAdd,
        Command::TerminalClose { terminal_id: None },
        Command::LoadFile {
            path: "x.log".into(),
        },
        Command::SetSettings {
            max_scrollback_lines: 1_000,
        },
        Command::SetViewportFontSize { size: 14.0 },
        Command::FilterDraftSet {
            pattern: "x".into(),
            use_regex: false,
        },
    ];
    for cmd in &terminal_free {
        assert!(
            !Engine::command_needs_active_terminal(cmd),
            "must not need an active terminal: {cmd:?}"
        );
    }
    let needs_terminal = [
        Command::TabAdd,
        Command::ScrollLines { delta: 1 },
        Command::SearchSet {
            query: "x".into(),
            regex: false,
            case_sensitive: false,
            whole_word: false,
        },
        Command::SetFollow { follow: true },
    ];
    for cmd in &needs_terminal {
        assert!(
            Engine::command_needs_active_terminal(cmd),
            "must need an active terminal: {cmd:?}"
        );
    }
}

#[test]
fn send_command_json_rejects_malformed_and_unknown_commands() {
    let mut engine = Engine::new();
    assert!(engine.send_command_json("{not json").is_err());
    assert!(engine
        .send_command_json(r#"{"cmd":"definitely_not_a_command"}"#)
        .is_err());
    // The rejected commands must not dirty the viewport.
    assert!(engine.viewport_dirty);
}

#[test]
fn severity_set_unknown_mode_is_dispatch_error() {
    let mut engine = Engine::new();
    let err = engine
        .send_command(Command::SeveritySet {
            mode: "bogus".into(),
        })
        .expect_err("unknown severity mode must fail");
    assert!(err.contains("unknown severity mode: bogus"), "err={err}");
}

#[test]
fn load_preset_alias_reports_missing_preset_via_status() {
    let mut engine = Engine::new();
    drain_events(&mut engine);
    engine
        .send_command_json(r#"{"cmd":"load_preset","name":"__noviewlog_test_missing_preset__"}"#)
        .expect("alias command itself must succeed");
    let events = drain_events(&mut engine);
    assert!(
        events
            .iter()
            .any(|e| e.contains("Preset not found: __noviewlog_test_missing_preset__")),
        "events={events:?}"
    );
}

#[test]
fn render_error_keeps_dirty_and_success_clears_it() {
    let mut engine = Engine::new();
    assert!(engine.needs_render());
    // Undersized buffer: the host-facing error must keep the dirty flag so
    // the host retries instead of showing a stale frame forever.
    let mut small = [0u8; 64];
    assert!(engine.render(64, 48, &mut small).is_err());
    assert!(engine.needs_render());

    let mut out = vec![0u8; 64 * 48 * 4];
    engine.render(64, 48, &mut out).expect("full-size render");
    assert!(!engine.needs_render());
    // Stopped Terminal tab paints the centered hint over a background.
    assert!(out.iter().any(|&b| b != 0));
}

#[test]
fn idle_tick_emits_stats_only_and_stays_clean() {
    let mut engine = Engine::new();
    // Settle: the first tick builds the initial (empty) view state and
    // may dirty once. Steady state must stay quiet.
    engine.tick();
    assert!(matches!(
        crate::engine::parse_engine_event(&drain_events(&mut engine)[0]),
        Some(crate::engine::EngineEvent::Stats(_))
    ));

    let mut out = vec![0u8; 800 * 600 * 4];
    engine.render(800, 600, &mut out).expect("render");
    assert!(!engine.needs_render());

    // Cross the stats throttle window (250 ms) so the next tick is
    // stats-eligible, mirroring the caret test's timing approach.
    std::thread::sleep(std::time::Duration::from_millis(300));
    engine.tick();
    assert!(
        !engine.needs_render(),
        "idle tick must not dirty the bitmap"
    );
    assert!(!engine.host_work_pending());
    let events = drain_events(&mut engine);
    assert_eq!(events.len(), 1, "exactly the stats snapshot: {events:?}");
    assert!(matches!(
        crate::engine::parse_engine_event(&events[0]),
        Some(crate::engine::EngineEvent::Stats(_))
    ));
    assert!(drain_events(&mut engine).is_empty());
}

#[test]
fn tick_auto_start_launch_loads_log_file_once() {
    let path = std::env::temp_dir().join(format!(
        "noviewlog-tick-autostart-{}.log",
        std::process::id()
    ));
    std::fs::write(&path, "hello\nworld\nwarn: boom\n").expect("write fixture");
    let mut engine = Engine::new();
    engine.auto_start_launch = true;
    engine.terminals[0].launch.log_file = Some(path.to_string_lossy().into_owned());

    assert!(!engine.file_load_pending_for_test());
    engine.tick();
    assert!(engine.process_started_for_test());
    // Small fixtures finish within the same tick (`advance_file_load` runs
    // at the end of `tick`); only assert the pending state afterwards.
    engine.finish_file_load_for_test();
    let records = engine.buffer_record_count_for_test();
    assert!(records >= 3);

    // A later tick must not restart the already-started launch.
    engine.tick();
    engine.finish_file_load_for_test();
    assert!(!engine.file_load_pending_for_test());
    assert_eq!(
        engine.buffer_record_count_for_test(),
        records,
        "second tick must not reload or duplicate records"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn poll_event_json_is_fifo() {
    let mut engine = Engine::new();
    engine.enqueue_event_for_test(r#"{"type":"status","message":"first"}"#.to_string());
    engine.enqueue_event_for_test(r#"{"type":"status","message":"second"}"#.to_string());
    let first = engine.poll_event_json().expect("first event");
    assert!(first.contains("first"));
    let second = engine.poll_event_json().expect("second event");
    assert!(second.contains("second"));
    assert!(engine.poll_event_json().is_none());
}

#[test]
fn stdin_command_without_pty_surfaces_status() {
    let mut engine = Engine::new();
    engine.mark_running_for_test();
    drain_events(&mut engine);
    engine
        .send_command(Command::Stdin {
            text: String::new(),
            bytes: Some(b"x".to_vec()),
        })
        .expect("stdin dispatch");
    let events = drain_events(&mut engine);
    assert!(
        events
            .iter()
            .any(|e| e.contains("stdin: no pty for terminal")),
        "events={events:?}"
    );
}

#[test]
fn stdin_on_stopped_filter_tab_is_dropped() {
    let mut engine = Engine::new();
    engine.send_command(Command::TabAdd).expect("tab add");
    assert_eq!(engine.active_tab_index_for_test(), 1);
    drain_events(&mut engine);
    // Not running + non-Terminal tab: keystrokes are ignored, no error spam.
    engine.handle_key(b"x");
    assert!(drain_events(&mut engine).is_empty());
}

#[test]
fn selection_clear_command_clears_selection_and_dirties() {
    let mut engine = Engine::new();
    let mut out = vec![0u8; 64 * 48 * 4];
    engine.render(64, 48, &mut out).expect("render");
    engine.active_terminal_mut().selection = Some(crate::viewport_layout::TextSelection::default());
    engine
        .send_command(Command::SelectionClear)
        .expect("selection clear");
    assert!(engine.active_terminal().selection.is_none());
    assert!(engine.needs_render());
}

#[test]
fn pty_ingest_dirty_throttle_cadence() {
    let mut engine = Engine::new();
    engine.viewport_dirty = false;

    // Never painted: flood frames must dirty immediately.
    engine.mark_viewport_dirty_after_pty_ingest(true);
    assert!(engine.viewport_dirty);

    // Just painted: throttled while more flood is pending.
    engine.note_viewport_painted();
    engine.viewport_dirty = false;
    engine.mark_viewport_dirty_after_pty_ingest(true);
    assert!(
        !engine.viewport_dirty,
        "throttle must skip mid-flood frames"
    );

    // Tail of the flood: always dirty so the tail becomes visible.
    engine.mark_viewport_dirty_after_pty_ingest(false);
    assert!(engine.viewport_dirty);
}

#[test]
fn pty_work_pending_tracks_hold_and_drain() {
    let mut engine = Engine::new();
    assert!(!engine.pty_work_pending());
    assert!(!engine.take_pty_drain_pending());

    engine.pty_drain_pending = true;
    assert!(engine.pty_work_pending());
    assert!(engine.take_pty_drain_pending());
    assert!(!engine.pty_work_pending());

    engine.pty_hold = Some(PtyEvent::Bytes {
        id: "t".into(),
        data: vec![b'x'],
        generation: 1,
    });
    assert!(engine.pty_work_pending());
    engine.last_pty_poll_at = Some(Instant::now());
    assert!(
        engine.defer_pty_reader_wake(),
        "recent poll with pending flood must defer the reader wake"
    );
}

#[test]
fn flush_persist_test_mode_clears_dirty_without_writes() {
    let mut engine = Engine::new();
    // mark_config_dirty is a no-op while persistence is skipped in tests.
    engine.mark_config_dirty();
    assert!(!engine.config_dirty);

    engine.config_dirty = true;
    engine.projects_dirty = true;
    engine.persist_changed_at = Some(Instant::now());
    engine.flush_persist();
    assert!(!engine.config_dirty);
    assert!(!engine.projects_dirty);
    assert!(engine.persist_changed_at.is_none());
}

#[test]
fn persist_retry_backoff_grows_times_four_and_caps() {
    use crate::engine::persist::next_persist_retry_delay;
    let mut delay = PERSIST_DEBOUNCE;
    assert_eq!(delay, Duration::from_millis(750));
    delay = next_persist_retry_delay(delay);
    assert_eq!(delay, Duration::from_millis(3000));
    delay = next_persist_retry_delay(delay);
    assert_eq!(delay, Duration::from_millis(12000));
    delay = next_persist_retry_delay(delay);
    assert_eq!(delay, PERSIST_RETRY_MAX, "48 s must clamp to the 30 s cap");
    assert_eq!(next_persist_retry_delay(delay), PERSIST_RETRY_MAX);
}

#[test]
fn persist_failure_retries_with_backoff_and_announces_once() {
    let mut engine = Engine::new();
    // Failure is injected: no real config write happens, but take the lock
    // anyway in case recovery below lands a save alongside other tests.
    let _guard = crate::tests::USER_CONFIG_LOCK
        .lock()
        .expect("user config lock");
    engine.skip_projects_persist = false;
    engine.persist_fail_saves = true;
    engine.mark_config_dirty();
    assert!(engine.config_dirty);

    engine.flush_persist();
    assert!(
        engine.config_dirty,
        "failed save must stay dirty (issue #109)"
    );
    assert_eq!(engine.persist_retry_delay, Duration::from_millis(3000));
    assert_eq!(
        drain_events(&mut engine).len(),
        1,
        "first failure announces"
    );

    // Later retries stay silent while the delay keeps growing.
    engine.flush_persist();
    engine.flush_persist();
    engine.flush_persist();
    assert_eq!(
        engine.persist_retry_delay, PERSIST_RETRY_MAX,
        "backoff caps at 30 s"
    );
    assert_eq!(
        drain_events(&mut engine).len(),
        0,
        "retries must not re-announce"
    );

    // A successful save clears the backoff and re-arms status reporting.
    engine.persist_fail_saves = false;
    engine.flush_persist();
    assert!(!engine.config_dirty);
    assert_eq!(engine.persist_retry_delay, PERSIST_DEBOUNCE);
    assert!(!engine.config_failure_announced);
    engine.skip_projects_persist = true;
}

#[test]
fn fresh_dirty_mark_resets_backoff_so_new_edit_persists_promptly() {
    let mut engine = Engine::new();
    // Lock FIRST, then redirect the config dir: the final flush really saves
    // config.yaml, and it must land in a temp dir, not the developer's.
    let _config_lock = crate::tests::USER_CONFIG_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _dir = crate::tests::ConfigDirGuard::new("persist-fresh-edit");
    engine.skip_projects_persist = false;
    engine.persist_fail_saves = true;

    // Failure streak drives the retry delay to the 30 s cap.
    engine.mark_config_dirty();
    for _ in 0..4 {
        engine.flush_persist();
    }
    assert_eq!(engine.persist_retry_delay, PERSIST_RETRY_MAX);

    // A NEW user edit (cause resolved) re-arms the normal debounce: the very
    // next flush after the debounce window must succeed, not wait 30 s.
    engine.persist_fail_saves = false;
    engine.mark_config_dirty();
    assert_eq!(
        engine.persist_retry_delay, PERSIST_DEBOUNCE,
        "a fresh dirty mark must reset the backoff (issue #253)"
    );
    engine.persist_changed_at = Some(Instant::now() - PERSIST_DEBOUNCE - Duration::from_millis(1));
    engine.flush_persist();
    assert!(!engine.config_dirty, "fresh edit must persist immediately");
    engine.skip_projects_persist = true;
}

#[test]
fn persist_failure_announcement_is_tracked_per_store() {
    let mut engine = Engine::new();
    // Lock FIRST, then redirect the config dir: the recovery flush really
    // saves both stores, and they must land in a temp dir.
    let _config_lock = crate::tests::USER_CONFIG_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let _dir = crate::tests::ConfigDirGuard::new("persist-per-store");
    engine.skip_projects_persist = false;

    // Both stores fail: each announces under its OWN store, once.
    engine.persist_fail_saves = true;
    engine.mark_config_dirty();
    engine.persist_projects_store();
    engine.flush_persist();
    assert!(engine.config_failure_announced);
    assert!(engine.projects_failure_announced);
    let events = drain_events(&mut engine);
    assert_eq!(
        events.len(),
        2,
        "one announcement per store, got {events:?}"
    );
    assert!(events.iter().any(|e| e.contains("projects")));
    assert!(events.iter().any(|e| e.contains("Config")));

    // Later retries of the same unwritten changes stay silent.
    engine.flush_persist();
    engine.flush_persist();
    assert_eq!(
        drain_events(&mut engine).len(),
        0,
        "retries must not re-announce"
    );

    // Both stores save: each store's announcement clears with its own
    // success (issue #253 per-store tracking).
    engine.persist_fail_saves = false;
    engine.flush_persist();
    assert!(!engine.projects_dirty);
    assert!(!engine.config_dirty);
    assert!(!engine.projects_failure_announced);
    assert!(!engine.config_failure_announced);
    engine.skip_projects_persist = true;
}

#[test]
fn disconnected_file_load_fails_and_unwedges_host_work_pending() {
    let mut engine = Engine::new();
    // Simulate a worker that died right after channel setup: the handle's
    // channel is disconnected and will never deliver Done/Failed (issue #253).
    engine.terminals[0].file_load = Some(crate::file_load::disconnected_file_load_handle_for_test(
        "stalled.log",
    ));
    assert!(engine.host_work_pending());

    engine.advance_file_load();
    assert!(
        engine.terminals[0].file_load.is_none(),
        "disconnected load must convert to Failed, not stay pending"
    );
    assert!(
        !engine.host_work_pending(),
        "a dead load must not wedge the fast tick cadence"
    );
    let events = drain_events(&mut engine);
    assert!(
        events.iter().any(|e| e.contains("stalled")),
        "stall must surface a status event, got {events:?}"
    );
}

#[test]
fn file_load_stall_timeout_backstop_pure_logic() {
    let now = Instant::now();
    assert!(!TerminalState::file_load_stall_expired(now, now));
    assert!(!TerminalState::file_load_stall_expired(
        now - crate::file_load::FILE_LOAD_STALL_TIMEOUT + Duration::from_secs(1),
        now
    ));
    assert!(TerminalState::file_load_stall_expired(
        now - crate::file_load::FILE_LOAD_STALL_TIMEOUT,
        now
    ));
    assert!(TerminalState::file_load_stall_expired(
        now - crate::file_load::FILE_LOAD_STALL_TIMEOUT - Duration::from_secs(60),
        now
    ));
}

#[test]
fn stalled_file_load_does_not_report_host_work() {
    let mut engine = Engine::new();
    engine.terminals[0].file_load = Some(crate::file_load::disconnected_file_load_handle_for_test(
        "stalled.log",
    ));
    // Backstop: even while the handle lingers, once the quiet period is past
    // the timeout the engine must stop reporting host work (issue #253).
    engine.terminals[0].file_load_stalled_at =
        Some(Instant::now() - crate::file_load::FILE_LOAD_STALL_TIMEOUT - Duration::from_secs(1));
    assert!(
        !engine.host_work_pending(),
        "a load stalled past the timeout must not pin the fast cadence"
    );
    engine.advance_file_load();
    assert!(engine.terminals[0].file_load.is_none());
}
