//! Filters sidebar wiring: add / draft preview / toggle / remove / update /
//! clear, severity mode, and record expand/collapse actions.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use noviewlog_core::core::types::FilterType;
use noviewlog_core::Command;
use slint::{ComponentHandle, SharedString, Timer, TimerMode};

use crate::ctx::Ctx;
use noviewlog_slint::ui::AppWindow;

/// Debounce for FILTERS draft highlight preview.
const FILTER_DRAFT_DEBOUNCE: Duration = Duration::from_millis(150);

/// Filter dropdown index → engine filter type ("include" is the default).
pub(crate) fn parse_filter_type(filter_type: &str) -> FilterType {
    match filter_type {
        "exclude" => FilterType::Exclude,
        _ => FilterType::Include,
    }
}

pub(crate) fn install(
    ui: &AppWindow,
    ctx: &Ctx,
    filter_draft_debounce: Rc<Timer>,
    filter_draft_pending: Rc<RefCell<Option<(String, bool)>>>,
) {
    install_severity(ui, ctx);
    install_records_expand_collapse(ui, ctx);
    install_add(
        ui,
        ctx,
        filter_draft_debounce.clone(),
        filter_draft_pending.clone(),
    );
    install_draft_changed(ui, ctx, filter_draft_debounce, filter_draft_pending);
    install_toggle_remove_update_clear(ui, ctx);
}

fn install_severity(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    let ui_sev = ui.as_weak();
    ui.on_set_severity(move |mode| {
        let mode_str = mode.to_string();
        if let Some(ui) = ui_sev.upgrade() {
            ui.set_severity_mode(mode.clone());
        }
        ctx.send_refresh(Command::SeveritySet { mode: mode_str });
    });
}

fn install_records_expand_collapse(ui: &AppWindow, ctx: &Ctx) {
    {
        let ctx = ctx.clone();
        ui.on_records_expand_all(move || {
            ctx.send_refresh(Command::RecordsExpandAll);
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_records_collapse_all(move || {
            ctx.send_refresh(Command::RecordsCollapseAll);
        });
    }
}

fn install_add(
    ui: &AppWindow,
    ctx: &Ctx,
    filter_draft_debounce: Rc<Timer>,
    filter_draft_pending: Rc<RefCell<Option<(String, bool)>>>,
) {
    let ctx = ctx.clone();
    let ui_filt = ui.as_weak();
    ui.on_filter_add(move |filter_type, pattern, use_regex| {
        let pattern = pattern.trim();
        if pattern.is_empty() {
            return;
        }
        let _ = ctx.send(Command::FilterAdd {
            filter_type: parse_filter_type(filter_type.as_str()),
            pattern: pattern.to_string(),
            regex: use_regex,
        });
        // Clear draft preview immediately (UI also notifies filter-draft-changed).
        filter_draft_debounce.stop();
        *filter_draft_pending.borrow_mut() = None;
        let _ = ctx.send(Command::FilterDraftSet {
            pattern: String::new(),
            use_regex: false,
        });
        if let Some(ui) = ui_filt.upgrade() {
            ui.set_filter_draft(SharedString::default());
        }
        ctx.refresh();
    });
}

fn install_draft_changed(
    ui: &AppWindow,
    ctx: &Ctx,
    filter_draft_debounce: Rc<Timer>,
    filter_draft_pending: Rc<RefCell<Option<(String, bool)>>>,
) {
    let ctx = ctx.clone();
    ui.on_filter_draft_changed(move |pattern, use_regex| {
        *filter_draft_pending.borrow_mut() = Some((pattern.to_string(), use_regex));
        let ctx = ctx.clone();
        let filter_draft_pending = filter_draft_pending.clone();
        filter_draft_debounce.start(TimerMode::SingleShot, FILTER_DRAFT_DEBOUNCE, move || {
            let Some((pattern, use_regex)) = filter_draft_pending.borrow_mut().take() else {
                return;
            };
            let _ = ctx.send(Command::FilterDraftSet { pattern, use_regex });
            ctx.refresh();
        });
    });
}

fn install_toggle_remove_update_clear(ui: &AppWindow, ctx: &Ctx) {
    {
        let ctx = ctx.clone();
        ui.on_filter_toggle(move |id, enabled| {
            let id = id.as_str();
            if id.is_empty() {
                return;
            }
            ctx.send_refresh(Command::FilterToggle {
                id: id.to_string(),
                enabled,
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_filter_remove(move |id| {
            let id = id.as_str();
            if id.is_empty() {
                return;
            }
            ctx.send_refresh(Command::FilterRemove { id: id.to_string() });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_filter_update(move |id, pattern| {
            let id = id.as_str();
            let pattern = pattern.trim();
            if id.is_empty() || pattern.is_empty() {
                return;
            }
            ctx.send_refresh(Command::FilterUpdate {
                id: id.to_string(),
                pattern: pattern.to_string(),
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_filter_clear(move || {
            ctx.send_refresh(Command::FilterClear);
        });
    }
}

#[cfg(test)]
mod tests {
    use noviewlog_core::core::types::FilterType;

    use super::parse_filter_type;

    #[test]
    fn dropdown_maps_to_filter_type() {
        assert!(matches!(parse_filter_type("exclude"), FilterType::Exclude));
        assert!(matches!(parse_filter_type("include"), FilterType::Include));
        assert!(matches!(parse_filter_type(""), FilterType::Include));
    }
}
