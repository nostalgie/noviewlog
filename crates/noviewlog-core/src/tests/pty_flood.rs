use crate::engine::{Command, Engine, PTY_INGEST_BYTES_PER_TICK};
use crate::pty::PtyEvent;
use std::time::{Duration, Instant};

#[test]
fn poll_pty_budgets_bytes_and_holds_remainder() {
    let mut engine = Engine::new();
    let id = engine.active_terminal().id.clone();
    // Ensure live VT screen exists so feed updates buffer.
    engine.ensure_live_screen_for_test();

    let chunk = vec![b'a'; 4096];
    let over = (PTY_INGEST_BYTES_PER_TICK / chunk.len()) + 8;
    for _ in 0..over {
        engine
            .pty_tx
            .try_send(PtyEvent::Bytes {
                id: id.clone(),
                data: chunk.clone(),
                generation: 0,
            })
            .expect("queue has room for budget+ test");
    }

    engine.poll_pty();
    assert!(
        engine.pty_hold.is_some() || engine.pty_rx.try_recv().is_ok(),
        "budgeted poll must leave PTY work for a later tick"
    );
}

#[test]
fn poll_pty_sets_drain_pending_without_requiring_mid_tick_wake() {
    let mut engine = Engine::new();
    let id = engine.active_terminal().id.clone();
    engine.ensure_live_screen_for_test();
    let chunk = vec![b'a'; 4096];
    let over = (PTY_INGEST_BYTES_PER_TICK / chunk.len()) + 8;
    for _ in 0..over {
        engine
            .pty_tx
            .try_send(PtyEvent::Bytes {
                id: id.clone(),
                data: chunk.clone(),
                generation: 0,
            })
            .expect("queue has room");
    }
    engine.poll_pty();
    assert!(
        engine.pty_work_pending(),
        "budget overrun must leave pty_work_pending for the host timer/retick"
    );
    assert!(
        engine.defer_pty_reader_wake(),
        "reader wake must be deferred until the paint interval so the host does not busy-drain"
    );
    assert!(engine.take_pty_drain_pending());
}

#[test]
fn poll_pty_widens_budget_in_drain_mode() {
    // Issue #126: a tick that starts with held-back work (reader queue backing
    // up) must ingest more than the base budget, else sustained throughput is
    // pinned at base_budget x tick rate (~8 MB/s).
    use crate::engine::PTY_INGEST_BYTES_PER_TICK;
    let mut engine = Engine::new();
    let id = engine.active_terminal().id.clone();
    engine.ensure_live_screen_for_test();
    // 64 KB chunks, each ending in a newline so every chunk commits one record.
    let chunk: Vec<u8> = {
        let mut c = vec![b'x'; 64 * 1024 - 1];
        c.push(b'\n');
        c
    };
    // 380 x 64 KB = 24 MB queued, far beyond the 2 MB drain cap.
    for _ in 0..380 {
        engine
            .pty_tx
            .try_send(PtyEvent::Bytes {
                id: id.clone(),
                data: chunk.clone(),
                generation: 0,
            })
            .expect("queue has room");
    }
    // Poll 1: base budget (256 KB) consumes 4 chunks and holds the 5th.
    engine.poll_pty();
    assert!(engine.pty_hold.is_some(), "base budget must hold remainder");
    // Poll 2 starts in drain mode: held chunk + ~30 more, i.e. ~2 MB.
    let before = engine.terminals[0].buffer.records_len();
    engine.poll_pty();
    let after = engine.terminals[0].buffer.records_len();
    let ingested = after - before;
    assert!(
        ingested >= PTY_INGEST_BYTES_PER_TICK / (64 * 1024) * 3,
        "drain-mode poll must exceed the base budget ({ingested} records)"
    );
}

#[test]
fn ring_trim_anchors_scroll_when_follow_off() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetSettings {
            max_scrollback_lines: 1_000,
        })
        .expect("settings");
    engine.ensure_live_screen_for_test();
    let id = engine.active_terminal().id.clone();

    let mut blob = Vec::new();
    for i in 0..1_200 {
        blob.extend_from_slice(format!("LINE-{i:04} {}\r\n", "x".repeat(40)).as_bytes());
    }
    for chunk in blob.chunks(4096) {
        engine
            .pty_tx
            .try_send(PtyEvent::Bytes {
                id: id.clone(),
                data: chunk.to_vec(),
                generation: 0,
            })
            .ok();
        engine.poll_pty();
        engine.tick();
    }
    for _ in 0..80 {
        engine.poll_pty();
        engine.tick();
        if engine.pty_hold.is_none() {
            break;
        }
    }

    engine
        .send_command(Command::SetFollow { follow: false })
        .expect("follow off");
    let _ = engine.send_command(Command::SetWrapLines { wrap: false });
    let max = engine.max_scroll_offset_for_test();
    // Lower half so ~120 trimmed head lines cannot evict the marker.
    let mid = (max * 0.55).max(engine.viewport_row_stride_for_test() * 10.0);
    engine
        .send_command(Command::Scroll { offset: mid })
        .expect("scroll");
    engine.tick();
    let scroll_before = engine.scroll_offset_y_for_test();
    assert!(
        scroll_before > 1.0,
        "precondition: scrolled up ({scroll_before})"
    );

    let stride = engine.viewport_row_stride_for_test();
    let first_row = (scroll_before / stride).floor() as usize;
    let marker = {
        let view = engine.active_terminal().active_view();
        view.flat_lines
            .get(first_row)
            .map(|l| l.raw.clone())
            .expect("flat line at viewport top")
    };

    let more = format!("NEWER {}\r\n", "y".repeat(40))
        .into_bytes()
        .repeat(120);
    for chunk in more.chunks(4096) {
        engine
            .pty_tx
            .try_send(PtyEvent::Bytes {
                id: id.clone(),
                data: chunk.to_vec(),
                generation: 0,
            })
            .ok();
        engine.poll_pty();
        engine.tick();
    }

    let scroll_after = engine.scroll_offset_y_for_test();
    assert!(
        scroll_after < scroll_before - 1.0,
        "scroll must shrink on trim when Follow off (before={scroll_before} after={scroll_after})"
    );

    let marker_idx = {
        let view = engine.active_terminal().active_view();
        view.flat_lines
            .iter()
            .position(|l| l.raw == marker)
            .expect("anchored marker line must still exist in scrollback")
    };
    let expected_top = marker_idx as f32 * stride;
    assert!(
        (scroll_after - expected_top).abs() < stride * 1.5,
        "scroll should keep marker near viewport top (scroll={scroll_after} expected≈{expected_top} idx={marker_idx})"
    );
}

#[test]
fn follow_snap_does_not_pin_filter_tab_scroll() {
    // Issue #316: the Terminal tab (view 0) keeps Follow while the user reads
    // a filter tab whose Follow is cleared — ingest must not move the shared
    // scroll_offset_y out from under the active filter tab.
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetSettings {
            max_scrollback_lines: 5_000,
        })
        .expect("settings");
    engine.ensure_live_screen_for_test();
    let id = engine.active_terminal().id.clone();

    let mut blob = Vec::new();
    for i in 0..600 {
        blob.extend_from_slice(format!("LINE-{i:04} {}\r\n", "x".repeat(40)).as_bytes());
    }
    for chunk in blob.chunks(4096) {
        engine
            .pty_tx
            .try_send(PtyEvent::Bytes {
                id: id.clone(),
                data: chunk.to_vec(),
                generation: 0,
            })
            .ok();
        engine.poll_pty();
        engine.tick();
    }

    engine
        .send_command(Command::TabAdd)
        .expect("add filter tab");
    assert_eq!(engine.active_terminal().active_view, 1);
    // Clear Follow on the filter tab only; view 0 keeps Follow — that is the bug trigger.
    engine
        .send_command(Command::SetFollow { follow: false })
        .expect("follow off on filter tab");
    assert!(!engine.active_terminal().active_view().auto_follow);
    assert!(engine.active_terminal().views[0].auto_follow);

    let max = engine.max_scroll_offset_for_test();
    engine
        .send_command(Command::Scroll { offset: max * 0.5 })
        .expect("scroll filter tab");
    engine.tick();
    let scroll_before = engine.scroll_offset_y_for_test();
    assert!(
        scroll_before > 1.0,
        "precondition: scrolled up ({scroll_before})"
    );

    let more = format!("NEWER {}\r\n", "y".repeat(40))
        .into_bytes()
        .repeat(120);
    for chunk in more.chunks(4096) {
        engine
            .pty_tx
            .try_send(PtyEvent::Bytes {
                id: id.clone(),
                data: chunk.to_vec(),
                generation: 0,
            })
            .ok();
        engine.poll_pty();
        engine.tick();
    }

    let scroll_after = engine.scroll_offset_y_for_test();
    assert!(
        (scroll_after - scroll_before).abs() < 1.0,
        "ingest must not pin the shared scroll while a filter tab is active \
         (before={scroll_before} after={scroll_after})"
    );
}

#[test]
#[ignore = "slow tier: real shell + 12 MiB fixture, latency-sensitive; run with -- --ignored"]
fn cat_big_log_poll_ticks_stay_bounded() {
    // Issue #72: generated fixture instead of a hardcoded dev-machine path.
    let path = crate::tests::big_log_fixture();
    let mut engine = Engine::new();
    engine.start_interactive_shell();
    // Let the shell start.
    for _ in 0..50 {
        engine.tick();
        std::thread::sleep(Duration::from_millis(20));
        if engine.active_terminal().running {
            break;
        }
    }
    assert!(
        engine.active_terminal().running,
        "interactive shell should be running"
    );
    // Extra settle so the prompt is ready.
    for _ in 0..10 {
        engine.tick();
        std::thread::sleep(Duration::from_millis(30));
    }

    let before = engine.active_terminal().buffer.records_len();
    // `\r` is Enter on both Linux shells and ConPTY (bare `\n` can hang
    // CR-expecting Windows readers); `cat` also works in PowerShell (alias).
    let cmd = format!("cat {}\r", path.display());
    engine.handle_key(cmd.as_bytes());

    let deadline = Instant::now() + Duration::from_secs(90);
    let mut max_tick = Duration::ZERO;
    let mut ticks = 0u32;
    let mut saw_growth = false;
    let mut quiet_streak = 0u32;
    while Instant::now() < deadline {
        let t0 = Instant::now();
        engine.tick();
        let dt = t0.elapsed();
        max_tick = max_tick.max(dt);
        ticks += 1;
        assert!(
            dt < Duration::from_millis(750),
            "single tick took {dt:?} (budgeted poll must keep UI responsive)"
        );
        let now_len = engine.active_terminal().buffer.records_len();
        if now_len > before + 100 {
            saw_growth = true;
        }
        let pending = engine.pty_hold.is_some();
        if saw_growth && !pending {
            quiet_streak += 1;
            if quiet_streak >= 20 {
                break;
            }
        } else {
            quiet_streak = 0;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        saw_growth,
        "expected scrollback growth from cat; ticks={ticks} max_tick={max_tick:?}"
    );
    assert!(
        ticks > 20,
        "expected many budgeted ticks draining cat; ticks={ticks} max_tick={max_tick:?}"
    );
    eprintln!("cat_big_log: ticks={ticks} max_tick={max_tick:?}");
}

#[test]
fn flood_paint_dirty_is_cadenced_and_follow_stays_snapped() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetFollow { follow: true })
        .expect("follow");
    {
        let term = engine.active_terminal_mut();
        term.running = true;
    }
    let id = engine.active_terminal().id.clone();
    let chunk = vec![b'x'; 4096];

    let over = (PTY_INGEST_BYTES_PER_TICK / chunk.len()) + 2;
    for _ in 0..over {
        engine
            .pty_tx
            .try_send(PtyEvent::Bytes {
                id: id.clone(),
                data: chunk.clone(),
                generation: 0,
            })
            .expect("queue");
    }
    engine.poll_pty();
    assert!(engine.pty_work_pending());
    assert!(engine.needs_render(), "first flood ingest must dirty");
    let max = engine.max_scroll_offset_for_test();
    let scroll = engine.scroll_offset_y_for_test();
    assert!(
        (scroll - max).abs() < 1.5 || max < 1.0,
        "Follow snap on ingest (scroll={scroll} max={max})"
    );
    let mut buf = vec![0u8; 800 * 400 * 4];
    engine.render(800, 400, &mut buf).expect("render");

    // Cadence helper (independent of slow VTE): just-painted + more_pending must not dirty.
    engine.note_viewport_painted();
    engine.viewport_dirty = false;
    engine.mark_viewport_dirty_after_pty_ingest(true);
    assert!(
        !engine.needs_render(),
        "within paint interval, flood must not dirty"
    );

    // Follow still snaps on a real poll even when paint is deferred.
    engine.note_viewport_painted();
    engine.viewport_dirty = false;
    for _ in 0..over {
        let _ = engine.pty_tx.try_send(PtyEvent::Bytes {
            id: id.clone(),
            data: chunk.clone(),
            generation: 0,
        });
    }
    // Force "paint was just now" so even a slow poll stays within the cadence window
    // for the dirty decision at the end of poll_pty.
    engine.note_viewport_painted();
    engine.poll_pty();
    // Re-stamp after poll's VTE cost so the assertion targets deferred dirty, not wall clock.
    // Snap must already have happened during poll.
    let max = engine.max_scroll_offset_for_test();
    let scroll = engine.scroll_offset_y_for_test();
    assert!(
        (scroll - max).abs() < 1.5 || max < 1.0,
        "Follow snap when paint may be deferred (scroll={scroll} max={max})"
    );

    std::thread::sleep(Duration::from_millis(40));
    engine.viewport_dirty = false;
    engine.mark_viewport_dirty_after_pty_ingest(true);
    assert!(
        engine.needs_render(),
        "after paint interval, flood ingest must dirty again"
    );

    // Catch-up / echo: more_pending=false always dirties.
    engine.note_viewport_painted();
    engine.viewport_dirty = false;
    engine.mark_viewport_dirty_after_pty_ingest(false);
    assert!(
        engine.needs_render(),
        "idle echo / catch-up must dirty immediately"
    );
}

#[test]
fn follow_scroll_does_not_jump_across_tick_rebuild() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetFollow { follow: true })
        .expect("follow");
    engine.ensure_live_screen_for_test();
    engine.tick();
    let id = engine.active_terminal().id.clone();
    engine.active_terminal_mut().running = true;
    let chunk = vec![b'x'; 4096];
    let over = (PTY_INGEST_BYTES_PER_TICK / chunk.len()) + 2;
    for _ in 0..over {
        let _ = engine.pty_tx.try_send(PtyEvent::Bytes {
            id: id.clone(),
            data: chunk.clone(),
            generation: 0,
        });
    }
    engine.poll_pty();
    let max_after_poll = engine.max_scroll_offset_for_test();
    let scroll_after_poll = engine.scroll_offset_y_for_test();
    engine.tick();
    let max_after_tick = engine.max_scroll_offset_for_test();
    let scroll_after_tick = engine.scroll_offset_y_for_test();
    assert!(
        (scroll_after_poll - max_after_poll).abs() < 2.0 || max_after_poll < 1.0,
        "Follow snap after poll (scroll={scroll_after_poll} max={max_after_poll})"
    );
    assert!(
        (max_after_tick - max_after_poll).abs() < engine.viewport_row_stride_for_test() * 2.0
            || max_after_poll < 1.0,
        "tick rebuild must not drop overlay height (max poll={max_after_poll} tick={max_after_tick})"
    );
    assert!(
        (scroll_after_tick - max_after_tick).abs() < 2.0 || max_after_tick < 1.0,
        "Follow must stay snapped after tick (scroll={scroll_after_tick} max={max_after_tick})"
    );
}

#[test]
fn follow_flood_does_not_patch_overlay_into_logview() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetFollow { follow: true })
        .expect("follow");
    {
        let term = engine.active_terminal_mut();
        term.running = true;
    }
    let before_lines = engine.active_view().flat_lines.len();
    let before_overlay = engine.active_view().overlay_len();
    let id = engine.active_terminal().id.clone();
    let line = format!("{}\r\n", "x".repeat(60));
    let blob = line.repeat(400);
    for chunk in blob.as_bytes().chunks(4096) {
        let _ = engine.pty_tx.try_send(PtyEvent::Bytes {
            id: id.clone(),
            data: chunk.to_vec(),
            generation: 0,
        });
    }
    engine.poll_pty();
    assert_eq!(
        engine.active_view().overlay_len(),
        before_overlay,
        "Follow must not replace overlay on the Terminal tab LogView"
    );
    assert_eq!(
        engine.active_view().flat_lines.len(),
        before_lines,
        "Follow must not grow Terminal tab flat_lines under flood"
    );
    assert!(
        engine.active_terminal().buffer.records_len() > 10,
        "scrolled-off rows must still become Records"
    );
    let mut buf = vec![0u8; 800 * 400 * 4];
    engine.render(800, 400, &mut buf).expect("live grid render");
}

#[test]
fn echo_does_not_defer_pty_reader_wake() {
    let mut engine = Engine::new();
    let id = engine.active_terminal().id.clone();
    engine
        .pty_tx
        .try_send(PtyEvent::Bytes {
            id,
            data: b"x".to_vec(),
            generation: 0,
        })
        .expect("echo");
    engine.poll_pty();
    assert!(
        !engine.pty_work_pending(),
        "single echo must drain in one poll"
    );
    assert!(
        !engine.defer_pty_reader_wake(),
        "echo must still allow an immediate host wake"
    );
}

#[test]
fn flood_wake_defer_clears_after_paint_interval() {
    let mut engine = Engine::new();
    let id = engine.active_terminal().id.clone();
    let chunk = vec![b'a'; 4096];
    let over = (PTY_INGEST_BYTES_PER_TICK / chunk.len()) + 8;
    for _ in 0..over {
        let _ = engine.pty_tx.try_send(PtyEvent::Bytes {
            id: id.clone(),
            data: chunk.clone(),
            generation: 0,
        });
    }
    engine.poll_pty();
    assert!(engine.defer_pty_reader_wake());
    std::thread::sleep(Duration::from_millis(40));
    assert!(
        !engine.defer_pty_reader_wake(),
        "after the paint interval the host timer may poll again"
    );
}

#[test]
fn follow_wrap_live_grid_render_does_not_panic() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetFollow { follow: true })
        .expect("follow");
    engine
        .send_command(Command::SetWrapLines { wrap: true })
        .expect("wrap");
    {
        let term = engine.active_terminal_mut();
        term.running = true;
        let long = format!("{}\r\n", "https://example.com/path/").repeat(30);
        term.ingest
            .feed(long.as_bytes(), &mut term.buffer, &mut term.parser);
    }
    let mut buf = vec![0u8; 800 * 400 * 4];
    engine
        .render(800, 400, &mut buf)
        .expect("Follow+WRAP live grid");
    assert!(engine.wrap_lines_for_test());
}

#[test]
fn follow_live_line_counter_grows_past_scrollback_cap() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetSettings {
            max_scrollback_lines: 200,
        })
        .expect("settings");
    engine
        .send_command(Command::SetFollow { follow: true })
        .expect("follow");
    {
        let term = engine.active_terminal_mut();
        term.running = true;
        let mut blob = String::new();
        for i in 0..800 {
            blob.push_str(&format!("line-{i}\n"));
        }
        term.ingest
            .feed(blob.as_bytes(), &mut term.buffer, &mut term.parser);
    }
    assert!(
        engine.active_terminal().buffer.dropped_count() > 0,
        "ring must have trimmed past the 200-line cap"
    );
    let (cur, total) = engine.viewport_line_position_for_test();
    assert_eq!(cur, total);
    assert!(
        total > 200,
        "Follow status must grow past max_scrollback (got {total}), not stick at the ring size"
    );
    // Scrollbar range is ring + screen (not ever-seen), so the thumb stays small.
    let max = engine.max_scroll_offset_for_test();
    let stride = engine.viewport_row_stride_for_test();
    let ever = total as f32 * stride;
    assert!(
        max < ever * 0.5,
        "live Follow max_scroll must not use ever-seen height (max={max} ever≈{ever})"
    );
    assert!(
        max > 50.0 * stride,
        "live Follow max_scroll should include the retained ring, not screen-only (max={max})"
    );
}

#[test]
fn selection_materializes_live_grid_under_follow() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::Resize {
            width: 800,
            height: 400,
        })
        .expect("resize");
    engine.mark_running_for_test();
    engine.ensure_live_screen_for_test();
    {
        let term = engine.active_terminal_mut();
        term.ingest.feed(
            b"hello selection marker line here\r\n",
            &mut term.buffer,
            &mut term.parser,
        );
    }
    assert!(engine.auto_follow_for_test());
    assert_eq!(
        engine.view_flat_line_count_for_test(0),
        Some(0),
        "live grid must not patch flat_lines while Follow paints the VT screen"
    );

    engine
        .send_command(Command::SelectionAt {
            x: 80.0,
            y: 20.0,
            extend: false,
            click_count: 1,
        })
        .expect("selection down");
    assert!(!engine.auto_follow_for_test());
    assert!(
        engine.view_flat_line_count_for_test(0).unwrap_or(0) > 0,
        "click must materialize live screen into flat_lines"
    );

    let stride = engine.viewport_row_stride_for_test();
    let scroll = engine.scroll_offset_y_for_test();
    let idx = engine
        .flat_line_texts_for_test()
        .iter()
        .position(|l| l.contains("selection marker"))
        .expect("materialized line with marker");
    let y = idx as f32 * stride - scroll + stride * 0.5;

    engine
        .send_command(Command::SelectionAt {
            x: 120.0,
            y,
            extend: false,
            click_count: 2,
        })
        .expect("word select");
    let text = engine.selection_text_for_test().unwrap_or_default();
    assert!(
        text.contains("selection"),
        "expected word selection, got {text:?}"
    );
}

fn seed_follow_live_grid_with_scrollback(engine: &mut Engine) {
    engine
        .send_command(Command::Resize {
            width: 800,
            height: 400,
        })
        .expect("resize");
    engine
        .send_command(Command::SetWrapLines { wrap: true })
        .expect("wrap");
    engine
        .send_command(Command::SetFollow { follow: true })
        .expect("follow");
    engine.push_lines_for_test([
        "Windows PowerShell".into(),
        "Copyright (C) Microsoft Corporation. All rights reserved.".into(),
        "PS C:\\projects\\noviewlog>".into(),
    ]);
    engine.mark_running_for_test();
    engine.ensure_live_screen_for_test();
    {
        let term = engine.active_terminal_mut();
        term.ingest.feed(
            b"Linux test-host 6.18.33.2-microsoft-standard-WSL2 uname\r\n",
            &mut term.buffer,
            &mut term.parser,
        );
    }
    assert!(
        engine.paints_live_vt_grid_for_test(),
        "precondition: Follow live-grid paint"
    );
}

fn assert_overlay_has_banner_and_tail(engine: &Engine, via: &str) {
    let texts = engine.flat_line_texts_for_test();
    let joined = texts.join("\n");
    assert!(
        texts.iter().any(|l| l.contains("Windows PowerShell")),
        "{via}: overlay must include committed banner, got {joined:?}"
    );
    assert!(
        texts.iter().any(|l| l.contains("Linux test-host")),
        "{via}: overlay must include live tail, got {joined:?}"
    );
    let max = engine.max_scroll_offset_for_test();
    let y = engine.scroll_offset_y_for_test();
    assert!(
        y <= max + 0.5,
        "{via}: scroll_y must be in overlay range (y={y} max={max})"
    );
}

#[test]
fn scrollbar_and_wheel_materialize_the_same_overlay_after_follow() {
    let mut wheel = Engine::new();
    seed_follow_live_grid_with_scrollback(&mut wheel);
    wheel
        .send_command(Command::ScrollLines { delta: -3 })
        .expect("wheel up");
    assert!(
        !wheel.auto_follow_for_test(),
        "wheel away from Follow must leave live-grid paint"
    );
    assert_overlay_has_banner_and_tail(&wheel, "wheel");

    let mut bar = Engine::new();
    seed_follow_live_grid_with_scrollback(&mut bar);
    bar.send_command(Command::Scroll { offset: 0.0 })
        .expect("scrollbar to top");
    assert!(
        !bar.auto_follow_for_test(),
        "scrollbar away from Follow must leave live-grid paint"
    );
    assert_overlay_has_banner_and_tail(&bar, "scrollbar");

    let wheel_lines = wheel.flat_line_texts_for_test();
    let bar_lines = bar.flat_line_texts_for_test();
    assert_eq!(
        wheel_lines, bar_lines,
        "scrollbar and wheel must compose the same overlay"
    );
}

#[test]
fn unfiltered_filter_tab_shows_live_overlay_while_running() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::Resize {
            width: 800,
            height: 400,
        })
        .expect("resize");
    engine
        .send_command(Command::SetFollow { follow: true })
        .expect("follow");
    engine.mark_running_for_test();
    engine.ensure_live_screen_for_test();
    {
        let term = engine.active_terminal_mut();
        term.ingest.feed(
            b"uname-a-kernel-line\r\n",
            &mut term.buffer,
            &mut term.parser,
        );
    }
    assert_eq!(
        engine.buffer_record_count_for_test(),
        0,
        "short output must stay on the live screen"
    );

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    assert_eq!(engine.active_tab_index_for_test(), 1);
    let texts = engine.flat_line_texts_for_test();
    assert!(
        texts.iter().any(|t| t.contains("uname-a-kernel-line")),
        "empty filter tab must show live overlay, got {texts:?}"
    );
}

#[test]
fn include_filter_tab_keeps_matching_live_overlay_only() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::Resize {
            width: 800,
            height: 400,
        })
        .expect("resize");
    engine.mark_running_for_test();
    engine.ensure_live_screen_for_test();
    {
        let term = engine.active_terminal_mut();
        term.ingest.feed(
            b"keep-me visible\r\ndrop-this line\r\n",
            &mut term.buffer,
            &mut term.parser,
        );
    }

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(
            r#"{"cmd":"filter_add","type":"include","pattern":"keep-me","regex":false}"#,
        )
        .expect("filter_add");
    engine.rebuild_if_needed_for_test();

    let texts = engine.flat_line_texts_for_test();
    assert!(
        texts.iter().any(|t| t.contains("keep-me visible")),
        "include filter must keep matching overlay lines, got {texts:?}"
    );
    assert!(
        texts.iter().all(|t| !t.contains("drop-this")),
        "include filter must drop non-matching overlay lines, got {texts:?}"
    );
    assert_eq!(
        engine.buffer_record_count_for_test(),
        0,
        "overlay filter must not commit live-screen frames as Records"
    );
}

#[test]
fn include_filter_tab_keeps_committed_matches_across_overlay_only() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::Resize {
            width: 800,
            height: 400,
        })
        .expect("resize");
    engine.mark_running_for_test();
    engine.ensure_live_screen_for_test();

    engine
        .send_command_json(r#"{"cmd":"tab_add"}"#)
        .expect("tab_add");
    engine
        .send_command_json(
            r#"{"cmd":"filter_add","type":"include","pattern":"KEEP-","regex":false}"#,
        )
        .expect("filter_add");
    engine.rebuild_if_needed_for_test();

    engine
        .send_command_json(r#"{"cmd":"tab_switch","index":0}"#)
        .expect("tab_switch terminal");

    let mut blob = Vec::new();
    for i in 0..80 {
        blob.extend(format!("KEEP-{i:03} matching line\r\n").into_bytes());
        blob.extend(format!("drop-{i:03} noise\r\n").into_bytes());
    }
    {
        let term = engine.active_terminal_mut();
        term.ingest.feed(&blob, &mut term.buffer, &mut term.parser);
    }
    let records = engine.buffer_record_count_for_test();
    assert!(
        records > 40,
        "enough lines must scroll off the VT screen, got {records} Records"
    );

    let stale_lines = engine.view_flat_line_count_for_test(1).unwrap();
    engine.rebuild_if_needed_for_test();
    assert_eq!(
        engine.view_flat_line_count_for_test(1),
        Some(stale_lines),
        "inactive filter tab must stay stale until selected"
    );

    engine
        .send_command_json(r#"{"cmd":"tab_switch","index":1}"#)
        .expect("tab_switch filter");
    let texts = engine.flat_line_texts_for_test();
    assert!(
        texts.iter().any(|t| t.contains("KEEP-000")),
        "switch onto filter tab must show committed matches, got {texts:?}"
    );
    let overlay_n = engine.overlay_len_for_test();
    let committed_keep = texts.len().saturating_sub(overlay_n);
    assert!(
        committed_keep > 11,
        "committed matching prefix must exceed one live screen, got committed={committed_keep} overlay={overlay_n} texts={texts:?}"
    );

    let id = engine.active_terminal().id.clone();
    engine
        .pty_tx
        .try_send(PtyEvent::Bytes {
            id: id.clone(),
            data: b"\rKEEP-live spinner-aaaa".to_vec(),
            generation: 0,
        })
        .expect("overlay-only");
    engine.poll_pty();
    engine.rebuild_if_needed_for_test();
    let after_spinner = engine.flat_line_texts_for_test();
    assert!(
        after_spinner.iter().any(|t| t.contains("KEEP-000")),
        "overlay-only ingest must not drop committed matches, got {after_spinner:?}"
    );
    let after_overlay_n = engine.overlay_len_for_test();
    let after_committed = after_spinner.len().saturating_sub(after_overlay_n);
    assert_eq!(
        after_committed, committed_keep,
        "overlay-only must not change committed matching count ({committed_keep} -> {after_committed})"
    );

    engine
        .pty_tx
        .try_send(PtyEvent::Bytes {
            id,
            data: b"\rx".to_vec(),
            generation: 0,
        })
        .expect("shorter overlay");
    engine.poll_pty();
    engine.rebuild_if_needed_for_test();
    let after_short = engine.flat_line_texts_for_test();
    assert!(
        after_short.iter().any(|t| t.contains("KEEP-000")),
        "shorter overlay-only frame must not drop committed matches, got {after_short:?}"
    );
    let short_overlay_n = engine.overlay_len_for_test();
    let short_committed = after_short.len().saturating_sub(short_overlay_n);
    assert_eq!(
        short_committed, committed_keep,
        "committed matching count must stay stable while overlay tail may change"
    );
}

#[test]
fn idle_flush_commits_background_terminal_pending_tail() {
    // Issue #193: `flush_idle_pending` ran for the active terminal only, so a
    // background terminal's last pushed line stayed pending in the
    // RecordParser — its scrollback missed the tail until the user switched
    // back or the process exited.
    let mut engine = Engine::new();
    engine
        .send_command_json(r#"{"cmd":"set_format","format_id":"raw"}"#)
        .expect("set_format raw");
    // Raw format keeps the last pushed line pending until the next line or
    // the idle flush.
    engine.push_lines_for_test(["bg-1".into(), "bg-2".into(), "bg-tail".into()]);
    let background = 0usize;
    assert_eq!(
        engine.terminals[background].buffer.records_len(),
        2,
        "last line must sit pending before the idle flush"
    );
    engine.terminal_add_blank_for_test();
    assert_eq!(engine.active_terminal_index_for_test(), 1);

    // Age the background terminal past the idle window without sleeping.
    engine.terminals[background].last_line_at = Some(Instant::now() - Duration::from_secs(1));

    engine.poll_pty_for_test();

    assert_eq!(
        engine.terminals[background].buffer.records_len(),
        3,
        "background terminal must flush its pending tail on idle"
    );
    assert!(engine.terminals[background].last_line_at.is_none());
    assert!(
        engine.terminals[background]
            .views
            .iter()
            .all(|v| v.is_flat_lines_dirty()),
        "background terminal's views must rebuild on next selection"
    );
}

#[test]
#[ignore = "slow tier: real PTY child spawn + fixed settle sleeps; run with -- --ignored"]
fn stop_releases_reader_parked_on_full_queue() {
    // Issue #196: `stop()` (kill child, drop master) can unblock a reader
    // parked in `read`, but never one parked in a blocking queue `send`.
    // With the host not draining and the queue full, the reader and the
    // exit-waiter joined to it leaked until the engine dropped `pty_rx`.
    use crate::pty::PtyManager;
    use crate::spawn_resolve::PreparedSpawn;

    const GENERATION: u64 = 7;
    let filler = |data: Vec<u8>| PtyEvent::Bytes {
        id: "queue-filler".into(),
        data,
        generation: 0,
    };

    // Tiny bounded queue, filled before the session starts: the reader's
    // chunks must go through the full-queue send path, not the blocking
    // one that `stop()` cannot release.
    let (tx, rx) = std::sync::mpsc::sync_channel::<PtyEvent>(2);
    let tx_test = tx.clone();
    for _ in 0..2 {
        tx.send(filler(vec![0u8; 16])).expect("fill bounded queue");
    }

    // A child that emits one chunk right away and then ticks ~once a
    // second: the first chunk proves output flows, the next one parks in
    // the full-queue send while the queue stays full.
    let (command, args): (String, Vec<String>) = if cfg!(windows) {
        (
            "ping".into(),
            vec!["-n".into(), "60".into(), "127.0.0.1".into()],
        )
    } else {
        (
            "sh".into(),
            vec![
                "-c".into(),
                "while true; do echo tick; sleep 1; done".into(),
            ],
        )
    };
    let mut pty = PtyManager::new();
    pty.start_prepared(
        tx,
        "t-196".into(),
        PreparedSpawn {
            command,
            args,
            cwd: std::env::temp_dir().to_string_lossy().into_owned(),
        },
        GENERATION,
        None,
    )
    .expect("PTY child spawn");

    // Prove the child's first chunk passed through the send path: drain
    // (freeing slots) until a session event pops out ahead of the parked
    // send completing.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut first_chunk_flowed = false;
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(PtyEvent::Bytes { generation, .. }) if generation == GENERATION => {
                first_chunk_flowed = true;
                break;
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
    assert!(
        first_chunk_flowed,
        "child output must reach the queue (send path works)"
    );

    // Refill to full and give the child its ~1 s cadence: the next chunk
    // parks the reader on the full queue and stays parked while rx lives.
    while tx_test.try_send(filler(vec![0u8; 16])).is_ok() {}
    std::thread::sleep(Duration::from_millis(2500));
    assert!(
        !pty.test_session_threads_finished(),
        "precondition: session threads alive while the queue is full"
    );

    pty.stop();

    // rx stays alive and the queue stays full, so a parked blocking send
    // could never resolve: the reader must still exit promptly, and the
    // exit-waiter joined to it with it.
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline && !pty.test_session_threads_finished() {
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        pty.test_session_threads_finished(),
        "stop() must release the reader + exit-waiter even on a full queue"
    );
}
