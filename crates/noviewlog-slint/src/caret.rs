//! Caret overlay helpers: engine geometry → Slint overlay, focus arming,
//! blink timer, and post-loop boot arming.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use noviewlog_core::{Command, Engine, CARET_BLINK_PERIOD};
use slint::{ComponentHandle, Timer, TimerMode};

use crate::ctx::Ctx;
use noviewlog_slint::ui::AppWindow;

/// Sync Slint caret overlay from engine geometry (device px → logical).
/// Returns whether the overlay is shown.
pub(crate) fn sync_terminal_caret(
    ui: &AppWindow,
    eng: &Engine,
    width: u32,
    height: u32,
    scale: f32,
) -> bool {
    if !eng.terminal_caret_active() {
        ui.set_caret_visible(false);
        return false;
    }
    let Some((x, y, w, h)) = eng.terminal_caret_rect(width, height) else {
        ui.set_caret_visible(false);
        return false;
    };
    let scale = scale.max(0.5);
    ui.set_caret_x(x / scale);
    ui.set_caret_y(y / scale);
    ui.set_caret_w(w / scale);
    ui.set_caret_h(h / scale);
    ui.set_caret_visible(true);
    true
}

/// Focus Terminal tab viewport + engine flag + overlay (startup / tab switch).
///
/// Takes the engine handle, not a borrowed `Engine`: `invoke_focus_viewport`
/// can synchronously re-enter `on_viewport_focused` (focus-viewport →
/// `has-focus` change), which borrows the same engine. The focus invocation
/// therefore happens BEFORE any `borrow_mut()` — a `RefMut` temporary in a
/// caller's argument expression would live across the re-entry and panic with
/// `BorrowMutError` (issues #232, #252).
pub(crate) fn arm_terminal_caret(
    ui: &AppWindow,
    engine: &Rc<RefCell<Engine>>,
    logical: (f32, f32),
) {
    ui.invoke_focus_viewport();
    let mut eng = engine.borrow_mut();
    let _ = eng.send_command(Command::SetViewportFocus { focused: true });
    eng.reset_caret_blink();
    ui.set_caret_blink_on(true);
    let scale = ui.window().scale_factor().max(0.5) as f32;
    let width = (logical.0 * scale).ceil().max(1.0) as u32;
    let height = (logical.1 * scale).ceil().max(1.0) as u32;
    let _ = sync_terminal_caret(ui, &eng, width, height, scale);
}

/// Blink only flips overlay opacity — never re-rasters the log Image.
/// The returned handle must stay alive for the UI lifetime (Drop stops the timer).
pub(crate) fn start_blink_timer(ui: &AppWindow) -> Rc<Timer> {
    let timer = Rc::new(Timer::default());
    let ui_blink = ui.as_weak();
    timer.start(TimerMode::Repeated, CARET_BLINK_PERIOD, move || {
        if let Some(ui) = ui_blink.upgrade() {
            if ui.get_caret_visible() {
                ui.set_caret_blink_on(!ui.get_caret_blink_on());
            }
        }
    });
    timer
}

/// Init / forward-focus can fire before Rust handlers exist, so engine never
/// learns viewport_focused=true until a later click. Arm caret after the loop starts.
pub(crate) fn install_boot_arm(
    ui: &AppWindow,
    engine: &Rc<RefCell<Engine>>,
    logical_size: Rc<RefCell<(f32, f32)>>,
    viewport_focused: Rc<Cell<bool>>,
    ctx: &Ctx,
) {
    let ui_boot = ui.as_weak();
    let schedule = move |delay_ms: u64| {
        let ui_boot = ui_boot.clone();
        let engine = engine.clone();
        let logical_size = logical_size.clone();
        let viewport_focused = viewport_focused.clone();
        let ctx = ctx.clone();
        let delayed = delay_ms > 0;
        Timer::single_shot(Duration::from_millis(delay_ms), move || {
            let Some(ui) = ui_boot.upgrade() else {
                return;
            };
            if ui.get_active_tab_index() != 0 {
                return;
            }
            // Don't steal focus the user already placed elsewhere (sidebar,
            // find bar) during the boot delay: their click sets
            // viewport_focused=false, so only re-arm while it is still true.
            if delayed && !viewport_focused.get() {
                return;
            }
            viewport_focused.set(true);
            // arm_terminal_caret invokes focus-viewport with NO engine borrow
            // held: it can re-enter on_viewport_focused, which borrows the
            // same engine (issues #232, #252).
            arm_terminal_caret(&ui, &engine, *logical_size.borrow());
            ctx.refresh();
        });
    };
    schedule(0);
    // Shell prompt / live screen may appear slightly after first paint.
    schedule(150);
}
