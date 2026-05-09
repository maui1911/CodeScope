//! Env-aware filesystem paths.
//!
//! Mirrors `NoScope.CodeScope.Core.AppPaths` in the C# build: a single
//! resolver that decides "is this dev or production?" once at startup,
//! and then every consumer asks `paths.config_file()` /
//! `paths.state_file()` instead of re-deriving paths.
//!
//! Dev mode (`CODESCOPE_DEV=1`) shifts every directory to a
//! `*.Dev`-suffixed sibling so a developer running `cargo run` doesn't
//! step on the v0.x C# build's `projects.json` or `layout.json`. The
//! Claude telemetry tail at `~/.claude/projects/…` is shared by design
//! — two FSWatchers, no state conflict.

use std::path::{Path, PathBuf};

/// One resolved set of paths for the running process. Built once at
/// startup; clone freely (`PathBuf` is cheap and we hold maybe two of
/// them).
#[derive(Debug, Clone)]
pub struct AppPaths {
    /// `true` if the process was launched with `CODESCOPE_DEV=1`.
    pub dev_mode: bool,
    /// The folder name we stamp into `%APPDATA%` / `%LOCALAPPDATA%`.
    /// Either `CodeScope` or `CodeScope.Dev`.
    pub app_folder: String,
    /// `%APPDATA%\<app_folder>` on Windows;
    /// `~/.config/<app_folder>` on Linux;
    /// `~/Library/Application Support/<app_folder>` on macOS.
    pub config_dir: PathBuf,
    /// `%LOCALAPPDATA%\<app_folder>` on Windows;
    /// `~/.local/state/<app_folder>` on Linux;
    /// `~/Library/Application Support/<app_folder>` on macOS (no
    /// separate `state` segment — Apple's HIG treats Application
    /// Support as the canonical home for both config and state, and
    /// nothing in our codebase currently distinguishes them on mac).
    pub state_dir: PathBuf,
}

impl AppPaths {
    /// Resolve once, against the current process env. Idempotent — call
    /// from `main()` and pass the result around.
    pub fn detect() -> Self {
        let dev_mode = std::env::var_os("CODESCOPE_DEV")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false);
        let app_folder = if dev_mode { "CodeScope.Dev" } else { "CodeScope" }.to_string();
        let config_dir = config_root().join(&app_folder);
        let state_dir = state_root().join(&app_folder);
        Self {
            dev_mode,
            app_folder,
            config_dir,
            state_dir,
        }
    }

    /// `%APPDATA%\CodeScope\settings.json` (and dev equivalent).
    pub fn settings_file(&self) -> PathBuf { self.config_dir.join("settings.json") }

    /// `%APPDATA%\CodeScope\projects.json` — populated later when we
    /// port the project model.
    pub fn projects_file(&self) -> PathBuf { self.config_dir.join("projects.json") }

    /// `%LOCALAPPDATA%\CodeScope\layout.json` — tab/sidebar state.
    pub fn layout_file(&self) -> PathBuf { self.state_dir.join("layout.json") }

    /// `%LOCALAPPDATA%\CodeScope\window.json` — last window pos/size.
    pub fn window_file(&self) -> PathBuf { self.state_dir.join("window.json") }

    /// Single-instance mutex name — `Global\CodeScope.SingleInstance`
    /// (and dev equivalent). Only meaningful on Windows.
    pub fn single_instance_mutex(&self) -> String {
        if self.dev_mode {
            "Global\\CodeScope.SingleInstance.Dev".to_string()
        } else {
            "Global\\CodeScope.SingleInstance".to_string()
        }
    }

    /// Make sure the config + state directories exist. Cheap;
    /// `create_dir_all` no-ops if they're already there.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.config_dir)?;
        std::fs::create_dir_all(&self.state_dir)?;
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn config_root() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_or_temp().join("AppData").join("Roaming"))
}

#[cfg(target_os = "windows")]
fn state_root() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_or_temp().join("AppData").join("Local"))
}

#[cfg(target_os = "macos")]
fn config_root() -> PathBuf {
    home_or_temp().join("Library").join("Application Support")
}

#[cfg(target_os = "macos")]
fn state_root() -> PathBuf {
    home_or_temp().join("Library").join("Application Support")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn config_root() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_or_temp().join(".config"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn state_root() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_or_temp().join(".local").join("state"))
}

fn home_or_temp() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Test helper: build an `AppPaths` rooted at an arbitrary directory.
/// Used by integration tests so they don't pollute the user's real
/// `%APPDATA%`.
#[doc(hidden)]
pub fn rooted_for_tests(dev_mode: bool, root: &Path) -> AppPaths {
    let app_folder = if dev_mode { "CodeScope.Dev" } else { "CodeScope" }.to_string();
    let config_dir = root.join("config").join(&app_folder);
    let state_dir = root.join("state").join(&app_folder);
    AppPaths {
        dev_mode,
        app_folder,
        config_dir,
        state_dir,
    }
}
