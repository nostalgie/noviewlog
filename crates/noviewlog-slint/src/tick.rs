//! Host tick: the UI-thread body run by the repeated timer and by PTY wakes.
//! Owns the `HOST_TICK` TLS slot, flood pacing, and the PTY activity wake.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use noviewlog_core::{parse_engine_event, Engine, EngineEvent};
use slint::{
    ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer, TimerMode,
    VecModel, Weak,
};

use crate::caret::sync_terminal_caret;
use crate::engine_bridge::{
    bump_fast_timer, set_occluded_timer, window_should_pause_paint, TICK_FAST, TICK_IDLE,
};
use crate::find::FindPending;
use noviewlog_slint::stats_sync::apply_stats;
use noviewlog_slint::ui::{AppWindow, FilterInfo, ProjectInfo, TabInfo, TerminalInfo};

// UI-thread tick body; PTY wake uses `invoke_from_event_loop` → this TLS (Send-safe).
thread_local! {
    static HOST_TICK: RefCell<Option<Box<dyn FnMut()>>> = const { RefCell::new(None) };
}

/// Reused viewport RGBA buffer: (width, height, pixels), recreated on size change.
pub(crate) type ViewportPixels = Rc<RefCell<Option<(u32, u32, SharedPixelBuffer<Rgba8Pixel>)>>>;

/// State captured by the host tick closure.
pub(crate) struct TickDeps {
    pub(crate) ui: Weak<AppWindow>,
    pub(crate) engine: Rc<RefCell<Engine>>,
    pub(crate) force_render: Rc<Cell<bool>>,
    pub(crate) timer: Rc<Timer>,
    pub(crate) timer_fast: Rc<Cell<bool>>,
    pub(crate) logical_size: Rc<RefCell<(f32, f32)>>,
    pub(crate) tabs_model: Rc<VecModel<TabInfo>>,
    pub(crate) terminals_model: Rc<VecModel<TerminalInfo>>,
    pub(crate) files_model: Rc<VecModel<TerminalInfo>>,
    pub(crate) projects_model: Rc<VecModel<ProjectInfo>>,
    pub(crate) filters_model: Rc<VecModel<FilterInfo>>,
    pub(crate) terminal_tab_active: Rc<Cell<bool>>,
    pub(crate) window_occluded: Rc<Cell<bool>>,
    pub(crate) was_occluded: Rc<Cell<bool>>,
    pub(crate) viewport_presented: Rc<Cell<bool>>,
    pub(crate) syncing_scroll: Rc<Cell<bool>>,
    pub(crate) syncing_follow: Rc<Cell<bool>>,
    pub(crate) has_selection: Rc<Cell<bool>>,
    pub(crate) pty_running: Rc<Cell<bool>>,
    pub(crate) viewport_font_size: Rc<Cell<f32>>,
    pub(crate) find_resync: Rc<Cell<bool>>,
    pub(crate) find_stats_tab: Rc<Cell<i32>>,
    pub(crate) find_pending: FindPending,
    pub(crate) find_debounce: Rc<Timer>,
    /// Viewport focus deferred by `on_viewport_focused` when the engine was
    /// already borrowed (issue #252); applied and cleared here each tick.
    pub(crate) pending_viewport_focus: Rc<Cell<Option<bool>>>,
    /// Reused RGBA buffer across paints (recreate only on size change).
    pub(crate) viewport_pixels: ViewportPixels,
}

/// Atomics + wake scheduler shared between the tick body and the PTY wake.
pub(crate) struct TickControls {
    pub(crate) ticking: Arc<AtomicBool>,
    pub(crate) needs_retick: Arc<AtomicBool>,
    pub(crate) flood_pacing: Arc<AtomicBool>,
    pub(crate) schedule_host_tick: Arc<dyn Fn() + Send + Sync>,
}

pub(crate) fn install_host_tick(deps: TickDeps) -> TickControls {
    let TickDeps {
        ui: ui_weak,
        engine: engine_tick,
        force_render: force_tick,
        timer: timer_tick,
        timer_fast: timer_fast_tick,
        logical_size: logical_tick,
        tabs_model: tabs_tick,
        terminals_model: terminals_tick,
        files_model: files_tick,
        projects_model: projects_tick,
        filters_model: filters_tick,
        terminal_tab_active: terminal_tab_tick,
        window_occluded: window_occluded_tick,
        was_occluded: was_occluded_tick,
        viewport_presented: presented_tick,
        syncing_scroll: syncing_scroll_tick,
        syncing_follow: syncing_follow_tick,
        has_selection: has_selection_tick,
        pty_running: pty_running_tick,
        viewport_font_size: viewport_font_size_tick,
        find_resync: find_resync_tick,
        find_stats_tab: find_stats_tab_tick,
        find_pending: find_pending_tick,
        find_debounce: find_debounce_tick,
        pending_viewport_focus: pending_viewport_focus_tick,
        viewport_pixels,
    } = deps;

    let ticking = Arc::new(AtomicBool::new(false));
    let needs_retick = Arc::new(AtomicBool::new(false));
    let flood_pacing = Arc::new(AtomicBool::new(false));

    let schedule_host_tick: Arc<dyn Fn() + Send + Sync> = {
        let host_tick_queued = Arc::new(AtomicBool::new(false));
        let host_tick_queued_worker = host_tick_queued.clone();
        Arc::new(move || {
            if host_tick_queued.swap(true, Ordering::AcqRel) {
                return;
            }
            let host_tick_queued = host_tick_queued_worker.clone();
            let _ = slint::invoke_from_event_loop(move || {
                host_tick_queued.store(false, Ordering::Release);
                HOST_TICK.with(|slot| {
                    if let Some(f) = slot.borrow_mut().as_mut() {
                        f();
                    }
                });
            });
        })
    };

    let controls = TickControls {
        ticking: ticking.clone(),
        needs_retick: needs_retick.clone(),
        flood_pacing: flood_pacing.clone(),
        schedule_host_tick,
    };

    let ticking = controls.ticking.clone();
    let needs_retick = controls.needs_retick.clone();
    let flood_pacing = controls.flood_pacing.clone();

    HOST_TICK.with(|slot| {
        *slot.borrow_mut() = Some(Box::new(move || {
            if ticking.swap(true, Ordering::AcqRel) {
                needs_retick.store(true, Ordering::Release);
                return;
            }
            needs_retick.store(false, Ordering::Release);

            let Some(ui) = ui_weak.upgrade() else {
                ticking.store(false, Ordering::Release);
                return;
            };

            let occluded = window_should_pause_paint(
                ui.window(),
                window_occluded_tick.get(),
                presented_tick.get(),
            );

            let became_occluded = occluded && !was_occluded_tick.get();
            if was_occluded_tick.get() && !occluded {
                force_tick.set(true);
            }
            was_occluded_tick.set(occluded);

            // Apply a viewport focus deferred by on_viewport_focused when the
            // engine was already borrowed (issue #252). Must run before the
            // main `engine_tick.borrow_mut()` below.
            if let Some(focused) = pending_viewport_focus_tick.take() {
                let mut eng = engine_tick.borrow_mut();
                crate::viewport::apply_viewport_focus(
                    &ui.as_weak(),
                    &mut eng,
                    focused,
                    &force_tick,
                    &timer_tick,
                    &timer_fast_tick,
                    &logical_tick,
                );
            }

            let mut eng = engine_tick.borrow_mut();
            eng.tick();

            if occluded {
                // Drain events even when occluded, but keep one-shot
                // Status/Exit messages: a process exiting while minimized
                // must still update the status bar on restore (#198).
                while let Some(ev) = eng.poll_event_json() {
                    match parse_engine_event(&ev) {
                        Some(EngineEvent::Status { message }) => {
                            ui.set_status_text(SharedString::from(message));
                        }
                        Some(EngineEvent::Exit { code, .. }) => {
                            ui.set_status_text(SharedString::from(format!("exit {code}")));
                        }
                        _ => {}
                    }
                }
                if became_occluded || timer_fast_tick.get() {
                    set_occluded_timer(&timer_tick, &timer_fast_tick);
                }
                let more = needs_retick.load(Ordering::Acquire) || eng.take_pty_drain_pending();
                drop(eng);
                ticking.store(false, Ordering::Release);
                flood_pacing.store(more, Ordering::Release);
                return;
            }

            while let Some(ev) = eng.poll_event_json() {
                match parse_engine_event(&ev) {
                    Some(EngineEvent::Stats(stats)) => {
                        if apply_stats(
                            &stats,
                            &tabs_tick,
                            &terminals_tick,
                            &files_tick,
                            &projects_tick,
                            &filters_tick,
                            &ui,
                            &terminal_tab_tick,
                            &syncing_scroll_tick,
                            &has_selection_tick,
                            &pty_running_tick,
                            &viewport_font_size_tick,
                            &syncing_follow_tick,
                            &find_resync_tick,
                            &find_stats_tab_tick,
                            &find_pending_tick,
                            &find_debounce_tick,
                        ) {
                            force_tick.set(true);
                        }
                    }
                    Some(EngineEvent::Status { message }) => {
                        ui.set_status_text(SharedString::from(message));
                    }
                    Some(EngineEvent::Exit { code, .. }) => {
                        ui.set_status_text(SharedString::from(format!("exit {code}")));
                    }
                    _ => {}
                }
            }

            let dirty = eng.needs_render() || force_tick.get();
            let work_pending = eng.host_work_pending();
            // Keep fast cadence while flooding / file load / match scan even if
            // this frame was not dirty yet.
            if dirty || work_pending {
                bump_fast_timer(&timer_tick, &timer_fast_tick);
                flood_pacing.store(eng.pty_work_pending(), Ordering::Release);
            } else if timer_fast_tick.get() {
                timer_tick.set_interval(TICK_IDLE);
                timer_fast_tick.set(false);
                flood_pacing.store(false, Ordering::Release);
            }

            let (lw, lh) = *logical_tick.borrow();
            if lw <= 1.0 || lh <= 1.0 {
                let more = needs_retick.load(Ordering::Acquire) || eng.take_pty_drain_pending();
                drop(eng);
                ticking.store(false, Ordering::Release);
                flood_pacing.store(more, Ordering::Release);
                if more {
                    bump_fast_timer(&timer_tick, &timer_fast_tick);
                }
                return;
            }
            let scale = ui.window().scale_factor().max(0.5) as f32;
            let width = (lw * scale).ceil().max(1.0) as u32;
            let height = (lh * scale).ceil().max(1.0) as u32;
            ui.set_viewport_page_w(width as f32);
            ui.set_viewport_page_h(height as f32);

            // Caret overlay tracks focus/tab/running even when the Image is idle.
            let was_visible = ui.get_caret_visible();
            let now_visible = sync_terminal_caret(&ui, &eng, width, height, scale);
            // Shell often becomes ready after first focus — re-arm blink when caret appears.
            if now_visible && !was_visible {
                ui.set_caret_blink_on(true);
            }

            if !dirty {
                // Ingest-only: wait for TICK_FAST. Do not schedule_host_tick (busy drain).
                let more = needs_retick.load(Ordering::Acquire) || eng.take_pty_drain_pending();
                drop(eng);
                ticking.store(false, Ordering::Release);
                flood_pacing.store(more, Ordering::Release);
                if more {
                    bump_fast_timer(&timer_tick, &timer_fast_tick);
                }
                return;
            }
            force_tick.set(false);

            let mut pixels = viewport_pixels.borrow_mut();
            let reuse = matches!(
                pixels.as_ref(),
                Some((w, h, _)) if *w == width && *h == height
            );
            if !reuse {
                *pixels = Some((
                    width,
                    height,
                    SharedPixelBuffer::<Rgba8Pixel>::new(width, height),
                ));
            }
            let buffer = &mut pixels.as_mut().expect("viewport buffer").2;
            if let Err(err) = eng.render(width, height, buffer.make_mut_bytes()) {
                ui.set_status_text(SharedString::from(format!("render: {err}")));
                drop(eng);
                ticking.store(false, Ordering::Release);
                return;
            }
            // Position may change with scroll/follow after paint.
            let was_visible = ui.get_caret_visible();
            let now_visible = sync_terminal_caret(&ui, &eng, width, height, scale);
            if now_visible && !was_visible {
                ui.set_caret_blink_on(true);
            }
            let more = needs_retick.load(Ordering::Acquire) || eng.take_pty_drain_pending();
            drop(eng);
            // `buffer.clone()` is a refcount bump (SharedVector is shared), not a
            // pixel copy. The copy is required and happens once per frame either
            // way: the Image must own stable bytes for texture upload while the
            // engine rewrites the same SharedPixelBuffer next tick, so
            // `make_mut_bytes` above COW-copies it. A double-buffer flip could
            // only remove that copy at the cost of upload/render race risk —
            // deliberately not taken.
            ui.set_viewport_image(Image::from_rgba8(buffer.clone()));
            if !presented_tick.get() {
                presented_tick.set(true);
                ui.window().request_redraw();
            }

            ticking.store(false, Ordering::Release);
            flood_pacing.store(more, Ordering::Release);
            if more {
                bump_fast_timer(&timer_tick, &timer_fast_tick);
            }
        }));
    });

    controls
}

/// Shared tick body so the PTY reader can wake the UI without waiting for TICK_FAST.
///
/// Terminal-first: under flood drain at display cadence (~33 ms), not as fast as
/// the event loop. Immediate schedule_host_tick while pty_work_pending pinned a
/// core (`cat` → 100% CPU). Flood continuation: bump_fast_timer only. Echo
/// (no flood pacing) still wakes immediately.
pub(crate) fn install_pty_wake(engine: &Rc<RefCell<Engine>>, controls: &TickControls) {
    let ticking = controls.ticking.clone();
    let needs_retick = controls.needs_retick.clone();
    let flood_pacing = controls.flood_pacing.clone();
    let schedule_host_tick = controls.schedule_host_tick.clone();
    engine.borrow_mut().set_pty_activity_wake(Arc::new(move || {
        if ticking.load(Ordering::Acquire) {
            needs_retick.store(true, Ordering::Release);
            return;
        }
        // Flood: TICK_FAST owns polling. Echo (pacing off) wakes immediately.
        if flood_pacing.load(Ordering::Acquire) {
            return;
        }
        schedule_host_tick();
    }));
}

pub(crate) fn start_tick_timer(timer: &Timer) {
    timer.start(TimerMode::Repeated, TICK_FAST, move || {
        HOST_TICK.with(|slot| {
            if let Some(f) = slot.borrow_mut().as_mut() {
                f();
            }
        });
    });
}
