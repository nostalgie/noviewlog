//! Source-level guard: chrome icon Text must not use tofu-prone Unicode glyphs.
//! A narrow ✎/×/✓-only grep missed TERMINALS/FILES ▾/▸ — this test bans the class.

use std::fs;
use std::path::PathBuf;

/// Icon-role codepoints that Windows / Noto Sans often render as empty boxes.
const BANNED_ICON_CHARS: &[char] = &[
    '✎', '×', '✕', '✓', '❐', '─', '▾', '▸', '›', '●', '↑', '↓', '↩', '＋', '－', '▶', '■',
];

fn ui_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ui")
}

fn slint_sources() -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(ui_dir()).expect("ui/") {
        let entry = entry.expect("dirent");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("slint") {
            continue;
        }
        let src =
            fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        out.push((path, src));
    }
    assert!(!out.is_empty(), "expected ui/*.slint files");
    out
}

/// Truncate a code line at the first `//` that sits outside a string literal
/// (issue #255): a trailing comment such as `text: "Save" // old "✕"` used to
/// leak its literals into the extraction and trip the guard falsely.
fn strip_trailing_comment(line: &str) -> &str {
    let mut in_string = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '/' if line[i..].starts_with("//") => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Collect every quoted string literal on lines that feed chrome text roles
/// (`text:` / `title:` / `label:`). Scanning the whole line (not just a
/// literal right after `text:`) keeps conditional icons such as
/// `text: flag ? "✕" : ""` inside the guard — a one-line refactor previously
/// reintroduced tofu with a green test.
fn text_literals(src: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for line in src.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") || trimmed.starts_with('*') {
            continue;
        }
        if !["text:", "title:", "label:"]
            .iter()
            .any(|k| line.contains(k))
        {
            continue;
        }
        // Drop trailing `//` comments first so literals quoted inside them
        // (`// old "✕"`) cannot trip the guard (#255). Multi-line `text:`
        // values remain a documented limitation.
        let mut rest = strip_trailing_comment(line);
        while let Some(start) = rest.find('"') {
            let after = &rest[start + 1..];
            match after.find('"') {
                Some(end) => {
                    out.push(&after[..end]);
                    rest = &after[end + 1..];
                }
                None => break, // unterminated quote (line continuation) — stop
            }
        }
    }
    out
}

#[test]
fn chrome_text_has_no_banned_icon_glyphs() {
    let mut hits = Vec::new();
    for (path, src) in slint_sources() {
        for lit in text_literals(&src) {
            for ch in lit.chars() {
                if BANNED_ICON_CHARS.contains(&ch) {
                    hits.push(format!(
                        "{}: text {:?} contains U+{:04X} '{}'",
                        path.display(),
                        lit,
                        ch as u32,
                        ch
                    ));
                }
            }
        }
    }
    assert!(
        hits.is_empty(),
        "chrome icon Text must use Path geometry, not Unicode glyphs:\n{}",
        hits.join("\n")
    );
}

#[test]
fn terminals_and_files_use_section_dot() {
    let app = ui_dir().join("app.slint");
    let src = fs::read_to_string(&app).unwrap();
    assert!(
        src.contains("SectionDot"),
        "TERMINALS/FILES markers must use SectionDot (colored disc), not Unicode"
    );
    let terminals = src
        .find("TERMINALS — collapsible")
        .expect("TERMINALS section");
    let files = src.find("FILES — collapsible").expect("FILES section");
    // Header + SectionDot + fill-color span >900 bytes (Unicode comments).
    let term_chunk = &src[terminals..terminals.saturating_add(1400).min(src.len())];
    let files_chunk = &src[files..files.saturating_add(1400).min(src.len())];
    assert!(
        term_chunk.contains("SectionDot") && term_chunk.contains("fill-color: Theme.accent"),
        "TERMINALS header must use SectionDot with Theme.accent"
    );
    assert!(
        files_chunk.contains("SectionDot") && files_chunk.contains("fill-color: Theme.include"),
        "FILES header must use SectionDot with Theme.include"
    );
    assert!(
        !src.contains("SectionChevron"),
        "SectionChevron was replaced by SectionDot"
    );
}

#[test]
fn sidebar_exports_section_dot() {
    let path = ui_dir().join("sidebar.slint");
    let src = fs::read_to_string(&path).unwrap();
    assert!(
        src.contains("export component SectionDot"),
        "SectionDot must stay exported from sidebar.slint"
    );
}

#[test]
fn terminal_row_status_dot_is_colored_per_section() {
    let app = fs::read_to_string(ui_dir().join("app.slint")).unwrap();
    assert!(
        app.contains("status-dot-color: term.running ? Theme.accent"),
        "TERMINALS rows wire status-dot-color to accent when running"
    );
    assert!(
        app.contains("status-dot-color: Theme.include"),
        "FILES rows wire status-dot-color to Theme.include"
    );
}

#[test]
fn shell_only_terminal_row_has_no_stop_button() {
    let app = fs::read_to_string(ui_dir().join("app.slint")).unwrap();
    assert!(
        app.contains("show-run-stop: term.has-launch"),
        "Play/Stop is only for Programs with a saved command, not a blank shell"
    );
    assert!(
        app.contains("show-edit-launch: true"),
        "blank TERMINALS rows still expose Edit Launch"
    );
}

#[test]
fn launch_preview_strip_is_theme_bar_not_accent() {
    let app = fs::read_to_string(ui_dir().join("app.slint")).unwrap();
    let idx = app
        .find("if root.show-launch-preview:")
        .expect("show-launch-preview strip");
    let chunk = &app[idx..idx.saturating_add(1600).min(app.len())];
    assert!(
        chunk.contains("background: Theme.bg-bar"),
        "launch preview must use Theme.bg-bar"
    );
    assert!(
        !chunk.contains("Theme.accent"),
        "launch preview must not use Theme.accent"
    );
    assert!(
        chunk.contains("overflow: elide"),
        "launch preview text must elide"
    );
}

#[test]
fn trailing_comment_literals_are_not_extracted() {
    // Issue #255: a trailing comment's quoted glyph must not trip the guard,
    // while `//` inside a real string literal must keep the literal intact.
    let code = r#"text: root.running ? "Stop" : "Run" // old "✕" glyph"#;
    assert_eq!(text_literals(code), vec!["Stop", "Run"]);

    let url = r#"text: root.hint // see "https://example.com/a//b" docs"#;
    assert_eq!(text_literals(url), Vec::<&str>::new());

    let literal_with_slashes = r#"text: "https://example.com""#;
    assert_eq!(
        text_literals(literal_with_slashes),
        vec!["https://example.com"]
    );
}
