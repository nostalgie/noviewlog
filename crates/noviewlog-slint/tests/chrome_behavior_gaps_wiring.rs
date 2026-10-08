//! Issue #333: three small chrome behavior contracts that live in Slint markup
//! and are easy to regress when menus/chips move between files.

use std::fs;
use std::path::PathBuf;

fn read_ui(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join(file);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn menu_bar_active_includes_severity_menu() {
    let src = read_ui("title-bar.slint");
    let idx = src
        .find("out property <bool> menu-bar-active:")
        .expect("menu-bar-active binding");
    let line = src[idx..].lines().next().expect("binding line");
    assert!(
        line.contains("severity-menu.is-open"),
        "menu-bar-active must include severity-menu so hover-switch works \
         while the severity flyout is open; got: {line}"
    );
}

#[test]
fn tab_chip_middle_click_close_gated_by_show_close() {
    let src = read_ui("sidebar.slint");
    let chip = src
        .find("export component TabChip")
        .expect("TabChip component");
    let after_decl = chip + "export component".len();
    let body_end = src[after_decl..]
        .find("export component")
        .map(|off| after_decl + off)
        .unwrap_or(src.len());
    let body = &src[chip..body_end];
    let mid = body
        .find("PointerEventButton.middle")
        .expect("middle-click handler on TabChip");
    // Look back a short window for the show-close gate on the same condition.
    let window_start = mid.saturating_sub(80);
    let window = &body[window_start..mid + 40];
    assert!(
        window.contains("show-close"),
        "TabChip middle-click close must be gated by show-close; nearby: {window}"
    );
}

#[test]
fn projects_delete_dialog_enter_confirms() {
    let src = read_ui("projects-dialog.slint");
    let keys = src.find("dialog-keys := FocusScope").expect("dialog-keys");
    let chunk = &src[keys..keys.saturating_add(700).min(src.len())];
    assert!(
        chunk.contains("Key.Return") || chunk.contains("\"\\n\""),
        "dialog-keys must handle Enter/Return"
    );
    assert!(
        chunk.contains("confirm-delete") && chunk.contains("confirm-form()"),
        "Enter in confirm-delete mode must call confirm-form()"
    );
}
