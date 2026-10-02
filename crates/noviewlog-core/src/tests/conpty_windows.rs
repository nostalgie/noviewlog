//! Real-ConPTY behavior tests (Windows host, `#[ignore]` by default).
//!
//! Run on a Windows dev host:
//!
//! ```text
//! cargo test -p noviewlog-core --lib conpty_windows -- --ignored --test-threads=1
//! ```
//!
//! These spawn a real `cmd.exe` under ConPTY (issue #71): output arrival,
//! typed-line CRLF conventions, resize reflow and kill-during-output ordering.

use crate::core::types::LaunchConfig;
use crate::engine::Engine;
use std::time::{Duration, Instant};

fn pump_until(
    engine: &mut Engine,
    mut done: impl FnMut(&Engine, &str) -> bool,
    seconds: u64,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    while Instant::now() < deadline {
        engine.poll_pty_for_test();
        engine.tick();
        let screen = engine.live_screen_text_for_test();
        if done(engine, &screen) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn spawn_interactive_cmd() -> Engine {
    let mut engine = Engine::new();
    engine.skip_projects_persist = true;
    engine.set_launch(LaunchConfig {
        command: Some("cmd".into()),
        args: vec!["/d".into(), "/k".into(), "prompt $G".into()],
        cwd: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        ..LaunchConfig::default()
    });
    engine
}

#[cfg(windows)]
#[test]
#[ignore = "real ConPTY; run with --ignored on a Windows host"]
fn conpty_typed_line_echoes_resizes_and_exits() {
    let mut engine = spawn_interactive_cmd();
    assert!(
        engine.active_terminal_running_for_test(),
        "cmd spawn must start: {}",
        engine.status_message_for_test()
    );

    // Prompt/banner output reaches the live screen.
    let got_prompt = pump_until(&mut engine, |_, screen| !screen.is_empty(), 15);
    assert!(got_prompt, "interactive cmd must show prompt output");

    // Typed line (CRLF conventions) executes and its output is visible.
    engine.handle_key(b"echo NOVIEWLOG_MARKER\r");
    let got_marker = pump_until(
        &mut engine,
        |_, screen| screen.contains("NOVIEWLOG_MARKER"),
        15,
    );
    assert!(got_marker, "echo output must reach the live screen");

    // A column-count change reflows the live grid; text must survive it.
    engine
        .send_command_json(r#"{"cmd":"resize","width":1400,"height":300}"#)
        .expect("resize");
    let survived = pump_until(
        &mut engine,
        |_, screen| screen.contains("NOVIEWLOG_MARKER"),
        8,
    );
    assert!(survived, "resize lost the live-screen marker");

    // `exit` ends the session: no auto-respawn, running cleared.
    engine.handle_key(b"exit\r");
    let exited = pump_until(
        &mut engine,
        |engine, _| !engine.active_terminal_running_for_test(),
        15,
    );
    assert!(exited, "typed exit must end the session");
    assert!(
        engine.exit_code_for_test().is_some(),
        "natural exit must record an exit code"
    );
}

#[cfg(windows)]
#[test]
#[ignore = "real ConPTY; run with --ignored on a Windows host"]
fn conpty_kill_during_output_stops_cleanly() {
    let mut engine = Engine::new();
    engine.skip_projects_persist = true;
    engine.set_launch(crate::tests::long_running_launch_config());
    assert!(engine.active_terminal_running_for_test());

    // Let some output (ping progress) arrive, then Stop mid-stream.
    std::thread::sleep(Duration::from_millis(400));
    engine.poll_pty_for_test();
    let id = engine.active_terminal_id_for_test();
    engine
        .send_command_json(&format!(r#"{{"cmd":"stop","terminal_id":"{id}"}}"#))
        .expect("stop");

    let stopped = pump_until(
        &mut engine,
        |engine, _| !engine.active_terminal_running_for_test(),
        6,
    );
    assert!(stopped, "kill during output did not stop the session");
    let status = engine.status_message_for_test();
    assert!(
        !status.starts_with("Running:"),
        "explicit stop must not respawn: {status}"
    );
}
