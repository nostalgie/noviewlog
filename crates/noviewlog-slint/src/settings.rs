//! Settings wiring: max scrollback apply.

use noviewlog_core::core::types::clamp_max_scrollback_lines;
use noviewlog_core::Command;
use slint::ComponentHandle;

use crate::ctx::Ctx;
use noviewlog_slint::ui::AppWindow;

pub(crate) fn install(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    let ui_set = ui.as_weak();
    ui.on_settings_apply(move |value| {
        let raw = if value < 0 { 0usize } else { value as usize };
        let capped = clamp_max_scrollback_lines(raw);
        let _ = ctx.send(Command::SetSettings {
            max_scrollback_lines: capped,
        });
        if let Some(ui) = ui_set.upgrade() {
            ui.set_max_scrollback_lines(capped as i32);
        }
        ctx.refresh();
    });
}
