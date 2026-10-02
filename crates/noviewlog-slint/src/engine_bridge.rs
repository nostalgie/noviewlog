//! Timer/occlusion helpers and small engine↔UI bridge utilities.

use std::cell::Cell;
use std::time::Duration;

use slint::winit_030::WinitWindowAccessor;
use slint::{Model, Timer, VecModel};

use noviewlog_slint::ui::TerminalInfo;

/// Interactive / dirty / caret-blink cadence (~30 Hz).
pub(crate) const TICK_FAST: Duration = Duration::from_millis(33);
/// Idle poll when nothing needs paint and caret is inactive.
pub(crate) const TICK_IDLE: Duration = Duration::from_millis(250);
/// Minimized / occluded: PTY ingest only, no RGBA paint / Slint model churn.
pub(crate) const TICK_OCCLUDED: Duration = Duration::from_millis(500);

pub(crate) fn bump_fast_timer(timer: &Timer, timer_fast: &Cell<bool>) {
    if !timer_fast.get() {
        timer.set_interval(TICK_FAST);
        timer_fast.set(true);
    }
}

pub(crate) fn set_occluded_timer(timer: &Timer, timer_fast: &Cell<bool>) {
    timer.set_interval(TICK_OCCLUDED);
    timer_fast.set(false);
}

/// True when the window should not drive paints.
///
/// On Wayland, Slint's `Window::is_minimized()` often never flips (winit returns
/// `None`), and `is_visible()` only means the component is shown — not compositor
/// occlusion. Prefer the `window_occluded` flag fed by `WindowEvent::Occluded`.
///
/// Until one viewport frame has been uploaded, never pause: launch-from-terminal
/// often maps the window occluded/unfocused, and skipping that first present
/// leaves an empty Image hole in a transparent swapchain.
pub(crate) fn window_should_pause_paint(
    window: &slint::Window,
    occluded_flag: bool,
    presented_once: bool,
) -> bool {
    if !presented_once {
        return false;
    }
    if occluded_flag || window.is_minimized() {
        return true;
    }
    // Secondary probe: some X11 paths update winit before Slint's property.
    window
        .with_winit_window(|w| w.is_minimized() == Some(true))
        .unwrap_or(false)
}

pub(crate) fn find_terminal_index(model: &VecModel<TerminalInfo>, id: &str) -> Option<i32> {
    for i in 0..model.row_count() {
        if let Some(t) = model.row_data(i) {
            if t.id.as_str() == id {
                return Some(i as i32);
            }
        }
    }
    None
}

/// Engine global session index (`TerminalInfo.index`) across TERMINALS + FILES models.
pub(crate) fn find_session_global_index(
    terminals: &VecModel<TerminalInfo>,
    files: &VecModel<TerminalInfo>,
    id: &str,
) -> Option<i32> {
    for model in [terminals, files] {
        for i in 0..model.row_count() {
            if let Some(t) = model.row_data(i) {
                if t.id.as_str() == id {
                    return Some(t.index);
                }
            }
        }
    }
    None
}
