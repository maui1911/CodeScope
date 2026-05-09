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
mod sidebar;
mod theme;

use std::sync::Arc;

use anyhow::Result;
use codescope_core::{AppPaths, LayoutState, ProjectsConfig, Settings, Theme, WindowState, builtin};
use gpui::{
    AppContext, Bounds, Context, IntoElement, ParentElement, Pixels, Render, Styled,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, point, px, size,
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
    let projects = Arc::new(projects);

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
    let layout = Arc::new(layout);

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

    let app = gpui::Application::new();

    app.run(move |cx| {
        let settings = settings.clone();
        let theme = theme.clone();
        let projects = projects.clone();
        let layout = layout.clone();

        // No quit-time save yet. Writing `LayoutState::default()`
        // would clobber any layout the user (or a previous session)
        // saved, and we don't yet have live state on the AppShell
        // to write instead. `window.json` is in the same boat — we
        // restore on launch but don't observe bounds-changes to
        // write back. Both wires land together with
        // `cx.observe_window_bounds`.

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
                    let shell = cx.new(|cx| AppShell::new(settings, theme.clone(), projects, window, cx));
                    cx.new(|_| Root { shell, theme })
                },
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .detach();
        let _ = layout;
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

#[allow(dead_code)]
fn bounds_to_window_state(bounds: Bounds<Pixels>, maximised: bool) -> WindowState {
    let f32_x: f32 = bounds.origin.x.into();
    let f32_y: f32 = bounds.origin.y.into();
    let f32_w: f32 = bounds.size.width.into();
    let f32_h: f32 = bounds.size.height.into();
    WindowState {
        x: f32_x as i32,
        y: f32_y as i32,
        width: f32_w.max(0.0) as u32,
        height: f32_h.max(0.0) as u32,
        maximised,
    }
}

