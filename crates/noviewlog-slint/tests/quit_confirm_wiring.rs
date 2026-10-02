//! Source-level wiring: quit confirm overlay and Rust close intercept must stay hooked.

use std::fs;
use std::path::PathBuf;

fn app_slint() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/app.slint");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn main_rs() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn quit_overlay_uses_form_dialog_and_hide() {
    let src = app_slint();
    assert!(
        src.contains("public function open-quit-confirm()"),
        "Rust must be able to show the quit overlay"
    );
    assert!(
        src.contains("title: \"Close NoViewLog?\""),
        "quit overlay title copy"
    );
    assert!(
        src.contains("Running terminals will stop. Project settings stay saved."),
        "quit overlay body copy"
    );
    assert!(
        src.contains("ok-text: \"Close\""),
        "primary action is Close"
    );
    let confirm = src.find("function confirm-quit()").expect("confirm-quit");
    let chunk = &src[confirm..confirm.saturating_add(160).min(src.len())];
    assert!(
        chunk.contains("root.hide()"),
        "confirm must hide() so close-requested does not re-enter"
    );
    assert!(
        !chunk.contains("root.close()"),
        "confirm must not call close() (would show the overlay again)"
    );
}

#[test]
fn rust_close_requested_keeps_window_and_opens_overlay() {
    let src = main_rs();
    assert!(
        src.contains("on_close_requested"),
        "Window::on_close_requested must intercept caption X, File Exit, Alt+F4"
    );
    assert!(
        src.contains("invoke_open_quit_confirm"),
        "close-requested must show the Slint overlay"
    );
    assert!(
        src.contains("KeepWindowShown"),
        "close-requested must keep the window until Confirm"
    );
}
