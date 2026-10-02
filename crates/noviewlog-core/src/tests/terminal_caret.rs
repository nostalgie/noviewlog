use crate::engine::{Command, Engine};

#[test]
fn terminal_caret_rect_when_focused_running() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetViewportFocus { focused: true })
        .expect("focus");
    engine.mark_running_for_test();
    engine.ensure_live_screen_for_test();
    engine.rebuild_if_needed_for_test();
    let rect = engine.terminal_caret_rect(800, 600);
    assert!(
        rect.is_some(),
        "focused running Terminal tab should expose a caret rect"
    );
    let (x, y, w, h) = rect.unwrap();
    assert!(w > 0.0 && h > 0.0, "cell size positive");
    assert!(x.is_finite() && y.is_finite());
}

#[test]
fn terminal_caret_rect_none_when_unfocused() {
    let mut engine = Engine::new();
    engine.mark_running_for_test();
    engine.ensure_live_screen_for_test();
    assert!(engine.terminal_caret_rect(800, 600).is_none());
}

#[test]
#[ignore = "slow tier: fixed 600 ms sleep; run with -- --ignored"]
fn caret_blink_tick_does_not_dirty_viewport() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetViewportFocus { focused: true })
        .expect("focus");
    engine.mark_running_for_test();
    engine.ensure_live_screen_for_test();
    engine.rebuild_if_needed_for_test();
    let mut rgba = vec![0u8; 800 * 600 * 4];
    engine.render(800, 600, &mut rgba).expect("paint");
    assert!(!engine.needs_render());
    std::thread::sleep(std::time::Duration::from_millis(600));
    engine.tick();
    assert!(
        !engine.needs_render(),
        "engine caret blink must not dirty the bitmap viewport"
    );
}

#[test]
fn follow_wrap_live_caret_stays_near_viewport_bottom() {
    let mut engine = Engine::new();
    engine
        .send_command(Command::SetFollow { follow: true })
        .expect("follow");
    engine
        .send_command(Command::SetWrapLines { wrap: true })
        .expect("wrap");
    engine
        .send_command(Command::SetViewportFocus { focused: true })
        .expect("focus");
    engine.mark_running_for_test();
    {
        let term = engine.active_terminal_mut();
        term.ingest.ensure_live_screen(&mut term.buffer);
        // Long URLs force WRAP visual height past the viewport → paint scrolls down.
        let long = format!("{}\r\n", "https://example.com/very/long/path/segment/").repeat(40);
        term.ingest
            .feed(long.as_bytes(), &mut term.buffer, &mut term.parser);
        term.ingest
            .feed(b"prompt$ ", &mut term.buffer, &mut term.parser);
    }
    let width = 400u32;
    let height = 300u32;
    let (row, _) = engine.active_terminal().ingest.grid_caret().expect("caret");
    let stride = engine.viewport_row_stride_for_test();
    let naive_y = row as f32 * stride;
    let (x, y, w, h) = engine
        .terminal_caret_rect(width, height)
        .expect("Follow+WRAP caret");
    assert!(w > 0.0 && h > 0.0 && x.is_finite());
    // Must sit in the bottom band of the viewport, not at unscrolled pty_row*stride.
    assert!(
        y + h > height as f32 * 0.55,
        "caret Y should be near viewport bottom after WRAP scroll (y={y} h={h} height={height})"
    );
    assert!(
        (y - naive_y).abs() > h * 0.5 || naive_y > height as f32,
        "caret must account for scroll_y (y={y} naive_pty_row_y={naive_y})"
    );
}
