//! Winit title-drag / occlusion handler setup.

use std::cell::{Cell, RefCell};
use std::process::Stdio;
use std::rc::Rc;
#[cfg(target_os = "linux")]
use std::time::Duration;

use slint::winit_030::{winit, EventResult, WinitWindowAccessor};
use slint::{ComponentHandle, SharedString, Timer};

use crate::engine_bridge::{bump_fast_timer, set_occluded_timer};
use noviewlog_slint::ui::AppWindow;

/// About → slint.dev: wait for compositor token then spawn xdg-open.
struct PendingOpenUrl {
    url: String,
    serial: winit::event_loop::AsyncRequestSerial,
}

/// Open an https URL in the default browser, optionally with an XDG activation token
/// so Wayland/GNOME can raise the browser instead of leaving NoViewLog focused.
fn spawn_https_url(url: &str, activation_token: Option<&str>) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("only https urls are allowed".into());
    }
    let spawn = {
        #[cfg(target_os = "windows")]
        {
            let _ = activation_token;
            std::process::Command::new("cmd")
                .args(["/C", "start", "", url])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        #[cfg(target_os = "macos")]
        {
            let _ = activation_token;
            std::process::Command::new("open")
                .arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
        }
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        {
            let mut cmd = std::process::Command::new("xdg-open");
            cmd.arg(url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(token) = activation_token {
                cmd.env("XDG_ACTIVATION_TOKEN", token);
                cmd.env("DESKTOP_STARTUP_ID", token);
            }
            cmd.spawn()
        }
    };
    spawn.map(|_| ()).map_err(|err| err.to_string())
}

fn report_open_url(ui: &AppWindow, url: &str, result: Result<(), String>) {
    match result {
        Ok(()) => ui.set_status_text(SharedString::from(format!("Opened {url}"))),
        Err(err) => ui.set_status_text(SharedString::from(format!("open url: {err}"))),
    }
}

/// Install title-bar drag + occlusion / pointer resync + About URL-open handlers.
pub(crate) fn install(
    ui: &AppWindow,
    window_occluded: Rc<Cell<bool>>,
    force_render: Rc<Cell<bool>>,
    timer: Rc<Timer>,
    timer_fast: Rc<Cell<bool>>,
    viewport_presented: Rc<Cell<bool>>,
) {
    let window_occluded = window_occluded.clone();
    let force_render = force_render.clone();
    let viewport_presented = viewport_presented.clone();
    let timer = timer.clone();
    let timer_fast = timer_fast.clone();
    let ui_weak = ui.as_weak();
    // After drag_window(), compositor eats button-up → clear Slint grab on next move.
    let resync_pointer_after_drag = Rc::new(Cell::new(false));
    let resync_for_events = resync_pointer_after_drag.clone();
    // Last cursor (logical px) — MouseInput has no position.
    let last_cursor = Rc::new(Cell::new((0.0f32, 0.0f32)));
    // False until the first CursorMoved: a press before any move event must
    // not be hit-tested at the (0,0) default (#198).
    let cursor_seen = Rc::new(Cell::new(false));
    // Press origin while chrome menu open + press hit title-drag gap (VS Code-like).
    let pending_title_drag = Rc::new(Cell::new(None::<(f32, f32)>));
    const TITLE_DRAG_SLOP: f32 = 4.0;
    let pending_open_url = Rc::new(RefCell::new(None::<PendingOpenUrl>));

    // Title-bar drag → winit drag_window (Wayland/X11 interactive move).
    // Compositor consumes the button release, so Slint's TouchArea grab/pressed
    // stays sticky until we resync on the next CursorMoved.
    let begin_title_drag = Rc::new({
        let ui_weak = ui.as_weak();
        let resync = resync_pointer_after_drag.clone();
        move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let _ = ui.window().with_winit_window(|winit_window| {
                let _ = winit_window.drag_window();
            });
            // Clear immediately in case no move arrives until a click; also arm
            // CursorMoved resync so hover/click work without a priming click.
            ui.window()
                .dispatch_event(slint::platform::WindowEvent::PointerExited);
            resync.set(true);
        }
    });

    {
        let begin_title_drag = begin_title_drag.clone();
        ui.on_title_bar_drag(move || begin_title_drag());
    }

    {
        let ui_open = ui.as_weak();
        #[cfg(target_os = "linux")]
        let pending_open_url = pending_open_url.clone();
        ui.on_open_url(move |url| {
            let url = url.to_string();
            let Some(ui) = ui_open.upgrade() else {
                return;
            };
            if !url.starts_with("https://") {
                ui.set_status_text(SharedString::from("open url: only https urls are allowed"));
                return;
            }

            #[cfg(target_os = "linux")]
            {
                use winit::platform::startup_notify::WindowExtStartupNotify;
                let requested = ui
                    .window()
                    .with_winit_window(|winit_window| winit_window.request_activation_token());
                match requested {
                    Some(Ok(serial)) => {
                        *pending_open_url.borrow_mut() = Some(PendingOpenUrl {
                            url: url.clone(),
                            serial,
                        });
                        // Compositor may not deliver a token; still open after a short wait.
                        let pending = pending_open_url.clone();
                        let ui_fallback = ui.as_weak();
                        Timer::single_shot(Duration::from_millis(200), move || {
                            let Some(pending) = pending.borrow_mut().take() else {
                                return;
                            };
                            if let Some(ui) = ui_fallback.upgrade() {
                                report_open_url(
                                    &ui,
                                    &pending.url,
                                    spawn_https_url(&pending.url, None),
                                );
                            } else {
                                let _ = spawn_https_url(&pending.url, None);
                            }
                        });
                        return;
                    }
                    Some(Err(_)) | None => {}
                }
            }

            report_open_url(&ui, &url, spawn_https_url(&url, None));
        });
    }

    let last_cursor_ev = last_cursor.clone();
    let cursor_seen_ev = cursor_seen.clone();
    let pending_title_drag_ev = pending_title_drag.clone();
    let begin_title_drag_ev = begin_title_drag.clone();
    let pending_open_ev = pending_open_url.clone();
    let ui_open_ev = ui_weak.clone();
    ui.window().on_winit_window_event(move |_win, event| {
        match event {
            winit::event::WindowEvent::Occluded(occluded) => {
                let was = window_occluded.get();
                window_occluded.set(*occluded);
                if *occluded {
                    // Drop to slow cadence immediately — don't wait for the next 33ms tick.
                    // Keep 33ms until the first viewport present (launch-from-terminal).
                    if viewport_presented.get() {
                        set_occluded_timer(&timer, &timer_fast);
                    }
                } else if was {
                    force_render.set(true);
                    bump_fast_timer(&timer, &timer_fast);
                }
            }
            winit::event::WindowEvent::Resized(size) => {
                if size.width == 0 || size.height == 0 {
                    // Windows / some compositors signal minimize via zero size.
                    window_occluded.set(true);
                    if viewport_presented.get() {
                        set_occluded_timer(&timer, &timer_fast);
                    }
                }
            }
            winit::event::WindowEvent::ActivationTokenDone { serial, token } => {
                let mut pending = pending_open_ev.borrow_mut();
                if pending.as_ref().is_some_and(|p| p.serial == *serial) {
                    if let Some(p) = pending.take() {
                        let raw = token.clone().into_raw();
                        let result = spawn_https_url(&p.url, Some(&raw));
                        if let Some(ui) = ui_open_ev.upgrade() {
                            report_open_url(&ui, &p.url, result);
                        }
                    }
                }
            }
            winit::event::WindowEvent::CursorMoved { position, .. } => {
                let Some(ui) = ui_weak.upgrade() else {
                    return EventResult::Propagate;
                };
                let scale = ui.window().scale_factor();
                let logical = position.to_logical::<f64>(scale as f64);
                let lx = logical.x as f32;
                let ly = logical.y as f32;
                last_cursor_ev.set((lx, ly));
                cursor_seen_ev.set(true);

                if resync_for_events.get() {
                    resync_for_events.set(false);
                    // Tear down stuck press/grab, then re-enter at the real cursor.
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerExited);
                    ui.window()
                        .dispatch_event(slint::platform::WindowEvent::PointerMoved {
                            position: slint::LogicalPosition::new(lx, ly),
                        });
                }

                // Armed title-bar drag while Popup blocked the gap TouchArea.
                if let Some((px, py)) = pending_title_drag_ev.get() {
                    if (lx - px).abs() >= TITLE_DRAG_SLOP || (ly - py).abs() >= TITLE_DRAG_SLOP {
                        pending_title_drag_ev.set(None);
                        begin_title_drag_ev();
                    }
                }

                // PopupWindow blocks main-window hover; arm menubar via geometry hit-test.
                if ui.get_menu_bar_active() {
                    ui.invoke_menu_bar_pointer_moved(lx, ly);
                }
            }
            winit::event::WindowEvent::MouseInput { state, button, .. } => {
                use winit::event::{ElementState, MouseButton};
                if *button != MouseButton::Left {
                    return EventResult::Propagate;
                }
                match state {
                    ElementState::Pressed => {
                        let Some(ui) = ui_weak.upgrade() else {
                            return EventResult::Propagate;
                        };
                        // Without a prior CursorMoved the stored position is
                        // the (0,0) default — hit-testing there would
                        // misfire on click-to-focus (see #198).
                        if !cursor_seen_ev.get() {
                            return EventResult::Propagate;
                        }
                        let (lx, ly) = last_cursor_ev.get();
                        // Popup blocks gap TouchArea — close menus + arm drag on same press.
                        if ui.get_menu_bar_active() && ui.invoke_title_bar_chrome_press(lx, ly) {
                            pending_title_drag_ev.set(Some((lx, ly)));
                        }
                    }
                    ElementState::Released => {
                        pending_title_drag_ev.set(None);
                    }
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });
}
