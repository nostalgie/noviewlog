//! Source-level guard: `update-tab-drop` in app.slint hardcodes a chip pitch
//! (120.0) that must match TabChip's actual laid-out width (preferred-width,
//! horizontal-stretch 0). The values live in different files; this test parses
//! both and cross-checks them so changing one without the other fails.

use std::fs;
use std::path::PathBuf;

fn read_ui(file: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ui")
        .join(file);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// First `key: value;` float after `from` in `src`.
fn parse_px(src: &str, from: usize, key: &str) -> f64 {
    let idx = src[from..]
        .find(&format!("{key}: "))
        .unwrap_or_else(|| panic!("{key} not found"));
    let rest = &src[from + idx + key.len() + 2..];
    let value = rest.split("px").next().expect("px unit").trim();
    value
        .parse()
        .unwrap_or_else(|e| panic!("parse {key}={value:?}: {e}"))
}

#[test]
fn tab_drop_pitch_matches_tab_chip_layout() {
    // Pitch literal in update-tab-drop (app.slint).
    let app = read_ui("app.slint");
    let pitch_idx = app
        .find("let pitch = ")
        .expect("update-tab-drop pitch literal");
    let rest = &app[pitch_idx + "let pitch = ".len()..];
    let value = rest.split(';').next().expect("pitch statement").trim();
    let pitch: f64 = value.parse().expect("pitch literal parses as f64");

    // TabChip laid-out width (sidebar.slint): with horizontal-stretch: 0 every
    // chip renders at preferred-width, which is what the pitch must equal.
    let sidebar = read_ui("sidebar.slint");
    let chip = sidebar
        .find("export component TabChip")
        .expect("TabChip component");
    // Bound the scan to the TabChip body (declaration to the next `export
    // component`): `horizontal-stretch: 0` elsewhere in sidebar.slint must
    // not satisfy this assertion (#255).
    let after_decl = chip + "export component".len();
    let body_end = sidebar[after_decl..]
        .find("export component")
        .map(|off| after_decl + off)
        .unwrap_or(sidebar.len());
    let chip_body = &sidebar[chip..body_end];
    assert!(
        chip_body.contains("horizontal-stretch: 0"),
        "TabChip must not stretch — chips render at preferred width, which is \
         what update-tab-drop's pitch assumes"
    );
    let preferred = parse_px(chip_body, 0, "preferred-width");

    assert_eq!(
        pitch, preferred,
        "update-tab-drop pitch ({pitch}) must equal TabChip preferred-width \
         ({preferred}px) — the drop-gap line assumes fixed-pitch chips"
    );
}

/// Value of `key: <n>px;` inside Theme (first match).
fn theme_px(theme: &str, key: &str) -> f64 {
    parse_px(theme, 0, key)
}

#[test]
fn terminal_row_pitch_comes_from_theme_tokens() {
    // Row height (44) + list gap (3) = drag pitch (47). The values live in
    // Theme only; sidebar.slint and app.slint must reference the tokens so the
    // three former restatements cannot drift apart.
    let theme = read_ui("theme.slint");
    let height = theme_px(&theme, "terminal-row-height");
    let gap = theme_px(&theme, "terminal-row-gap");
    assert_eq!((height, gap), (44.0, 3.0), "TerminalRow geometry changed");
    assert!(
        theme.contains("terminal-row-pitch: Theme.terminal-row-height + Theme.terminal-row-gap"),
        "pitch must be derived as height + gap"
    );

    let sidebar = read_ui("sidebar.slint");
    let row = sidebar
        .find("export component TerminalRow inherits")
        .expect("TerminalRow component");
    let row_end = sidebar[row + 1..]
        .find("export component")
        .map_or(sidebar.len(), |off| row + 1 + off);
    assert!(
        sidebar[row..row_end].contains("height: Theme.terminal-row-height;"),
        "TerminalRow height must come from Theme.terminal-row-height"
    );

    let app = read_ui("app.slint");
    assert_eq!(
        app.matches("spacing: Theme.terminal-row-gap;").count(),
        2,
        "TERMINALS and FILES columns must use Theme.terminal-row-gap"
    );
    assert!(
        app.contains("n * Theme.terminal-row-height + max(0, n - 1) * Theme.terminal-row-gap"),
        "sidebar-list-height must use the row tokens"
    );
    assert!(
        app.contains("y_px / (Theme.terminal-row-pitch / 1px)"),
        "update-term-drop must use Theme.terminal-row-pitch"
    );
    for literal in ["44px", "47.0", "n * 44"] {
        assert!(
            !app.contains(literal),
            "app.slint must not restate the TerminalRow pitch literal {literal}"
        );
    }
}

#[test]
fn menu_flyout_height_and_title_strip_come_from_theme() {
    // Menu row/separator heights and the flyout formula live in Theme; the
    // title strip and CSD resize grip are wired from Theme too.
    let theme = read_ui("theme.slint");
    assert_eq!(theme_px(&theme, "menu-item-height"), 32.0);
    assert_eq!(theme_px(&theme, "menu-separator-height"), 9.0);
    assert!(
        theme.contains("menu-flyout-height(items: int, seps: int)"),
        "Theme.menu-flyout-height(items, seps) must exist"
    );

    let menus = read_ui("chrome-menus.slint");
    assert!(
        menus.contains("Theme.menu-flyout-height(root.item-count, root.separator-count)"),
        "ChromeSubmenu panel-height must use Theme.menu-flyout-height"
    );
    for literal in ["8px + 32px *", "9px * root.separator-count"] {
        assert!(
            !menus.contains(literal),
            "chrome-menus.slint must not restate the flyout formula ({literal})"
        );
    }

    let bar = read_ui("title-bar.slint");
    assert!(
        bar.contains("Theme.menu-flyout-height(6, 0)")
            && bar.contains("Theme.menu-flyout-height(3, 0)"),
        "View menu cascade heights must use Theme.menu-flyout-height"
    );
    assert!(
        bar.contains("height: Theme.title-strip;"),
        "TitleBar height must come from Theme.title-strip"
    );
    assert!(
        !bar.contains("8px + 32px"),
        "title-bar.slint must not restate the flyout formula"
    );

    let app = read_ui("app.slint");
    assert!(
        app.contains("resize-border-width: root.maximized ? 0px : Theme.resize-border;"),
        "AppWindow resize grip must use Theme.resize-border"
    );
}
