//! Source-level wiring: the engine borrow must never span the synchronous
//! focus re-entry window (issues #232, #252).
//!
//! `arm_terminal_caret` invokes `focus-viewport`, which can synchronously fire
//! the `has-focus` change handler → `on_viewport_focused`, and that handler
//! borrows the same `Rc<RefCell<Engine>>`. A `RefMut` temporary in a caller's
//! argument expression lives until the end of the enclosing statement, so it
//! would still be held across the whole `arm_terminal_caret` call and panic
//! with `BorrowMutError`. The guard is structural, in two layers:
//!
//! 1. `arm_terminal_caret` takes `&Rc<RefCell<Engine>>` and invokes
//!    `focus-viewport` BEFORE its first `engine.borrow_mut()`.
//! 2. `on_viewport_focused` uses `try_borrow_mut`; on re-entry it defers the
//!    focus change into `pending_viewport_focus`, which the host tick applies
//!    and clears — so re-entry can never panic regardless of call shapes.

use std::fs;
use std::path::PathBuf;

fn read(rel: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Extract the body of `fn <name>` up to the next top-level `pub(crate) fn`
/// or end of file (best-effort source slicing for wiring assertions).
fn fn_body(src: &str, name: &str) -> String {
    let start = src
        .find(name)
        .unwrap_or_else(|| panic!("fn {name} not found"));
    let rest = &src[start..];
    let end = rest["fn ".len()..]
        .find("\npub(crate) fn")
        .map(|i| i + "fn ".len())
        .unwrap_or(rest.len());
    rest[..end].to_string()
}

#[test]
fn arm_terminal_caret_invokes_focus_before_first_engine_borrow() {
    let body = fn_body(&read("src/caret.rs"), "arm_terminal_caret");
    let invoke = body
        .find("ui.invoke_focus_viewport()")
        .expect("arm_terminal_caret must invoke focus-viewport");
    let borrow = body
        .find("engine.borrow_mut()")
        .expect("arm_terminal_caret must borrow the engine afterwards");
    assert!(
        invoke < borrow,
        "arm_terminal_caret must invoke focus-viewport BEFORE its first \
         engine.borrow_mut(): the invocation can synchronously re-enter \
         on_viewport_focused, which borrows the same engine (issues #232, #252)"
    );
    assert!(
        body.contains("engine: &Rc<RefCell<Engine>>"),
        "arm_terminal_caret must take the engine handle, not a borrowed \
         Engine, so callers cannot pass a temporary RefMut that outlives the \
         focus re-entry window (issues #232, #252)"
    );
}

#[test]
fn call_sites_pass_engine_handle_not_borrow_to_caret_arm() {
    for (file, anchor) in [
        ("src/tabs.rs", "on_tab_switch"),
        ("src/caret.rs", "Timer::single_shot"),
    ] {
        let src = read(file);
        let at = src
            .find(anchor)
            .unwrap_or_else(|| panic!("{anchor} in {file}"));
        let body = &src[at..];
        let arm = body
            .find("arm_terminal_caret(")
            .unwrap_or_else(|| panic!("arm_terminal_caret call after {anchor} in {file}"));
        assert!(
            body[arm..].contains("&engine, "),
            "{file}: arm_terminal_caret must receive the engine handle \
             (&engine), not a temporary `&mut engine.borrow_mut()` — a \
             temporary RefMut lives to the end of the statement and is held \
             across the focus re-entry (issues #232, #252)"
        );
        assert!(
            !body[arm..arm + 200].contains("borrow_mut()"),
            "{file}: no engine borrow may be opened inside the \
             arm_terminal_caret call (issues #232, #252)"
        );
    }
}

#[test]
fn on_viewport_focused_uses_try_borrow_mut_and_defers_on_reentry() {
    let body = fn_body(&read("src/viewport.rs"), "on_viewport_focused");
    assert!(
        body.contains("try_borrow_mut()"),
        "on_viewport_focused must use engine.try_borrow_mut(): a synchronous \
         re-entrant call while the engine is already borrowed must not panic \
         (issue #252)"
    );
    assert!(
        !body.contains("engine.borrow_mut()"),
        "on_viewport_focused must not take a bare engine.borrow_mut() — \
         re-entry during an already-live borrow would panic (issue #252)"
    );
    assert!(
        body.contains("pending_viewport_focus"),
        "on_viewport_focused must defer the focus change into \
         pending_viewport_focus when try_borrow_mut fails (issue #252)"
    );
}

#[test]
fn host_tick_applies_and_clears_deferred_viewport_focus() {
    let tick = read("src/tick.rs");
    assert!(
        tick.contains("pending_viewport_focus: Rc<Cell<Option<bool>>>"),
        "TickDeps must carry the deferred viewport-focus state (issue #252)"
    );
    let take = tick
        .find("pending_viewport_focus_tick.take()")
        .expect("host tick must consume the deferred focus");
    let body = &tick[take..];
    assert!(
        body.contains("apply_viewport_focus"),
        "host tick must apply the deferred focus via apply_viewport_focus \
         (issue #252)"
    );
    // The application must happen before the main engine borrow of the tick.
    let main_borrow = tick
        .find("let mut eng = engine_tick.borrow_mut();")
        .expect("main tick engine borrow");
    assert!(
        take < main_borrow,
        "deferred focus must be applied BEFORE the main engine_tick.borrow_mut() \
         of the tick body (issue #252)"
    );
}

#[test]
fn tab_switch_rejects_negative_index_before_cast() {
    let src = read("src/tabs.rs");
    let handler = src.find("on_tab_switch").expect("on_tab_switch handler");
    let body = &src[handler..];
    let guard = body.find("if index < 0").expect("negative index guard");
    let cast = body.find("index as usize").expect("index cast");
    assert!(
        guard < cast,
        "on_tab_switch must reject a negative index before `index as usize`, \
         mirroring on_tab_move (issue #252, low)"
    );
}
