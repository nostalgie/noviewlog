//! File-session wiring: open-log-file picker and reload.

use slint::{ComponentHandle, SharedString};

use noviewlog_core::Command;

use crate::ctx::Ctx;
use noviewlog_slint::ui::AppWindow;

pub(crate) fn install(ui: &AppWindow, ctx: &Ctx) {
    install_open_log_file(ui, ctx);
    install_reload(ui, ctx);
}

fn install_open_log_file(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    let ui_open = ui.as_weak();
    // Sync `pick_file()` stalled the whole Slint event loop (tick timer,
    // PTY ingest, paint) while the modal was open. The async picker runs
    // the dialog on rfd's own thread; the result is applied back here on
    // the UI thread via `spawn_local`.
    let dialog_open = std::rc::Rc::new(std::cell::Cell::new(false));
    ui.on_open_log_file(move || {
        if dialog_open.get() {
            return;
        }
        dialog_open.set(true);
        let ctx = ctx.clone();
        let ui_open = ui_open.clone();
        let dialog_open = dialog_open.clone();
        let _ = slint::spawn_local(async move {
            let picked = rfd::AsyncFileDialog::new()
                .set_title("Open log file")
                .add_filter("Log files", &["log", "txt", "out", "json", "jsonl"])
                .add_filter("All files", &["*"])
                .pick_file()
                .await;
            dialog_open.set(false);
            let Some(picked) = picked else {
                return;
            };
            let path_str = picked.path().to_string_lossy();
            if path_str.is_empty() {
                if let Some(ui) = ui_open.upgrade() {
                    ui.set_status_text(SharedString::from("open log: empty path"));
                }
                return;
            }
            if let Err(err) = ctx.send(Command::LoadFile {
                path: path_str.into_owned(),
            }) {
                if let Some(ui) = ui_open.upgrade() {
                    ui.set_status_text(SharedString::from(format!("open log: {err}")));
                }
                return;
            }
            ctx.refresh();
        });
    });
}

fn install_reload(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    ui.on_reload_file(move |id| {
        let terminal_id = if id.is_empty() {
            None
        } else {
            Some(id.as_str().to_string())
        };
        ctx.send_refresh(Command::ReloadFile { terminal_id });
    });
}
