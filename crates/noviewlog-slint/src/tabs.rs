//! Tab bar wiring: switch / move / add / close / restore / rename.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use noviewlog_core::Command;
use slint::{ComponentHandle, Model, SharedString, VecModel};

use crate::caret::arm_terminal_caret;
use crate::ctx::Ctx;
use crate::engine_bridge::bump_fast_timer;
use noviewlog_slint::ui::{AppWindow, TabInfo};

/// Active tab after closing `index`, computed against the post-removal row count.
/// Retained for tests: tab-close now relies on the stats flush instead of
/// precomputing the next active row (#198).
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn active_after_close(index: i32, old_active: i32, remaining: usize) -> i32 {
    let max = (remaining.saturating_sub(1)) as i32;
    if index < old_active {
        old_active - 1
    } else if index == old_active {
        index.min(max)
    } else {
        old_active
    }
}

pub(crate) fn install(
    ui: &AppWindow,
    ctx: &Ctx,
    tabs_model: Rc<VecModel<TabInfo>>,
    terminal_tab_active: Rc<Cell<bool>>,
    logical_size: Rc<RefCell<(f32, f32)>>,
    viewport_focused: Rc<Cell<bool>>,
) {
    install_switch(
        ui,
        ctx,
        terminal_tab_active.clone(),
        logical_size,
        viewport_focused,
    );
    install_move(ui, ctx);
    install_add(ui, ctx, tabs_model.clone(), terminal_tab_active.clone());
    install_close(ui, ctx);
    install_restore(ui, ctx, terminal_tab_active);
    install_rename(ui, ctx, tabs_model);
}

fn install_switch(
    ui: &AppWindow,
    ctx: &Ctx,
    terminal_tab_active: Rc<Cell<bool>>,
    logical_size: Rc<RefCell<(f32, f32)>>,
    viewport_focused: Rc<Cell<bool>>,
) {
    let engine = ctx.engine.clone();
    let force_render = ctx.force_render.clone();
    let timer = ctx.timer.clone();
    let timer_fast = ctx.timer_fast.clone();
    let ui_tabs = ui.as_weak();
    ui.on_tab_switch(move |index| {
        // Reject negative indices before the usize cast (mirrors on_tab_move).
        if index < 0 {
            return;
        }
        if let Some(ui) = ui_tabs.upgrade() {
            ui.set_active_tab_index(index);
            ui.set_filters_editable(index != 0);
        }
        terminal_tab_active.set(index == 0);
        // Send the switch with its own borrow: arm_terminal_caret below can
        // re-enter on_viewport_focused (focus-viewport → has-focus change)
        // when focus actually moves, and that handler borrows the same
        // engine (issues #232, #252).
        {
            let mut eng = engine.borrow_mut();
            let _ = eng.send_command(Command::TabSwitch {
                index: index as usize,
            });
            force_render.set(true);
        }
        bump_fast_timer(&timer, &timer_fast);
        if let Some(ui) = ui_tabs.upgrade() {
            if index == 0 {
                viewport_focused.set(true);
                // arm_terminal_caret takes the handle and invokes
                // focus-viewport before borrowing, so no RefMut is alive
                // across the synchronous re-entry (issues #232, #252).
                arm_terminal_caret(&ui, &engine, *logical_size.borrow());
            } else {
                ui.set_caret_visible(false);
            }
        }
    });
}

fn install_move(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    ui.on_tab_move(move |from_index, to_index| {
        if from_index < 0 || to_index < 0 {
            return;
        }
        ctx.send_refresh(Command::TabMove {
            from_index: from_index as usize,
            to_index: to_index as usize,
        });
    });
}

fn install_add(
    ui: &AppWindow,
    ctx: &Ctx,
    tabs_model: Rc<VecModel<TabInfo>>,
    terminal_tab_active: Rc<Cell<bool>>,
) {
    let ctx = ctx.clone();
    let ui_tabs = ui.as_weak();
    ui.on_tab_add(move || {
        let _ = ctx.send(Command::TabAdd);
        // Do not touch tabs_model optimistically: if the engine rejects or
        // reorders the add, the chip strip would show a phantom tab until
        // the next stats flush (same reasoning as terminals::install_add).
        // The chip appears with the immediate stats flush; naming stays
        // consistent via stats_sync ("Tab {index + 1}" default).
        let next_index = tabs_model.row_count() as i32;
        if let Some(ui) = ui_tabs.upgrade() {
            ui.set_active_tab_index(next_index);
            ui.set_filters_editable(true);
            ui.set_filter_draft(SharedString::default());
        }
        terminal_tab_active.set(false);
        ctx.refresh();
    });
}

fn install_close(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    ui.on_tab_close(move |index| {
        if index <= 0 {
            return;
        }
        let _ = ctx.send(Command::TabClose {
            index: index as usize,
        });
        // No optimistic model mutation (#198): if the engine refuses the close
        // (or a TabMove is still in flight) removing the chip here would show
        // a tab that still exists until the next flush. The immediate stats
        // flush updates chips, active index, can-restore and filter
        // editability from engine truth.
        ctx.refresh();
    });
}

fn install_restore(ui: &AppWindow, ctx: &Ctx, terminal_tab_active: Rc<Cell<bool>>) {
    let ctx = ctx.clone();
    let ui_tabs = ui.as_weak();
    ui.on_tab_restore(move || {
        let _ = ctx.send(Command::TabRestore);
        terminal_tab_active.set(false);
        if let Some(ui) = ui_tabs.upgrade() {
            ui.set_filters_editable(true);
        }
        ctx.refresh();
    });
}

fn install_rename(ui: &AppWindow, ctx: &Ctx, tabs_model: Rc<VecModel<TabInfo>>) {
    let ctx = ctx.clone();
    ui.on_tab_rename(move |index, name| {
        let name = name.trim();
        if name.is_empty() || index < 0 {
            return;
        }
        let _ = ctx.send(Command::TabRename {
            index: index as usize,
            name: name.to_string(),
        });
        let row = index as usize;
        if row < tabs_model.row_count() {
            if let Some(mut t) = tabs_model.row_data(row) {
                t.name = SharedString::from(name);
                tabs_model.set_row_data(row, t);
            }
        }
        ctx.refresh();
    });
}

#[cfg(test)]
mod tests {
    use super::active_after_close;

    #[test]
    fn closing_before_active_shifts_left() {
        assert_eq!(active_after_close(0, 2, 3), 1);
        assert_eq!(active_after_close(1, 2, 3), 1);
    }

    #[test]
    fn closing_active_clamps_to_last_row() {
        assert_eq!(active_after_close(2, 2, 2), 1);
        assert_eq!(active_after_close(0, 0, 1), 0);
    }

    #[test]
    fn closing_after_active_keeps_active() {
        assert_eq!(active_after_close(2, 0, 3), 0);
    }
}
