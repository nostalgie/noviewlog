//! Find bar wiring: debounced query, goto next/prev, commit, and close.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use noviewlog_core::Command;
use slint::{ComponentHandle, Timer, TimerMode};

use crate::ctx::Ctx;
use noviewlog_slint::ui::AppWindow;

/// Debounce for find `search_set` (search bar cadence).
const FIND_DEBOUNCE: Duration = Duration::from_millis(150);

/// Pending find input captured while typing: (query, regex, case_sensitive, whole_word).
pub(crate) type FindPending = Rc<RefCell<Option<(String, bool, bool, bool)>>>;

pub(crate) fn install(
    ui: &AppWindow,
    ctx: &Ctx,
    find_debounce: Rc<Timer>,
    find_pending: FindPending,
) {
    install_query_changed(ui, ctx, find_debounce.clone(), find_pending.clone());
    install_goto(ui, ctx, find_debounce.clone(), find_pending.clone());
    install_commit(ui, ctx, find_debounce.clone(), find_pending.clone());
    install_closed(ui, ctx, find_debounce, find_pending);
}

fn install_query_changed(
    ui: &AppWindow,
    ctx: &Ctx,
    find_debounce: Rc<Timer>,
    find_pending: FindPending,
) {
    let ctx = ctx.clone();
    ui.on_find_query_changed(move |query, regex, case_sensitive, whole_word| {
        *find_pending.borrow_mut() = Some((query.to_string(), regex, case_sensitive, whole_word));
        let ctx = ctx.clone();
        let find_pending = find_pending.clone();
        find_debounce.start(TimerMode::SingleShot, FIND_DEBOUNCE, move || {
            let Some((q, re, cs, ww)) = find_pending.borrow_mut().take() else {
                return;
            };
            let _ = ctx.send(Command::SearchSet {
                query: q,
                regex: re,
                case_sensitive: cs,
                whole_word: ww,
            });
            ctx.refresh();
        });
    });
}

fn install_goto(ui: &AppWindow, ctx: &Ctx, find_debounce: Rc<Timer>, find_pending: FindPending) {
    let ctx = ctx.clone();
    ui.on_find_goto(move |delta| {
        // Flush pending search_set before navigating.
        find_debounce.stop();
        if let Some((q, re, cs, ww)) = find_pending.borrow_mut().take() {
            let _ = ctx.send(Command::SearchSet {
                query: q,
                regex: re,
                case_sensitive: cs,
                whole_word: ww,
            });
        }
        ctx.send_refresh(Command::SearchGoto { delta });
    });
}

fn install_commit(ui: &AppWindow, ctx: &Ctx, find_debounce: Rc<Timer>, find_pending: FindPending) {
    let ctx = ctx.clone();
    ui.on_find_commit(move || {
        find_debounce.stop();
        let Some((q, re, cs, ww)) = find_pending.borrow_mut().take() else {
            return;
        };
        let _ = ctx.send(Command::SearchSet {
            query: q,
            regex: re,
            case_sensitive: cs,
            whole_word: ww,
        });
        ctx.refresh();
    });
}

fn install_closed(ui: &AppWindow, ctx: &Ctx, find_debounce: Rc<Timer>, find_pending: FindPending) {
    let ctx = ctx.clone();
    let ui_closed = ui.as_weak();
    ui.on_find_closed(move || {
        // Drop in-flight typing so a late debounce cannot re-apply search.
        find_debounce.stop();
        find_pending.borrow_mut().take();
        let (regex, case_sensitive, whole_word) = ui_closed
            .upgrade()
            .map(|ui| {
                (
                    ui.get_find_regex(),
                    ui.get_find_case_sensitive(),
                    ui.get_find_whole_word(),
                )
            })
            .unwrap_or((false, false, false));
        let _ = ctx.send(Command::SearchSet {
            query: String::new(),
            regex,
            case_sensitive,
            whole_word,
        });
        ctx.refresh();
    });
}
