//! `cargo run --bin codescope-rs` — launches the gpui app shell.
//!
//! Boot sequence:
//!
//! 1. Resolve env-aware paths (`CODESCOPE_DEV` redirect honoured).
//! 2. Load `settings.json` (or fall back to defaults; missing file is
//!    fine, malformed file logs a warning and uses defaults).
//! 3. Resolve the active theme by name from the built-in registry.
//! 4. Open one window with a transparent native titlebar so the tab
//!    strip can claim that vertical space.
//! 5. Hand control to [`crate::app::AppShell`], which spawns the
//!    initial terminal tab and owns everything from there.

mod app;
mod command_palette;
mod confirm_dialog;
mod new_project_dialog;
mod new_worktree_dialog;
mod notifications;
mod overview;
mod rename_dialog;
mod settings_dialog;
mod sidebar;
mod theme;
#[cfg(target_os = "windows")]
mod win32_titlebar;

use std::sync::Arc;

use anyhow::Result;
use codescope_core::{
    AppPaths, LayoutState, ProjectsConfig, SessionManager, Settings, Theme, WindowState, builtin,
    now_iso8601,
};
use gpui::{
    AppContext, Bounds, Context, IntoElement, ParentElement, Render, Styled, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, point, px, size,
};

use crate::app::AppShell;

struct Root {
    shell: gpui::Entity<AppShell>,
    theme: Arc<Theme>,
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(theme::canvas(&self.theme))
            .child(self.shell.clone())
    }
}

fn main() -> Result<()> {
    let paths = AppPaths::detect();
    if let Err(err) = paths.ensure_dirs() {
        eprintln!(
            "warning: could not create config dirs ({}): {}",
            paths.config_dir.display(),
            err
        );
    }

    // Install the crash-log panic hook before any gpui state spins up so a
    // panic during early boot still writes `%LOCALAPPDATA%\CodeScope\crash.log`
    // (parity with C# `App.LogFatal`). Dev mode redirects to `CodeScope.Dev`
    // automatically via `AppPaths`.
    codescope_core::crash_log::install_panic_hook(
        paths.clone(),
        env!("CODESCOPE_VERSION_DISPLAY").to_string(),
    );

    // Dev-only memory watchdog — surfaces working-set creep every 5 min
    // so per-session terminal scrollback regressions don't go unnoticed
    // during long dev runs (parity with the C# build's
    // `App/Diagnostics/MemoryWatchdog.cs`). No-op in production.
    codescope_core::memory_watchdog::start_if_dev(paths.dev_mode);

    // Create the process-wide job object before any pty spawns, so
    // every Claude / Codex / pwsh descendant is killed when CodeScope
    // exits — even on a hard crash. Mirrors C# `App.OnStartup`'s
    // `_appKiller = new ProcessTreeKiller(); _appKiller.Adopt(...)`.
    // No-op on non-Windows targets. A failure here would only mean
    // orphaned children are possible on crash; do not abort startup.
    if let Err(err) = codescope_terminal::process_group::ensure() {
        eprintln!("process_group: failed to initialise job object: {err:#}");
    }

    let settings = match Settings::load(&paths) {
        Ok(s) => s,
        Err(err) => {
            eprintln!(
                "warning: failed to load {} ({}); using defaults",
                paths.settings_file().display(),
                err
            );
            Settings::default()
        }
    };
    let theme = Arc::new(builtin::by_name(&settings.theme));
    let settings = Arc::new(settings);

    // Load projects via `SessionManager::load_with_sweep_persisting`
    // so the closed-session retention policy runs once per launch and
    // any pruned rows are written back to disk in the same step,
    // mirroring C# `SessionStore.LoadAsync`'s finally-block. Without
    // the persisting save, expired closed-history rows would linger
    // on disk forever even after the in-memory state had dropped them.
    let projects = match SessionManager::load_with_sweep_persisting(&paths, &now_iso8601()) {
        Ok((p, save_err)) => {
            if let Some(err) = save_err {
                eprintln!(
                    "warning: post-load retention sweep persisted state \
                     but failed to save: {err:#}"
                );
            }
            p
        }
        Err(err) => {
            eprintln!(
                "warning: failed to load {} ({}); starting with no projects",
                paths.projects_file().display(),
                err
            );
            ProjectsConfig::default()
        }
    };

    let layout = match LayoutState::load(&paths) {
        Ok(l) => l,
        Err(err) => {
            eprintln!(
                "warning: failed to load {} ({}); using default layout",
                paths.layout_file().display(),
                err
            );
            LayoutState::default()
        }
    };

    let saved_window = match WindowState::load(&paths) {
        Ok(state) => state,
        Err(err) => {
            eprintln!(
                "warning: failed to load {} ({}); window will open at default size",
                paths.window_file().display(),
                err
            );
            None
        }
    };

    let window_bounds = saved_window.map(window_state_to_bounds);
    let paths = Arc::new(paths);

    let app = gpui::Application::new();

    app.run(move |cx| {
        // `window.json` save lives inside `AppShell::new` (observes
        // bounds, debounces writes). `layout.json` save lives inside
        // `Sidebar::select` / `add_project` — selection changes are
        // user-driven and slow, no debounce needed.

        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    window_bounds,
                    titlebar: Some(TitlebarOptions {
                        title: Some("CodeScope".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let shell = cx.new(|cx| {
                        AppShell::new(
                            settings,
                            theme.clone(),
                            projects,
                            layout,
                            paths,
                            window,
                            cx,
                        )
                    });
                    cx.new(|_| Root { shell, theme })
                },
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });

    Ok(())
}

fn window_state_to_bounds(state: WindowState) -> WindowBounds {
    let bounds = Bounds {
        origin: point(px(state.x as f32), px(state.y as f32)),
        size: size(px(state.width as f32), px(state.height as f32)),
    };
    if state.maximised {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    }
}

