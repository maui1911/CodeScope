//! `cargo run --bin window` — launches the gpui app shell.
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
mod new_worktree_dialog;
mod notifications;
mod sidebar;
mod theme;
#[cfg(target_os = "windows")]
mod win32_titlebar;

use std::sync::Arc;

use anyhow::Result;
use codescope_core::{AppPaths, LayoutState, ProjectsConfig, Settings, Theme, WindowState, builtin};
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

    let projects = match ProjectsConfig::load(&paths) {
        Ok(p) => p,
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

