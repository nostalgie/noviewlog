//! Deferred spawn behavior (issue #59): Start/Restart must not probe the
//! filesystem / registry on the UI thread. Cold cache parks the spawn on the
//! terminal and applies it when the background resolution lands; warm cache
//! starts synchronously; Stop cancels a spawn that is still resolving.

use crate::engine::Engine;
use crate::spawn_resolve::PreparedSpawn;
use crate::spawn_resolver::SpawnResolver;
use crate::tests::USER_CONFIG_LOCK;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

type BoxedResolve =
    Arc<dyn Fn(&str, Vec<String>, &str) -> Result<PreparedSpawn, String> + Send + Sync>;

fn engine_isolated() -> Engine {
    let mut engine = Engine::new();
    engine.skip_projects_persist = true;
    engine.projects = crate::core::types::ProjectsStore::default();
    engine.active_project = None;
    engine
}

/// Resolver whose resolution blocks until the gate opens — simulates a slow
/// PATH scan (dead network share, aggressive AV) deterministically.
fn gated_resolver() -> (SpawnResolver, Arc<(Mutex<bool>, Condvar)>) {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let gate2 = gate.clone();
    let resolve: BoxedResolve = Arc::new(move |command, args, cwd| {
        let (lock, cv) = &*gate2;
        let mut open = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !*open {
            open = cv.wait(open).unwrap_or_else(|e| e.into_inner());
        }
        Ok(PreparedSpawn {
            command: command.to_string(),
            args,
            cwd: cwd.to_string(),
        })
    });
    (SpawnResolver::with_resolve_fn(resolve), gate)
}

fn open_gate(gate: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, cv) = &**gate;
    let mut open = lock.lock().unwrap_or_else(|e| e.into_inner());
    *open = true;
    cv.notify_all();
}

/// Poll until the PTY for `terminal_id` exists (spawn applied).
fn wait_for_pty(engine: &mut Engine, terminal_id: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        engine.poll_pty_for_test();
        if engine.has_pty_for_test(terminal_id) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "slow tier: real shell spawn + wall-clock poll; run with -- --ignored"]
fn cold_start_parks_spawn_then_applies_and_buffers_stdin() {
    let _guard = USER_CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut engine = engine_isolated();
    let (resolver, gate) = gated_resolver();
    engine.set_spawn_resolver_for_test(resolver);

    engine.start_interactive_shell();
    let id = engine.active_terminal_id_for_test();
    assert!(
        engine.active_terminal_running_for_test(),
        "terminal is marked running while the spawn is parked"
    );
    assert!(
        !engine.has_pty_for_test(&id),
        "cold cache must NOT create the PTY synchronously (issue #59)"
    );
    assert!(
        engine.spawn_pending_for_test(&id),
        "spawn must be parked on the terminal"
    );

    // Keystrokes during the resolving window are buffered, not errored.
    engine.handle_key(b"echo hi\r\n");
    assert!(
        engine.pending_stdin_len_for_test(&id) > 0,
        "stdin typed during resolution must be buffered"
    );

    open_gate(&gate);
    assert!(
        wait_for_pty(&mut engine, &id),
        "parked spawn must apply once resolution lands"
    );
    assert!(engine.active_terminal_running_for_test());
    assert_eq!(
        engine.pending_stdin_len_for_test(&id),
        0,
        "buffered stdin must be flushed into the started PTY"
    );
    assert!(
        !engine.spawn_pending_for_test(&id),
        "pending spawn must be cleared after apply"
    );
}

#[test]
#[ignore = "slow tier: real shell spawn + wall-clock poll; run with -- --ignored"]
fn warm_cache_starts_synchronously_without_new_resolution() {
    let _guard = USER_CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut engine = engine_isolated();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = calls.clone();
    let resolve: BoxedResolve = Arc::new(move |command, args, cwd| {
        calls2.fetch_add(1, Ordering::SeqCst);
        Ok(PreparedSpawn {
            command: command.to_string(),
            args,
            cwd: cwd.to_string(),
        })
    });
    let resolver = SpawnResolver::with_resolve_fn(resolve);
    engine.set_spawn_resolver_for_test(resolver.clone());

    // Warm the cache for the exact shell argv the spawn will use.
    let (command, args, workdir) = engine
        .active_shell_argv_for_test()
        .expect("shell argv resolvable");
    assert!(resolver.request(&command, args.clone(), &workdir).is_none());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if resolver.request(&command, args.clone(), &workdir).is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "warmup never completed");
        std::thread::sleep(Duration::from_millis(5));
    }

    engine.start_interactive_shell();
    let id = engine.active_terminal_id_for_test();
    assert!(
        engine.has_pty_for_test(&id),
        "warm cache must start the PTY synchronously (zero probing)"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "warm start must not resolve again"
    );
}

#[test]
#[ignore = "slow tier: real shell spawn + wall-clock poll; run with -- --ignored"]
fn warm_saved_launch_starts_synchronously() {
    let _guard = USER_CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut engine = engine_isolated();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = calls.clone();
    let resolve: BoxedResolve = Arc::new(move |command, args, cwd| {
        calls2.fetch_add(1, Ordering::SeqCst);
        Ok(PreparedSpawn {
            command: command.to_string(),
            args,
            cwd: cwd.to_string(),
        })
    });
    let resolver = SpawnResolver::with_resolve_fn(resolve);
    engine.set_spawn_resolver_for_test(resolver.clone());

    // The saved launch must name a REAL executable: on the warm path the
    // resolved plan goes straight into start_prepared, which spawns it.
    #[cfg(windows)]
    let launch_json =
        r#"{"cmd":"program_set_launch","command":"cmd","args":["/c","echo","warm"],"cwd":"."}"#;
    #[cfg(not(windows))]
    let launch_json =
        r#"{"cmd":"program_set_launch","command":"sh","args":["-c","echo warm"],"cwd":"."}"#;
    engine.send_command_json(launch_json).expect("set launch");

    // Warm the cache for the exact argv the launch spawn will use.
    let (command, args, workdir) = engine
        .active_launch_argv_for_test()
        .expect("launch argv resolvable");
    assert!(resolver.request(&command, args.clone(), &workdir).is_none());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if resolver.request(&command, args.clone(), &workdir).is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "warmup never completed");
        std::thread::sleep(Duration::from_millis(5));
    }

    engine
        .send_command_json(r#"{"cmd":"terminal_start"}"#)
        .expect("terminal_start");
    let id = engine.active_terminal_id_for_test();
    assert!(
        engine.has_pty_for_test(&id),
        "warm cache must start a saved launch synchronously"
    );
    assert!(
        engine.active_terminal_running_for_test(),
        "warm launch must mark the terminal running"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "warm launch start must not resolve again"
    );
}

#[test]
#[ignore = "slow tier: real shell spawn + wall-clock poll; run with -- --ignored"]
fn stop_cancels_spawn_still_resolving() {
    let _guard = USER_CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut engine = engine_isolated();
    let (resolver, gate) = gated_resolver();
    engine.set_spawn_resolver_for_test(resolver);

    engine.start_interactive_shell();
    let id = engine.active_terminal_id_for_test();
    assert!(!engine.has_pty_for_test(&id));

    engine.stop(Some(&id));
    assert!(
        !engine.spawn_pending_for_test(&id),
        "stop must cancel the parked spawn"
    );
    assert!(!engine.active_terminal_running_for_test());

    // Even when the stale resolution lands afterwards, no PTY may appear.
    open_gate(&gate);
    let deadline = Instant::now() + Duration::from_millis(400);
    while Instant::now() < deadline {
        engine.poll_pty_for_test();
        assert!(
            !engine.has_pty_for_test(&id),
            "cancelled spawn must never start a PTY"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!engine.active_terminal_running_for_test());
}

#[test]
#[ignore = "slow tier: real shell spawn + wall-clock poll; run with -- --ignored"]
fn launch_spawn_failure_surfaces_status_and_unsticks() {
    let _guard = USER_CONFIG_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut engine = engine_isolated();
    let resolve: BoxedResolve =
        Arc::new(|command, _args, _cwd| Err(format!("executable '{command}' not found")));
    engine.set_spawn_resolver_for_test(SpawnResolver::with_resolve_fn(resolve));

    engine
        .send_command_json(r#"{"cmd":"program_set_launch","command":"definitely-missing-tool","args":["run"],"cwd":"."}"#)
        .expect("set launch");
    engine
        .send_command_json(r#"{"cmd":"terminal_start"}"#)
        .expect("terminal_start");
    let id = engine.active_terminal_id_for_test();
    assert!(
        !engine.has_pty_for_test(&id),
        "failed resolution must not create a PTY"
    );

    // The failure lands asynchronously and must unstick the terminal.
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        engine.poll_pty_for_test();
        if !engine.active_terminal_running_for_test() && !engine.spawn_pending_for_test(&id) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "spawn failure was never surfaced"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        engine.status_message_for_test().contains("Failed to start"),
        "status must surface the spawn failure, got: {}",
        engine.status_message_for_test()
    );
    assert!(!engine.has_pty_for_test(&id));
}
