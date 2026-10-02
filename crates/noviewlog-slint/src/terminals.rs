//! Terminals / FILES sidebar wiring: switch, move, rename, add, close,
//! start/stop, sidebar expansion, and Edit Launch submission.

use std::rc::Rc;

use noviewlog_core::Command;
use slint::{ComponentHandle, Model, SharedString, VecModel};

use crate::ctx::Ctx;
use crate::engine_bridge::{find_session_global_index, find_terminal_index};
use noviewlog_slint::ui::{AppWindow, TerminalInfo};

/// Split a whitespace-separated args line into argument strings.
pub(crate) fn parse_launch_args(args_text: &str) -> Vec<String> {
    args_text
        .split_whitespace()
        .map(|s| s.to_string())
        .collect()
}

/// Empty UI string field → `None` (engine default), else trimmed ownership.
fn non_empty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

pub(crate) fn install(
    ui: &AppWindow,
    ctx: &Ctx,
    terminals_model: Rc<VecModel<TerminalInfo>>,
    files_model: Rc<VecModel<TerminalInfo>>,
) {
    install_switch(ui, ctx, terminals_model.clone(), files_model.clone());
    install_move(ui, ctx);
    install_rename(ui, ctx, terminals_model.clone(), files_model.clone());
    install_add(ui, ctx, terminals_model.clone(), files_model.clone());
    install_close(ui, ctx);
    install_start_stop(ui, ctx);
    install_sidebar(ui, ctx);
    install_set_launch(ui, ctx);
}

fn install_switch(
    ui: &AppWindow,
    ctx: &Ctx,
    terminals_model: Rc<VecModel<TerminalInfo>>,
    files_model: Rc<VecModel<TerminalInfo>>,
) {
    let ctx = ctx.clone();
    let ui_term = ui.as_weak();
    ui.on_terminal_switch(move |id| {
        let id_str = id.as_str();
        if let Some(global) = find_session_global_index(&terminals_model, &files_model, id_str) {
            if let Some(ui) = ui_term.upgrade() {
                ui.set_active_terminal_index(global);
            }
        }
        ctx.send_refresh(Command::TerminalSwitch {
            terminal_id: id_str.to_string(),
        });
    });
}

fn install_move(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    ui.on_terminal_move(move |id, to_index| {
        if to_index < 0 {
            return;
        }
        ctx.send_refresh(Command::TerminalMove {
            terminal_id: id.as_str().to_string(),
            to_index: to_index as usize,
        });
    });
}

fn install_rename(
    ui: &AppWindow,
    ctx: &Ctx,
    terminals_model: Rc<VecModel<TerminalInfo>>,
    files_model: Rc<VecModel<TerminalInfo>>,
) {
    let ctx = ctx.clone();
    ui.on_terminal_rename(move |id, name| {
        let name = name.trim();
        if name.is_empty() || id.is_empty() {
            return;
        }
        let id_str = id.as_str();
        let _ = ctx.send(Command::TerminalRename {
            terminal_id: id_str.to_string(),
            name: name.to_string(),
        });
        for model in [&terminals_model, &files_model] {
            if let Some(row) = find_terminal_index(model, id_str).map(|i| i as usize) {
                if let Some(mut t) = model.row_data(row) {
                    t.label = SharedString::from(name);
                    model.set_row_data(row, t);
                }
                break;
            }
        }
        ctx.refresh();
    });
}

fn install_add(
    ui: &AppWindow,
    ctx: &Ctx,
    terminals_model: Rc<VecModel<TerminalInfo>>,
    files_model: Rc<VecModel<TerminalInfo>>,
) {
    let ctx = ctx.clone();
    let ui_term = ui.as_weak();
    ui.on_terminal_add(move || {
        let _ = ctx.send(Command::TerminalAdd);
        // Optimistic highlight; full row (with id) comes from immediate stats flush.
        // active-terminal-index is a GLOBAL session index across TERMINALS + FILES
        // (engine_bridge), so the new terminal's index is the combined row count —
        // using the TERMINALS count alone highlighted the wrong row with files open.
        let next = (terminals_model.row_count() + files_model.row_count()) as i32;
        if let Some(ui) = ui_term.upgrade() {
            ui.set_active_terminal_index(next);
        }
        ctx.refresh();
    });
}

fn install_close(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    ui.on_terminal_close(move |id| {
        let _ = ctx.send(Command::TerminalClose {
            terminal_id: Some(id.as_str().to_string()),
        });
        // Models refresh from stats (split TERMINALS / FILES lists).
        ctx.refresh();
    });
}

fn install_start_stop(ui: &AppWindow, ctx: &Ctx) {
    {
        let ctx = ctx.clone();
        ui.on_terminal_start(move |id| {
            ctx.send_refresh(Command::TerminalStart {
                terminal_id: Some(id.as_str().to_string()),
            });
        });
    }
    {
        let ctx = ctx.clone();
        ui.on_terminal_stop(move |id| {
            ctx.send_refresh(Command::Stop {
                terminal_id: Some(id.as_str().to_string()),
            });
        });
    }
}

fn install_sidebar(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    ui.on_set_sidebar_expanded(move |terminals, files| {
        let _ = ctx.send(Command::SetSidebarExpanded { terminals, files });
    });
}

fn install_set_launch(ui: &AppWindow, ctx: &Ctx) {
    let ctx = ctx.clone();
    ui.on_program_set_launch(
        move |terminal_id, command, args_text, cwd, wsl, wsl_distro| {
            let args = parse_launch_args(args_text.as_str());
            let cmd = command.as_str().trim();
            let cwd_s = cwd.as_str().trim();
            let distro = wsl_distro.as_str().trim();
            let _ = ctx.send(Command::ProgramSetLaunch {
                terminal_id: Some(terminal_id.as_str().to_string()),
                command: non_empty(cmd),
                args,
                cwd: non_empty(cwd_s),
                wsl,
                wsl_distro: non_empty(distro),
            });
            ctx.refresh();
        },
    );
}

#[cfg(test)]
mod tests {
    use super::{non_empty, parse_launch_args};

    #[test]
    fn args_split_on_whitespace() {
        assert_eq!(
            parse_launch_args("cargo run --profile release-dev"),
            vec!["cargo", "run", "--profile", "release-dev"]
        );
        assert_eq!(parse_launch_args("  "), Vec::<String>::new());
        assert_eq!(parse_launch_args(""), Vec::<String>::new());
    }

    #[test]
    fn empty_fields_map_to_none() {
        assert_eq!(non_empty(""), None);
        assert_eq!(non_empty("bash"), Some("bash".to_string()));
    }
}
