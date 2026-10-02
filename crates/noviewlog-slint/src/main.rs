//! NoViewLog Slint prototype: viewport, tabs, terminals sidebar.
//! This file is thin wiring only — callback bodies live in the modules below.

// GUI app: no console window behind the UI on Windows (ignored on other OSes).
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod app_state;
mod caret;
mod ctx;
mod engine_bridge;
mod files;
mod filters;
mod find;
mod input;
mod launch_args;
mod projects;
mod settings;
mod tabs;
mod terminals;
mod tick;
mod viewport;
mod window_chrome;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use noviewlog_core::core::types::DEFAULT_MAX_SCROLLBACK_LINES;
use noviewlog_core::{Engine, TERMINAL_TAB_NAME};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, VecModel};

use crate::app_state::ClickTracker;
use crate::ctx::Ctx;
use noviewlog_slint::ui::*;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Slint defaults to `with_transparent(true)` for FemtoVG WGPU. On Wayland that
    // yields a translucent swapchain: empty/alpha-0 regions show the launching
    // terminal until a full opaque paint. Force an opaque window surface.
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attrs| attrs.with_transparent(false))
        .select()?;

    let ui = AppWindow::new()?;

    let tabs_model = Rc::new(VecModel::<TabInfo>::from(vec![TabInfo {
        index: 0,
        name: SharedString::from(TERMINAL_TAB_NAME),
        is_terminal_tab: true,
    }]));
    ui.set_tabs_model(ModelRc::from(tabs_model.clone()));
    ui.set_active_tab_index(0);
    ui.set_can_restore_tab(false);

    let terminals_model = Rc::new(VecModel::<TerminalInfo>::from(vec![TerminalInfo {
        index: 0,
        id: SharedString::from(""),
        label: SharedString::from("."),
        cwd: SharedString::from(""),
        running: true,
        has_launch: false,
        launch_command: SharedString::from(""),
        launch_args: SharedString::from(""),
        launch_cwd: SharedString::from(""),
        launch_wsl: false,
        launch_wsl_distro: SharedString::from(""),
    }]));
    ui.set_terminals_model(ModelRc::from(terminals_model.clone()));
    let files_model = Rc::new(VecModel::<TerminalInfo>::from(vec![]));
    ui.set_files_model(ModelRc::from(files_model.clone()));
    let projects_model = Rc::new(VecModel::<ProjectInfo>::from(vec![]));
    ui.set_projects_model(ModelRc::from(projects_model.clone()));
    ui.set_active_project_id(SharedString::from(""));
    ui.set_active_project_name(SharedString::from(""));
    ui.set_active_terminal_index(0);
    ui.set_is_file_session(false);
    ui.set_host_is_windows(cfg!(windows));
    ui.set_terminals_section_expanded(true);
    ui.set_files_section_expanded(true);

    let filters_model = Rc::new(VecModel::<FilterInfo>::from(vec![]));
    ui.set_filters_model(ModelRc::from(filters_model.clone()));
    ui.set_filter_draft(SharedString::default());
    ui.set_filter_draft_regex(false);
    ui.set_filters_editable(false);
    ui.set_find_open(false);
    ui.set_find_query(SharedString::default());
    ui.set_find_case_sensitive(false);
    ui.set_find_whole_word(false);
    ui.set_find_regex(false);
    ui.set_find_status(SharedString::default());
    ui.set_find_error(SharedString::default());
    ui.set_max_scrollback_lines(DEFAULT_MAX_SCROLLBACK_LINES as i32);
    // Opaque placeholder before `ui.run()` — empty Image punches a see-through hole.
    viewport::seed_opaque_viewport(&ui);

    let cli: Vec<String> = std::env::args().skip(1).collect();
    let launch = launch_args::parse(&cli);
    let mut engine = Engine::new();
    let restored_project = engine.finish_startup(launch.clone());
    if launch.has_process_launch() {
        let label = launch
            .command
            .as_deref()
            .or(launch.log_file.as_deref())
            .unwrap_or("launch");
        ui.set_status_text(SharedString::from(format!("launch: {label}")));
    } else if !restored_project {
        ui.set_status_text(SharedString::from("interactive shell"));
    }

    let engine = Rc::new(RefCell::new(engine));
    let logical_size = Rc::new(RefCell::new((800.0f32, 600.0f32)));
    let force_render = Rc::new(Cell::new(true));
    let terminal_tab_active = Rc::new(Cell::new(true));
    // Engine starts unfocused; Window forward-focus may focus the viewport and fire the callback.
    let viewport_focused = Rc::new(Cell::new(false));
    let timer = Rc::new(Timer::default());
    let timer_fast = Rc::new(Cell::new(true));
    let find_debounce = Rc::new(Timer::default());
    let find_pending = Rc::new(RefCell::new(None::<(String, bool, bool, bool)>));
    let filter_draft_debounce = Rc::new(Timer::default());
    let filter_draft_pending = Rc::new(RefCell::new(None::<(String, bool)>));
    // When true, next stats push overwrites find query/toggles (open / tab switch).
    let find_resync = Rc::new(Cell::new(false));
    let find_stats_tab = Rc::new(Cell::new(-1i32));
    // Compositor occlusion (Wayland minimize) — Slint Window::is_minimized is unreliable here.
    let window_occluded = Rc::new(Cell::new(false));
    // Last tick saw an occluded window; used to force one paint on restore.
    let was_occluded = Rc::new(Cell::new(false));
    // At least one Engine::render bitmap has been uploaded this process.
    let viewport_presented = Rc::new(Cell::new(false));

    window_chrome::install(
        &ui,
        window_occluded.clone(),
        force_render.clone(),
        timer.clone(),
        timer_fast.clone(),
        viewport_presented.clone(),
    );

    {
        let ui_quit = ui.as_weak();
        ui.window().on_close_requested(move || {
            let Some(ui) = ui_quit.upgrade() else {
                return slint::CloseRequestResponse::HideWindow;
            };
            ui.invoke_open_quit_confirm();
            slint::CloseRequestResponse::KeepWindowShown
        });
    }

    let syncing_scroll = Rc::new(Cell::new(false));
    let syncing_follow = Rc::new(Cell::new(false));
    let has_selection = Rc::new(Cell::new(false));
    let pty_running = Rc::new(Cell::new(true));
    let selecting = Rc::new(Cell::new(false));
    let click_tracker = Rc::new(RefCell::new(ClickTracker::new()));
    ui.set_can_copy(false);
    ui.set_can_paste(false);
    // Engine tabs default wrap_lines: true — match app.slint / menu checkmarks.
    // Ongoing sync: apply_stats from typed StatsSnapshot.
    ui.set_wrap_lines(true);
    // Engine auto_follow ↔ Slint auto-follow / set-follow callback.
    ui.set_auto_follow(true);
    ui.set_can_close_tab(false);
    ui.set_can_restore_tab(false);
    // Startup active tab is the Terminal tab — Rename disabled until a filter tab is active.
    ui.set_can_rename_tab(false);
    // Explicit idle — empty renaming-terminal-id must not match placeholder term.id "".
    ui.set_renaming_tab_index(-1);
    ui.set_renaming_terminal_id(SharedString::default());
    ui.set_rename_draft(SharedString::default());
    let viewport_font_size = Rc::new(Cell::new(13.0_f32));

    let ctx = Ctx::new(
        engine.clone(),
        force_render.clone(),
        timer.clone(),
        timer_fast.clone(),
    );

    viewport::install_resized(
        &ui,
        logical_size.clone(),
        &ctx,
        window_occluded.clone(),
        viewport_presented.clone(),
    );
    viewport::install_focused(&ui, &ctx, viewport_focused.clone(), logical_size.clone());
    viewport::install_zoom(&ui, &ctx, viewport_font_size.clone());
    viewport::install_wrap(&ui, &ctx);
    viewport::install_follow(&ui, &ctx, syncing_follow.clone());
    viewport::install_scroll(&ui, &ctx, syncing_scroll.clone());
    input::install_pointer(
        &ui,
        &ctx,
        terminal_tab_active.clone(),
        has_selection.clone(),
        selecting.clone(),
        click_tracker.clone(),
    );
    input::install_context_menu(
        &ui,
        &ctx,
        terminal_tab_active.clone(),
        has_selection.clone(),
        pty_running.clone(),
    );
    input::install_copy_paste(&ui, &ctx, terminal_tab_active.clone());
    input::install_key_event(
        &ui,
        &ctx,
        terminal_tab_active.clone(),
        has_selection.clone(),
        viewport_font_size.clone(),
        find_resync.clone(),
    );
    tabs::install(
        &ui,
        &ctx,
        tabs_model.clone(),
        terminal_tab_active.clone(),
        logical_size.clone(),
        viewport_focused.clone(),
    );
    terminals::install(&ui, &ctx, terminals_model.clone(), files_model.clone());
    projects::install(&ui, &ctx);
    filters::install(
        &ui,
        &ctx,
        filter_draft_debounce.clone(),
        filter_draft_pending.clone(),
    );
    find::install(&ui, &ctx, find_debounce.clone(), find_pending.clone());
    files::install(&ui, &ctx);
    settings::install(&ui, &ctx);

    // Reused RGBA buffer across paints (recreate only on size change).
    let viewport_pixels: tick::ViewportPixels = Rc::new(RefCell::new(None));

    let controls = tick::install_host_tick(tick::TickDeps {
        ui: ui.as_weak(),
        engine: engine.clone(),
        force_render: force_render.clone(),
        timer: timer.clone(),
        timer_fast: timer_fast.clone(),
        logical_size: logical_size.clone(),
        tabs_model: tabs_model.clone(),
        terminals_model: terminals_model.clone(),
        files_model: files_model.clone(),
        projects_model: projects_model.clone(),
        filters_model: filters_model.clone(),
        terminal_tab_active: terminal_tab_active.clone(),
        window_occluded: window_occluded.clone(),
        was_occluded: was_occluded.clone(),
        viewport_presented: viewport_presented.clone(),
        syncing_scroll: syncing_scroll.clone(),
        syncing_follow: syncing_follow.clone(),
        has_selection: has_selection.clone(),
        pty_running: pty_running.clone(),
        viewport_font_size: viewport_font_size.clone(),
        find_resync: find_resync.clone(),
        find_stats_tab: find_stats_tab.clone(),
        find_pending: find_pending.clone(),
        find_debounce: find_debounce.clone(),
        pending_viewport_focus: ctx.pending_viewport_focus.clone(),
        viewport_pixels,
    });
    tick::install_pty_wake(&engine, &controls);
    tick::start_tick_timer(&timer);

    // Keep Rc<Timer> alive for the UI lifetime (Drop stops the timer).
    let _caret_blink_timer = caret::start_blink_timer(&ui);
    caret::install_boot_arm(
        &ui,
        &engine,
        logical_size.clone(),
        viewport_focused.clone(),
        &ctx,
    );

    let _tick_timer = timer;
    let _find_debounce = find_debounce;
    let _filter_draft_debounce = filter_draft_debounce;

    ui.run()?;
    Ok(())
}
