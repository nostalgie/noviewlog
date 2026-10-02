//! Projects sidebar wiring: open / create / rename / delete.

use noviewlog_core::Command;

use crate::ctx::Ctx;
use noviewlog_slint::ui::AppWindow;

pub(crate) fn install(ui: &AppWindow, ctx: &Ctx) {
    {
        let ctx = ctx.clone();
        ui.on_project_open(move |id| {
            ctx.send_refresh(Command::ProjectOpen {
                project_id: id.as_str().to_string(),
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_project_create(move |name| {
            ctx.send_refresh(Command::ProjectCreate {
                name: name.as_str().to_string(),
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_project_rename(move |id, name| {
            ctx.send_refresh(Command::ProjectRename {
                project_id: id.as_str().to_string(),
                name: name.as_str().to_string(),
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_project_delete(move |id| {
            ctx.send_refresh(Command::ProjectDelete {
                project_id: id.as_str().to_string(),
            });
        });
    }
}
