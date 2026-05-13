//! Velopack-rs apply path — Rust port mirror of the C# build's
//! `src/CodeScope.App/Updates/UpdateService.cs`.
//!
//! ## Why this lives in the binary, not in `codescope-core`
//!
//! `codescope-core` is the UI-free shared-models crate (see the
//! preamble in `core/src/lib.rs`). Velopack pulls in logging, IO, and
//! a `VelopackApp` entry-point that needs to run early in `main()` to
//! handle install / uninstall hooks. None of that fits the "pure
//! shared models" remit. The existing GitHub-release poll in
//! `core/src/update_check.rs` stays where it is — it has no Velopack
//! coupling and the unit-test surface there is the cheapest place to
//! exercise the version-comparison logic without spinning up a window.
//!
//! ## Two-tier strategy
//!
//! 1. **`VelopackApp::build().run()`** runs at the top of `main()`.
//!    Velopack's installer / uninstaller / first-run / restarted-after-update
//!    hooks dispatch here. For a build that wasn't installed via a
//!    Velopack bootstrapper (e.g. `cargo run`, a cargo-dist MSI install,
//!    or a manually-unpacked binary) this is a fast no-op — Velopack
//!    detects "no `Update.exe` sibling" and returns immediately.
//! 2. **`maybe_apply_now`** is called *automatically* on the
//!    background executor as soon as the GitHub-release poll
//!    reports a newer version, **gated on `is_velopack_install()`
//!    returning `true`**. It tries to construct an `UpdateManager`
//!    against the same GitHub releases endpoint the C# `vpk upload
//!    github` flow publishes to; on success, Velopack downloads the
//!    delta and `apply_updates_and_restart` exits the process so the
//!    bootstrap helper can relaunch us on the new version. If
//!    construction fails (binary isn't Velopack-installed) we report
//!    `Unsupported` — but the gate above means we should never
//!    actually reach this path in practice; the caller logs a
//!    diagnostic if it ever does. Mirrors the C# `UpdateService`'s
//!    "check + download + apply" auto-flow exactly.
//!
//! The GitHub-release polling loop in
//! `codescope_core::update_check::check_once` continues to run
//! unconditionally so users on non-Velopack installs (cargo-dist MSI,
//! `cargo run`, unpacked zip) still get the "update available"
//! notification — they just can't click "Apply" to auto-install.
//!
//! ## Where the C# methods map
//!
//! | C# (`UpdateService.cs`)                         | Rust (`velopack_bridge.rs`)                |
//! |--------------------------------------------------|--------------------------------------------|
//! | `new GithubSource(RepoUrl, …)`                   | `sources::GithubSource::new(REPO_URL, None, false)` |
//! | `mgr.IsInstalled`                                | `UpdateManager::new()` returning `Ok`       |
//! | `mgr.CheckForUpdatesAsync()`                     | `mgr.check_for_updates()`                   |
//! | `mgr.DownloadUpdatesAsync(info)`                 | `mgr.download_updates(&info, None)`         |
//! | `mgr.ApplyUpdatesAndRestart(info)`               | `mgr.apply_updates_and_restart(&info)`      |
//!
//! ## Channel
//!
//! Single-channel (`win`), matching the C# `Velopack.UpdateOptions
//! { ExplicitChannel = Channel }`. Multi-channel beta / alpha rings
//! are a documented follow-up.

use std::env;

/// GitHub repository URL — the same `RepoUrl` constant the C#
/// `UpdateService.cs` uses. Velopack's `GithubSource` calls the
/// GitHub releases REST API under this host and pulls the
/// `releases.<channel>.json` asset from each tagged release.
const REPO_URL: &str = "https://github.com/maui1911/CodeScope";

/// Default Velopack channel name for the *current* build target.
///
/// We use one channel per platform/arch so each build only ever pulls
/// updates compatible with its own runtime — a macOS arm64 client
/// asking for `releases.win.json` would (correctly) be told there's
/// nothing applicable, but the round-trip is wasted and the result
/// confusing in logs. Per-platform channels also match the asset
/// layout `vpk pack --channel <name>` produces in the release
/// pipeline (`rs--release.yml`):
///
/// - `x86_64-pc-windows-msvc`      → `win`
/// - `aarch64-apple-darwin`        → `osx-arm64`
/// - `x86_64-apple-darwin`         → `osx-x64`
/// - `x86_64-unknown-linux-gnu`    → `linux-x64`
///
/// The Windows variant keeps the historical `win` slug (not
/// `win-x64`) so already-installed v0.3 builds keep finding the same
/// `releases.win.json` after this change. Any future Windows arm64
/// build would add `win-arm64` without disturbing the existing one.
///
/// Anything outside this matrix falls back to `win` — harmless on a
/// build that hasn't been Velopack-installed (the apply gate
/// short-circuits before the channel is even consulted) and a safe
/// "no asset for this triple" failure path on a hypothetical
/// future target until the pipeline catches up.
pub(crate) const fn default_channel() -> &'static str {
    if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "win"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "osx-arm64"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "osx-x64"
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "linux-x64"
    } else {
        "win"
    }
}

/// Result of an attempted Velopack apply.
///
/// The caller usually gates this entire call site on
/// `is_velopack_install()` returning `true`, so `Unsupported` is
/// only reachable when the install state changed between the gate
/// and the call (or we've made a code-mismatch — the caller logs a
/// diagnostic). The fallback URL ("open the release page") lives on
/// the `UpdateStatus::Available` value from
/// `codescope_core::update_check`, not on this enum — keep the
/// Velopack outcome purely about the apply attempt and let the
/// caller decide which fallback to surface.
#[derive(Debug, Clone)]
pub enum ApplyOutcome {
    /// Velopack staged the update and called `apply_updates_and_restart`,
    /// which exits the process — we never return from a successful
    /// apply path. This variant only exists for completeness and is
    /// returned when the underlying `apply_updates_and_restart` call
    /// is somehow a no-op (e.g. nothing to apply).
    Applied,
    /// No update was available (the feed was empty, or the running
    /// version matches the latest).
    UpToDate,
    /// This binary wasn't installed via Velopack — caller should
    /// have gated on `is_velopack_install()` and surfaced the
    /// GitHub release URL (from
    /// `codescope_core::update_check::UpdateStatus::Available.url`)
    /// instead. Indicates a code-path mismatch when actually
    /// returned.
    Unsupported,
    /// Velopack tried but something failed (network, parse, file IO,
    /// …). Message is a single human-readable line suitable for the
    /// notification detail field.
    Failed(String),
}

/// Run the `VelopackApp` first-line entry-point hooks.
///
/// Must be called as the first non-trivial line in `main()` —
/// Velopack's install / uninstall / restarted-after-update events
/// will call `std::process::exit(...)` or `restart_with(...)` from
/// this point if they fire, so anything we do before this call
/// might execute twice (once in the bootstrap helper process, once
/// in the freshly-installed app process).
///
/// On a build that wasn't installed via a Velopack bootstrapper this
/// is a fast no-op — Velopack detects "no `Update.exe` sibling /
/// missing manifest" and returns without side effects. Safe to call
/// from `cargo run`, from a cargo-dist MSI install, or from an
/// unpacked zip; it just won't do anything.
pub fn run_startup_hooks() {
    velopack::VelopackApp::build().run();
}

/// Attempt to download + apply any pending update via Velopack.
///
/// Designed to be called from a background thread (it blocks on
/// network IO). Returns `ApplyOutcome` so the caller can decide how
/// to surface the result; on a successful `Applied` outcome the
/// process is replaced by the freshly-installed copy and we never
/// reach the return statement.
///
/// Mirrors C# `UpdateService.CheckAsync` — same gating, same
/// channel, same call sequence.
pub fn maybe_apply_now() -> ApplyOutcome {
    // Velopack-rs preserves the C# field names verbatim (`#[allow(non_snake_case)]`
    // on `UpdateOptions`) — that's why this looks PascalCase. Mirrors
    // C# `new UpdateOptions { ExplicitChannel = Channel }`.
    //
    // `channel_override()` returns `default_channel()` (per-platform)
    // unless `CODESCOPE_VELOPACK_CHANNEL` is set — QA can flip a
    // build's update feed without rebuilding by exporting the env
    // var.
    let options = velopack::UpdateOptions {
        ExplicitChannel: Some(channel_override()),
        ..Default::default()
    };
    // `GithubSource` directly mirrors C#
    // `new GithubSource(RepoUrl, accessToken: null, prerelease: false)`.
    // No token → public-API rate limit (60 req/hr per IP); same as
    // C# build's behaviour.
    let source = velopack::sources::GithubSource::new(REPO_URL, None, false);
    // Velopack-rs returns Err from `UpdateManager::new` when the
    // binary wasn't installed via a Velopack bootstrapper. That's
    // the documented "not installed" detection path; we treat it
    // as `Unsupported` rather than `Failed` so the caller knows to
    // open the release URL instead of toasting a scary error.
    let mgr = match velopack::UpdateManager::new(source, Some(options), None) {
        Ok(m) => m,
        Err(_) => return ApplyOutcome::Unsupported,
    };
    let check = match mgr.check_for_updates() {
        Ok(c) => c,
        Err(e) => return ApplyOutcome::Failed(format!("check failed: {e}")),
    };
    let info = match check {
        velopack::UpdateCheck::UpdateAvailable(i) => i,
        velopack::UpdateCheck::NoUpdateAvailable | velopack::UpdateCheck::RemoteIsEmpty => {
            return ApplyOutcome::UpToDate;
        }
    };
    if let Err(e) = mgr.download_updates(&info, None) {
        return ApplyOutcome::Failed(format!("download failed: {e}"));
    }
    // `apply_updates_and_restart` exits the process on success; if
    // it returns it means there was a soft error and we report
    // it. The C# build's equivalent is also expected to not return
    // (the call is documented as "restart the app").
    match mgr.apply_updates_and_restart(&info) {
        Ok(_) => ApplyOutcome::Applied,
        Err(e) => ApplyOutcome::Failed(format!("apply failed: {e}")),
    }
}

/// Whether Velopack's apply path is *available* — i.e. this binary
/// was installed via a Velopack bootstrapper. Cheap probe that just
/// tries to construct an `UpdateManager` against a `NoneSource` and
/// checks whether the locator step succeeds.
///
/// Used by the AppShell to decide whether the "update available"
/// notification should expose an "Apply" action (Velopack install)
/// or only an "Open release page" action (cargo-dist install /
/// `cargo run` / unpacked zip).
pub fn is_velopack_install() -> bool {
    // The Velopack manager's constructor itself is the detection
    // mechanism — a binary installed by a Velopack bootstrapper has
    // an `Update.exe` sibling + manifest that `UpdateManager::new`
    // looks for. We pass the cheapest source (`NoneSource`) because
    // the manager doesn't actually hit it during construction; it
    // only consults local install metadata.
    //
    // Caches the result behind a `OnceLock` so the periodic
    // notification check doesn't keep probing the filesystem.
    static CACHE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| {
        let source = velopack::sources::NoneSource {};
        velopack::UpdateManager::new(source, None, None).is_ok()
    })
}

/// Resolve the Velopack channel name for this run.
///
/// Defaults to [`default_channel`] (per-platform/-arch). The
/// `CODESCOPE_VELOPACK_CHANNEL` env var overrides — handy for QA
/// pointing a Windows build at a beta feed, or a macOS build at a
/// `nightly` channel, without recompiling. Empty / whitespace
/// values are ignored so a stale shell export doesn't shadow the
/// default with a feed that doesn't exist.
pub fn channel_override() -> String {
    match env::var("CODESCOPE_VELOPACK_CHANNEL") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default_channel().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialises every env-mutation test in this module. cargo runs
    /// `#[test]` functions in parallel by default and the env-mutation
    /// API (`set_var` / `remove_var`) is unsound across threads, so we
    /// hold this mutex for the entire body of each env test. Reused
    /// pattern from `std::env::set_var` rustdoc — same approach the
    /// stdlib's own internal tests take.
    ///
    /// Caught by Copilot on PR #162 (the original tests asserted
    /// "single-threaded" without enforcing it; cargo's parallel test
    /// runner would have flaked sooner or later).
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Snapshot + restore guard for `CODESCOPE_VELOPACK_CHANNEL`. The
    /// `Drop` impl puts the env back the way we found it even if the
    /// test panics — without it a panicking test would leak the
    /// override into whatever test runs next on the same process.
    struct EnvGuard {
        previous: Option<String>,
    }
    impl EnvGuard {
        fn capture() -> Self {
            Self {
                previous: std::env::var("CODESCOPE_VELOPACK_CHANNEL").ok(),
            }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: lock is held by the caller for the lifetime of
            // this guard (test holds `_lock` until end of scope).
            unsafe {
                match &self.previous {
                    Some(v) => std::env::set_var("CODESCOPE_VELOPACK_CHANNEL", v),
                    None => std::env::remove_var("CODESCOPE_VELOPACK_CHANNEL"),
                }
            }
        }
    }

    #[test]
    fn channel_override_defaults_to_platform_channel() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::capture();
        // SAFETY: env mutation serialised via `ENV_LOCK`; `EnvGuard`
        // restores the previous value on drop.
        unsafe {
            std::env::remove_var("CODESCOPE_VELOPACK_CHANNEL");
        }
        // Default tracks the build target's channel name — see
        // `default_channel` for the full mapping.
        assert_eq!(channel_override(), default_channel());
    }

    #[test]
    fn channel_override_ignores_blank_env_var() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::capture();
        // SAFETY: serialised + restored.
        unsafe {
            std::env::set_var("CODESCOPE_VELOPACK_CHANNEL", "   ");
        }
        // Blank value should fall back to the platform default
        // rather than shadowing the feed with an empty channel name.
        assert_eq!(channel_override(), default_channel());
    }

    #[test]
    fn default_channel_matches_current_target() {
        // Lock the platform → channel mapping. Adding a new target
        // belongs alongside an explicit case here so a future
        // regression (typo / wrong arch) trips the test.
        let expected = if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
            "win"
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            "osx-arm64"
        } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
            "osx-x64"
        } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            "linux-x64"
        } else {
            // Documented fallback — see `default_channel` for the
            // reasoning behind picking `win` here.
            "win"
        };
        assert_eq!(default_channel(), expected);
    }

    #[test]
    fn channel_override_honours_env_var() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _guard = EnvGuard::capture();
        // SAFETY: same as above — serialised + restored.
        unsafe {
            std::env::set_var("CODESCOPE_VELOPACK_CHANNEL", "beta");
        }
        assert_eq!(channel_override(), "beta");
    }

    #[test]
    fn apply_outcome_is_debug_and_clone() {
        // Future callers may want to log the outcome by value; make
        // sure the API stays compatible. This is a compile-only
        // check (any of these traits going away breaks the build).
        fn assert_debug_clone<T: std::fmt::Debug + Clone>() {}
        assert_debug_clone::<ApplyOutcome>();
    }

    #[test]
    fn is_velopack_install_is_false_under_cargo_run() {
        // The test harness binary is *not* installed via Velopack
        // (it's a plain `cargo test` build), so the probe must
        // return `false`. This is the public guarantee the AppShell
        // relies on to suppress the "Apply" action.
        assert!(!is_velopack_install());
    }
}
