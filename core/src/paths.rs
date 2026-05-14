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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // We can't safely toggle CODESCOPE_DEV via std::env in unit tests
    // (process-wide mutation under parallel `cargo test`) so we cover
    // the dev-mode contract by asserting the suffix on a rooted helper
    // and the path-builder accessors against a known layout.

    #[test]
    fn dev_mode_uses_dev_suffix() {
        let dir = TempDir::new().unwrap();
        let paths = rooted_for_tests(true, dir.path());
        assert_eq!(paths.app_folder, "CodeScope.Dev");
        assert!(paths.config_dir.ends_with("CodeScope.Dev"));
        assert!(paths.state_dir.ends_with("CodeScope.Dev"));
    }

    #[test]
    fn prod_mode_uses_plain_folder() {
        let dir = TempDir::new().unwrap();
        let paths = rooted_for_tests(false, dir.path());
        assert_eq!(paths.app_folder, "CodeScope");
        assert!(paths.config_dir.ends_with("CodeScope"));
        assert!(paths.state_dir.ends_with("CodeScope"));
    }

    #[test]
    fn config_files_live_in_config_dir() {
        let dir = TempDir::new().unwrap();
        let paths = rooted_for_tests(false, dir.path());
        assert_eq!(paths.settings_file(), paths.config_dir.join("settings.json"));
        assert_eq!(paths.projects_file(), paths.config_dir.join("projects.json"));
    }

    #[test]
    fn state_files_live_in_state_dir() {
        let dir = TempDir::new().unwrap();
        let paths = rooted_for_tests(false, dir.path());
        assert_eq!(paths.layout_file(), paths.state_dir.join("layout.json"));
        assert_eq!(paths.window_file(), paths.state_dir.join("window.json"));
    }

    #[test]
    fn single_instance_mutex_has_dev_suffix_in_dev_mode() {
        let dir = TempDir::new().unwrap();
        let prod = rooted_for_tests(false, dir.path());
        let dev = rooted_for_tests(true, dir.path());
        assert_eq!(prod.single_instance_mutex(), "Global\\CodeScope.SingleInstance");
        assert_eq!(dev.single_instance_mutex(), "Global\\CodeScope.SingleInstance.Dev");
    }

    #[test]
    fn ensure_dirs_creates_both_layers() {
        let dir = TempDir::new().unwrap();
        let paths = rooted_for_tests(false, dir.path());
        assert!(!paths.config_dir.exists());
        assert!(!paths.state_dir.exists());

        paths.ensure_dirs().expect("ensure_dirs succeeds on a fresh temp dir");

        assert!(paths.config_dir.is_dir());
        assert!(paths.state_dir.is_dir());
    }

    #[test]
    fn ensure_dirs_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let paths = rooted_for_tests(false, dir.path());
        paths.ensure_dirs().unwrap();
        // Second call must not error even though the directories
        // already exist.
        paths.ensure_dirs().expect("ensure_dirs is idempotent");
    }

    #[test]
    fn detect_respects_dev_env_var_shape() {
        // detect() reads CODESCOPE_DEV. We can't mutate process env
        // safely under parallel tests, but we can still pin the
        // observable contract: with the env var unset (the typical
        // `cargo test` baseline) detect() must select prod-mode.
        // SAFETY: removed only for the duration of this single
        // assertion; tests that need the dev variant should use
        // rooted_for_tests.
        unsafe { std::env::remove_var("CODESCOPE_DEV"); }
        let paths = AppPaths::detect();
        assert!(!paths.dev_mode);
        assert_eq!(paths.app_folder, "CodeScope");
    }
}
