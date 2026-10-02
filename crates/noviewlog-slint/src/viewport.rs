//! Viewport wiring: resize/focus, zoom, wrap/follow toggles, scrolling.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use noviewlog_core::{Command, Engine};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, Timer, Weak};

use crate::caret::sync_terminal_caret;
use crate::ctx::Ctx;
use crate::engine_bridge::bump_fast_timer;
use noviewlog_slint::ui::AppWindow;

/// Placeholder fill matching `Theme.bg-window` (`#0d1117`) so the first `Image`
/// is opaque on a transparent winit swapchain.
const VIEWPORT_PLACEHOLDER_RGBA: [u8; 4] = [0x0d, 0x11, 0x17, 0xff];

pub(crate) fn seed_opaque_viewport(ui: &AppWindow) {
    let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(8, 8);
    for px in buffer.make_mut_bytes().chunks_exact_mut(4) {
        px.copy_from_slice(&VIEWPORT_PLACEHOLDER_RGBA);
    }
    ui.set_viewport_image(Image::from_rgba8(buffer));
}

pub(crate) fn install_resized(
    ui: &AppWindow,
    logical_size: Rc<RefCell<(f32, f32)>>,
    ctx: &Ctx,
    window_occluded: Rc<Cell<bool>>,
    viewport_presented: Rc<Cell<bool>>,
) {
    let force_render = ctx.force_render.clone();
    let timer = ctx.timer.clone();
    let timer_fast = ctx.timer_fast.clone();
    ui.on_viewport_resized(move |width, height| {
        if width > 1.0 && height > 1.0 {
            *logical_size.borrow_mut() = (width, height);
            force_render.set(true);
            // Don't re-arm 33ms while occluded — unless we still owe the first present.
            if !window_occluded.get() || !viewport_presented.get() {
                bump_fast_timer(&timer, &timer_fast);
            }
        }
    });
}

pub(crate) fn install_focused(
    ui: &AppWindow,
    ctx: &Ctx,
    viewport_focused: Rc<Cell<bool>>,
    logical_size: Rc<RefCell<(f32, f32)>>,
) {
    let engine = ctx.engine.clone();
    let force_render = ctx.force_render.clone();
    let timer = ctx.timer.clone();
    let timer_fast = ctx.timer_fast.clone();
    let pending_viewport_focus = ctx.pending_viewport_focus.clone();
    let ui_focus = ui.as_weak();
    ui.on_viewport_focused(move |focused| {
        viewport_focused.set(focused);
        // Re-entry guard (issue #252): a synchronous caller may still hold an
        // engine borrow across focus-viewport. Never panic — defer the engine
        // update to the next tick instead.
        let Ok(mut eng) = engine.try_borrow_mut() else {
            pending_viewport_focus.set(Some(focused));
            return;
        };
        // This handler just observed the freshest focus state — a stale
        // deferred value (from an earlier re-entry) must not override it.
        pending_viewport_focus.set(None);
        apply_viewport_focus(
            &ui_focus,
            &mut eng,
            focused,
            &force_render,
            &timer,
            &timer_fast,
            &logical_size,
        );
    });
}

/// Apply a resolved viewport-focus change to the engine + caret overlay.
/// Caller must hold the engine borrow; shared by the live handler and the
/// deferred next-tick application in `tick.rs` (issue #252).
pub(crate) fn apply_viewport_focus(
    ui: &Weak<AppWindow>,
    eng: &mut Engine,
    focused: bool,
    force_render: &Rc<Cell<bool>>,
    timer: &Rc<Timer>,
    timer_fast: &Rc<Cell<bool>>,
    logical_size: &Rc<RefCell<(f32, f32)>>,
) {
    let _ = eng.send_command(Command::SetViewportFocus { focused });
    if focused {
        eng.reset_caret_blink();
        force_render.set(true);
        bump_fast_timer(timer, timer_fast);
        if let Some(ui) = ui.upgrade() {
            ui.set_caret_blink_on(true);
            let (lw, lh) = *logical_size.borrow();
            let scale = ui.window().scale_factor().max(0.5) as f32;
            let width = (lw * scale).ceil().max(1.0) as u32;
            let height = (lh * scale).ceil().max(1.0) as u32;
            let _ = sync_terminal_caret(&ui, eng, width, height, scale);
        }
    } else if let Some(ui) = ui.upgrade() {
        ui.set_caret_visible(false);
    }
}

/// Send a zoom step and remember the size locally (`Zoom` menu + Ctrl shortcuts).
pub(crate) fn apply_zoom(ctx: &Ctx, viewport_font_size: &Rc<Cell<f32>>, next: f32) {
    let _ = ctx.send(Command::SetViewportFontSize { size: next });
    viewport_font_size.set(next);
    ctx.refresh();
}

pub(crate) fn install_zoom(ui: &AppWindow, ctx: &Ctx, viewport_font_size: Rc<Cell<f32>>) {
    {
        let ctx = ctx.clone();
        let viewport_font_size = viewport_font_size.clone();
        ui.on_zoom_in(move || {
            let next = (viewport_font_size.get() + 1.0).clamp(8.0, 32.0);
            apply_zoom(&ctx, &viewport_font_size, next);
        });
    }
    {
        let ctx = ctx.clone();
        let viewport_font_size = viewport_font_size.clone();
        ui.on_zoom_out(move || {
            let next = (viewport_font_size.get() - 1.0).clamp(8.0, 32.0);
            apply_zoom(&ctx, &viewport_font_size, next);
        });
    }
    {
        let ctx = ctx.clone();
        let viewport_font_size = viewport_font_size.clone();
        ui.on_zoom_reset(move || {
            apply_zoom(&ctx, &viewport_font_size, 13.0);
        });
    }
    {
        let ctx = ctx.clone();
        let viewport_font_size = viewport_font_size.clone();
        ui.on_viewport_zoom_wheel(move |delta_y| {
            // Zero-delta events (trackpad momentum end) must not zoom.
            if delta_y == 0.0 {
                return;
            }
            let step = if delta_y > 0.0 { 1.0 } else { -1.0 };
            let next = (viewport_font_size.get() + step).clamp(8.0, 32.0);
            if (next - viewport_font_size.get()).abs() < f32::EPSILON {
                return;
            }
            apply_zoom(&ctx, &viewport_font_size, next);
        });
    }
}

pub(crate) fn install_wrap(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    let ui_wrap = ui.as_weak();
    ui.on_set_wrap_lines(move |wrap| {
        if let Some(ui) = ui_wrap.upgrade() {
            ui.set_wrap_lines(wrap);
        }
        ctx.send_refresh(Command::SetWrapLines { wrap });
    });
}

pub(crate) fn install_follow(ui: &AppWindow, ctx: &Ctx, syncing_follow: Rc<Cell<bool>>) {
    let ctx = ctx.clone();
    let ui_follow = ui.as_weak();
    // Slint set-follow → engine SetFollow; stats field is auto_follow / property auto-follow.
    ui.on_set_follow(move |follow| {
        if syncing_follow.get() {
            return;
        }
        if let Some(ui) = ui_follow.upgrade() {
            ui.set_auto_follow(follow);
        }
        ctx.send_refresh(Command::SetFollow { follow });
    });
}

pub(crate) fn install_scroll(ui: &AppWindow, ctx: &Ctx, syncing_scroll: Rc<Cell<bool>>) {
    {
        let ctx = ctx.clone();
        ui.on_viewport_scrolled(move |delta_y| {
            // Zero-delta events (trackpad momentum end) must not scroll.
            if delta_y == 0.0 {
                return;
            }
            let lines = if delta_y > 0.0 { -3 } else { 3 };
            ctx.send_refresh(Command::ScrollLines { delta: lines });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_viewport_scrolled_x(move |delta_x| {
            // Logical-ish step; engine clamps to max_scroll_x. Zero-delta
            // wheel events (momentum end) must not scroll.
            if delta_x == 0.0 {
                return;
            }
            let delta = if delta_x > 0.0 { -40.0 } else { 40.0 };
            ctx.send_refresh(Command::ScrollHorizontal { delta });
        });
    }
    {
        let ctx = ctx.clone();
        let syncing_scroll = syncing_scroll.clone();
        ui.on_viewport_scroll_y_changed(move |value| {
            if syncing_scroll.get() {
                return;
            }
            ctx.send_refresh(Command::Scroll { offset: value });
        });
    }
    {
        let ctx = ctx.clone();
        let syncing_scroll = syncing_scroll.clone();
        ui.on_viewport_scroll_x_changed(move |value| {
            if syncing_scroll.get() {
                return;
            }
            ctx.send_refresh(Command::SetScrollX { offset: value });
        });
    }
}
