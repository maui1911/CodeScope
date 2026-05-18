// Tell the Windows linker to mark this exe as a GUI binary so a
// double-click (or the per-user MSI Start Menu shortcut) doesn't pop
// a leftover console window. Without this attribute the default
// `console` subsystem applies: Windows spawns a conhost.exe child,
// stdout goes there, and **closing that console window kills the app
// too** — exactly the regression the user just reported. Cargo
// builds for non-Windows targets ignore this attribute.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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
mod assets;
mod command_palette;
mod dialogs;
mod empty_state;
mod idle_notifier;
mod notifications;
mod overview;
mod sidebar;
mod taskbar_badge;
mod text_field;
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
    codescope_core::crash_log::install_panic_hook(
        paths.clone(),
        env!("CODESCOPE_VERSION_DISPLAY").to_string(),
    );

    // Per-phase boot tape — written line-by-line to `state_dir/boot.log`
    // with the file handle dropped between phases so a process kill
    // *after* the line is written still leaves the line on disk. Lets
    // us tell, on a no-crash-log crash, exactly which phase ate the
    // process. Cheap; <10 lines per launch.
    //
    // **Rotate, don't just truncate.** A user's natural reaction to a
    // crash is to relaunch, which would wipe the only forensic
    // artifact if we just truncated. Rename the current tape to
    // `boot.prev.log` (replacing any older prev) so the previous
    // launch survives one more run — long enough for the user to grab
    // it after a crash. Best-effort at every step; we never lose more
    // than the run-before-last.
    {
        let cur = paths.state_dir.join("boot.log");
        let prev = paths.state_dir.join("boot.prev.log");
        let _ = std::fs::rename(&cur, &prev);
        let _ = boot_log_options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&cur);
    }
    write_boot_phase(&paths, "main:enter");
    {
        let argv: Vec<String> = std::env::args().skip(1).collect();
        write_boot_phase(&paths, &format!("main:argv {argv:?}"));
    }

    // Resize-cascade diagnostic tape. PR #218/#219 fixed the
    // bounds-observer half of the cascade but the user-reported
    // "release-build resize doesn't repaint until tab-swap" symptom
    // still reproduces (and also on splitter drag, which never goes
    // through `observe_window_bounds`), so at least one checkpoint
    // along the canvas-layout → maybe_resize → apply_resize chain is
    // still racing. Wire the per-launch path here; the terminal
    // crate's `view.rs` taps it from each checkpoint. No-op until
    // this call sets the path.
    codescope_terminal::diag::set_log_path(paths.state_dir.join("terminal-resize.log"));

    write_boot_phase(&paths, "memory_watchdog:start_if_dev");
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
    write_boot_phase(&paths, "state_loaded:settings+projects+layout+window");
    let paths = Arc::new(paths);

    // `with_assets` registers our static SVG icon set so
    // `gpui::svg().path("icons/foo.svg")` resolves through
    // `crate::assets::AppAssets` to bytes embedded via
    // `include_bytes!`. Without an explicit `AssetSource`, gpui's
    // `SvgRenderer` silently no-ops every `svg()` element (the
    // default `()` source returns `None`). See `crate::assets`.
    let app = gpui::Application::new().with_assets(crate::assets::AppAssets);
    {
        let boot_paths = paths.as_ref();
        write_boot_phase(boot_paths, "gpui:application_built:about_to_run");
    }

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

/// Append a single timestamped phase marker to
/// `state_dir/boot.log`. Opens, writes, and closes the file per call
/// so a process kill *immediately after* the line was written still
/// leaves the line on disk — what we need to tell, on a no-crash-log
/// crash, which phase ate the process.
///
/// I/O errors at any layer are silently dropped — best-effort
/// diagnostic. `ensure_dirs` is called once from `main` and is
/// allowed to fail without aborting startup, so this helper has to
/// be robust against a missing `state_dir`: the `OpenOptions::open`
/// call returns `Err`, the `let _ =` discards it, and the function
/// returns without writing anything. A diagnostic mishap should
/// never take down boot.
fn write_boot_phase(paths: &AppPaths, phase: &str) {
    use std::io::Write as _;
    let line = format!("{} {}\n", now_iso8601(), phase);
    let path = paths.state_dir.join("boot.log");
    let _ = boot_log_options()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(line.as_bytes()));
}

/// Open-options preconfigured for the boot tape. On Unix we set
/// mode 0600 so `boot.log` (which records launch argv and per-phase
/// markers) is readable only by the owning user, not by anything
/// else on the box that happens to traverse the state dir. Windows
/// inherits the per-user ACL from `%LOCALAPPDATA%\CodeScope\` and
/// needs no extra flag.
fn boot_log_options() -> std::fs::OpenOptions {
    let opts = std::fs::OpenOptions::new();
    #[cfg(unix)]
    let opts = {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut o = opts;
        o.mode(0o600);
        o
    };
    opts
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

