//! Source-level UI wiring: dead-space and click-away must stay hooked.
//! A stretch Rectangle under FILES previously swallowed clicks.

use std::fs;
use std::path::PathBuf;

fn app_slint() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/app.slint");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

fn sidebar_slint() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/sidebar.slint");
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

#[test]
fn sidebar_dead_space_is_toucharea_that_dismisses_rename() {
    let src = app_slint();
    assert!(
        src.contains("sidebar-dead-space := TouchArea"),
        "leftover sidebar height must be a TouchArea (not a silent Rectangle)"
    );
    let idx = src
        .find("sidebar-dead-space := TouchArea")
        .expect("sidebar-dead-space");
    let chunk = &src[idx..idx.saturating_add(500).min(src.len())];
    assert!(
        chunk.contains("dismiss-any-rename-if-any()"),
        "sidebar-dead-space must call dismiss-any-rename-if-any"
    );
    assert!(
        !chunk.contains("background: transparent;"),
        "do not replace dead-space with a non-interactive fill"
    );
}

#[test]
fn viewport_press_dismisses_rename() {
    let src = app_slint();
    assert!(src.contains("function dismiss-any-rename-if-any()"));
    assert!(src.contains("viewport-host.focus();"));
    // Anchor at the viewport TouchArea itself: it is the only I-beam cursor
    // region (`mouse-cursor: text`). The FIRST PointerEventKind.down in the
    // file belongs to the sidebar dead-space handler — anchoring there used to
    // keep this test green even with the viewport wiring deleted.
    assert_eq!(
        src.matches("mouse-cursor: text;").count(),
        1,
        "viewport TouchArea anchor (I-beam cursor) must stay unique"
    );
    let area = src.find("mouse-cursor: text;").expect("viewport TouchArea");
    let down = src[area..]
        .find("if (event.kind == PointerEventKind.down)")
        .expect("viewport pointer down")
        + area;
    // Wide window: the selection wiring follows the right-click branch and
    // explanatory comments (~1800 bytes).
    let window = &src[down..down.saturating_add(2200).min(src.len())];
    // The window must be the viewport handler (starts selection via
    // viewport-pointer), not the sidebar dead-space TouchArea.
    assert!(
        window.contains("root.viewport-pointer("),
        "anchored window must be the viewport TouchArea handler"
    );
    assert!(
        window.contains("dismiss-any-rename-if-any()") && window.contains("viewport-host.focus();"),
        "viewport pointer-down must dismiss rename and focus the viewport"
    );
}

#[test]
fn files_and_terminals_headers_dismiss_rename() {
    let src = app_slint();
    assert!(src.contains("dismiss-any-rename-if-any(); root.toggle-files-section()"));
    assert!(src.contains("dismiss-any-rename-if-any(); root.toggle-terminals-section()"));
}

#[test]
fn files_rows_cannot_rename() {
    let src = app_slint();
    let files = src
        .find("for file in root.files-model: TerminalRow")
        .expect("files TerminalRow");
    let chunk = &src[files..files.saturating_add(1200).min(src.len())];
    assert!(
        chunk.contains("can-rename: false"),
        "FILES TerminalRow must set can-rename: false"
    );
    assert!(
        !chunk.contains("start-terminal-rename(file.id"),
        "FILES must not wire start-terminal-rename"
    );
}

#[test]
fn terminals_rows_can_rename() {
    let src = app_slint();
    let terms = src
        .find("for term in root.terminals-model: TerminalRow")
        .expect("terminals TerminalRow");
    let chunk = &src[terms..terms.saturating_add(1200).min(src.len())];
    assert!(
        chunk.contains("can-rename: true") || chunk.contains("start-terminal-rename(term.id"),
        "TERMINALS rows must allow rename"
    );
}

#[test]
fn terminal_row_keeps_one_title_subtitle_stack() {
    let src = sidebar_slint();
    let idx = src.find("title-slot := Rectangle").expect("title-slot");
    let chunk = &src[idx..idx.saturating_add(2500).min(src.len())];
    assert!(
        chunk.contains("height: Theme.rename-terminal-height"),
        "title line must have a fixed height in idle and rename"
    );
    assert!(
        !chunk.contains("if !root.renaming: VerticalLayout"),
        "do not swap a second VerticalLayout for rename — subtitle would jump"
    );
}

#[test]
fn empty_files_list_height_stays_zero_in_slint() {
    let src = app_slint();
    assert!(
        src.contains("if (!expanded || count <= 0)") && src.contains("return 0px;"),
        "empty FILES list stays 0px — dead-space TouchArea is the hit target"
    );
}

#[test]
fn status_bar_press_dismisses_rename() {
    // The strip is the `StatusBar` component (status-bar.slint): it reports
    // pointer-down via `pressed-down`, and the host wires that to the dismiss.
    let src = app_slint();
    let idx = src.find("StatusBar {").expect("StatusBar instance");
    let window = &src[idx..idx.saturating_add(500).min(src.len())];
    assert!(
        window.contains("pressed-down => { root.dismiss-any-rename-if-any(); }"),
        "status bar must dismiss rename on pointer-down"
    );
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/status-bar.slint");
    let bar = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let down = bar
        .find("if (event.kind == PointerEventKind.down)")
        .expect("StatusBar pointer-down handler");
    assert!(
        bar[down..down.saturating_add(120).min(bar.len())].contains("root.pressed-down();"),
        "StatusBar must fire pressed-down on pointer-down"
    );
}

#[test]
fn launch_preview_press_dismisses_rename() {
    let src = app_slint();
    let idx = src
        .find("root.launch-preview-text")
        .expect("launch-preview-text");
    let window = &src[idx.saturating_sub(700)..idx];
    assert!(
        window.contains("dismiss-any-rename-if-any()"),
        "launch preview strip must dismiss rename on pointer-down"
    );
}

#[test]
fn rename_fields_use_even_inner_padding() {
    let theme = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/theme.slint");
    let theme = fs::read_to_string(&theme).unwrap();
    assert!(theme.contains("rename-pad:"));
    assert!(theme.contains("field-pad-x:"));
    assert!(theme.contains("field-pad-y:"));
    let src = sidebar_slint();
    assert!(
        src.contains("padding-left: Theme.field-pad-x")
            && src.contains("padding-top: Theme.field-pad-y")
            && src.contains("padding-right: Theme.field-pad-x")
            && src.contains("padding-bottom: Theme.field-pad-y"),
        "tab rename must use even Theme.field-pad inset"
    );
    let idx = src.find("title-slot := Rectangle").expect("title-slot");
    let chunk = &src[idx..idx.saturating_add(2200).min(src.len())];
    assert!(
        chunk.matches("x: Theme.rename-pad").count() >= 2
            && chunk
                .matches("width: parent.width - 2 * Theme.rename-pad")
                .count()
                >= 2,
        "TERMINALS rename and idle title must inset by Theme.rename-pad on left and right"
    );
}

#[test]
fn form_text_field_has_even_inner_padding() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui/form-dialogs.slint");
    let src = fs::read_to_string(&path).unwrap();
    assert!(src.contains("padding-left: Theme.field-pad-x"));
    assert!(src.contains("padding-right: Theme.field-pad-x"));
    assert!(src.contains("padding-top: Theme.field-pad-y"));
    assert!(src.contains("padding-bottom: Theme.field-pad-y"));
    assert!(src.contains("vertical-alignment: center"));
}

#[test]
fn viewport_scrollbar_press_dismisses_rename() {
    // Issue #80: EngineScrollBar must fire `press` on left-button down and the
    // viewport V/H scrollbars must wire it to dismiss-any-rename-if-any().
    let sidebar = sidebar_slint();
    assert!(
        sidebar.contains("callback press();"),
        "EngineScrollBar must expose a press callback"
    );
    // Anchor at the EngineScrollBar callback: earlier components in the file
    // have identical pointer-down handlers.
    let bar_idx = sidebar
        .find("callback press();")
        .expect("EngineScrollBar press callback");
    let touch_idx = sidebar[bar_idx..]
        .find(
            "if (event.button == PointerEventButton.left && event.kind == PointerEventKind.down) {",
        )
        .expect("scrollbar pointer down handler")
        + bar_idx;
    let chunk = &sidebar[touch_idx..touch_idx.saturating_add(120).min(sidebar.len())];
    assert!(
        chunk.contains("root.press();"),
        "scrollbar left-button down must fire root.press()"
    );

    let src = app_slint();
    let wired = src
        .matches("press() => { root.dismiss-any-rename-if-any(); }")
        .count();
    assert!(
        wired >= 2,
        "both viewport scrollbars must dismiss rename on press (found {wired})"
    );
}

#[test]
fn filters_panel_controls_dismiss_rename() {
    // Issue #320: the FILTERS panel lives on filter tabs — exactly where
    // inline rename happens. The always-on rule: any pointer-down outside
    // the rename TextInput must dismiss it (FilterRow rows already do).
    let src = app_slint();

    // Anchor at each control's visible label and require the dismiss call
    // inside the control's own handler: buttons place `clicked => {` just
    // before the label (chunk = that handler up to the label), the
    // ModeToggle places `toggled` after it.
    let clicked_anchors: &[(&str, &str)] = &[
        ("+ Include button", "text: \"+ Include\";"),
        ("- Exclude button", "text: \"- Exclude\";"),
        ("clear-all-filters button", "text: \"Clear all filters\";"),
    ];
    for (name, anchor) in clicked_anchors {
        let idx = src
            .find(anchor)
            .unwrap_or_else(|| panic!("{name} anchor {anchor:?} missing"));
        let clicked = src[..idx]
            .rfind("clicked => {")
            .unwrap_or_else(|| panic!("{name}: no clicked handler before {anchor:?}"));
        assert!(
            src[clicked..idx].contains("dismiss-any-rename-if-any()"),
            "{name} must dismiss inline rename before acting (issue #320)"
        );
    }
    let mode = src
        .find("label: \".*\";")
        .expect("regex ModeToggle anchor missing");
    let mode_chunk = &src[mode..(mode + 400).min(src.len())];
    assert!(
        mode_chunk.contains("dismiss-any-rename-if-any()"),
        "regex ModeToggle must dismiss inline rename before acting (issue #320)"
    );
}

#[test]
fn rename_state_machine_lives_in_one_component() {
    // TabChip and TerminalRow supply chrome only; the init latch, blur-commit
    // gating and Escape handling (focus-race guards) live in InlineRenameInput.
    let src = sidebar_slint();
    let input = src
        .find("export component InlineRenameInput")
        .expect("InlineRenameInput component");
    let input_end = src[input + 1..]
        .find("export component")
        .map_or(src.len(), |off| input + 1 + off);
    let body = &src[input..input_end];
    for needle in [
        "had-focus",
        "closing",
        "self.focus();",
        "self.select-all();",
        "Key.Escape",
    ] {
        assert!(
            body.contains(needle),
            "InlineRenameInput must own the rename state machine ({needle})"
        );
    }
    for host in [
        "export component TabChip",
        "export component TerminalRow inherits",
    ] {
        let start = src.find(host).expect(host);
        let end = src[start + 1..]
            .find("export component")
            .map_or(src.len(), |off| start + 1 + off);
        let chunk = &src[start..end];
        assert!(
            chunk.contains("InlineRenameInput {"),
            "{host} must host InlineRenameInput"
        );
        assert!(
            !chunk.contains("rename-had-focus") && !chunk.contains("TextInput {"),
            "{host} must not re-implement the rename TextInput state machine"
        );
    }
}
