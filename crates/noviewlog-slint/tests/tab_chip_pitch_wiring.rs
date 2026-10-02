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
