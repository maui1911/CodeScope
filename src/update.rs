//! In-app updater — the binary-crate half.
//!
//! `core/src/update_check.rs` owns the version model + the GitHub
//! poll. This module owns the runtime state, the background thread,
//! and the platform-specific apply step (atomic exe-swap on Win /
//! Linux, browser fallback on macOS).
//!
//! Threading: `UpdateState` is shared between the gpui render loop
//! and at most two background threads (one poller, one installer).
//! `RwLock` because reads are frequent (every frame snapshots the
//! state) and writes are rare (~1 per state transition).

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;

use codescope_core::update_check::{self, ReleaseInfo};
use semver::Version;

/// The running binary's semver, parsed from `CARGO_PKG_VERSION`. Lives
/// in the binary crate (not `codescope-core`) because `env!` expands
/// in the crate being compiled — the core helper crate has its own
/// independent version. The expect panics only if the root manifest
/// holds a non-semver string, which would be a build-time bug.
fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("CARGO_PKG_VERSION must be valid semver")
}

/// State machine for the updater. Transitions are driven by the
/// poller (Idle / Checking ↔ Available / UpToDate) and the
/// installer (Available → Downloading → Installing → Ready, or any
/// → Failed).
#[derive(Clone, Debug)]
pub enum UpdateStatus {
    Idle,
    Checking,
    UpToDate,
    Available(ReleaseInfo),
    Downloading {
        /// Bytes received so far.
        received: u64,
        /// Total expected bytes. `None` when self_update doesn't
        /// report Content-Length.
        total: Option<u64>,
    },
    Installing,
    /// Atomic swap done; awaiting user-triggered restart.
    Ready(ReleaseInfo),
    Failed {
        message: String,
    },
}

/// Shared handle to the updater's current status. Wrap once in
/// `AppShell::new`; clone the `Arc` into background threads.
pub type UpdateState = Arc<RwLock<UpdateStatus>>;

pub fn new_state() -> UpdateState {
    Arc::new(RwLock::new(UpdateStatus::Idle))
}

/// Snapshot the status. Cheap — clones the small enum (ReleaseInfo
/// is the heaviest payload, ~200 bytes).
pub fn snapshot(state: &UpdateState) -> UpdateStatus {
    state.read().clone()
}

/// Spawn the polling thread. Polls once after a brief startup delay,
/// then every 3 hours. Updates the state slot on each check.
/// Returns immediately; the thread runs detached.
///
/// Dev override: `CODESCOPE_DEV_FAKE_UPDATE_TOAST=1` short-circuits
/// the poll and writes a synthetic `Available` status pointing at
/// version 99.0.0 with the current platform's archive suffix. Used
/// by RELEASE-VALIDATION.md to exercise the toast without staging a
/// real release.
pub fn start_poll(state: UpdateState) {
    std::thread::Builder::new()
        .name("update-poll".into())
        .spawn(move || {
            // Brief startup delay so the gpui main thread can finish
            // first-frame work without competing for network.
            std::thread::sleep(Duration::from_secs(15));
            loop {
                run_one_poll(&state);
                std::thread::sleep(Duration::from_secs(3 * 60 * 60));
            }
        })
        .expect("spawn update-poll thread");
}

fn run_one_poll(state: &UpdateState) {
    // Don't clobber an in-flight or completed install. Once the user
    // has started downloading (Downloading / Installing) or the swap
    // is done and awaiting restart (Ready), a background poll must not
    // reset the state machine back to Checking — that would wipe the
    // "Restart to activate" prompt and re-offer the same update. The
    // poll resumes normally after the user restarts into the new build.
    {
        let current = state.read();
        if matches!(
            *current,
            UpdateStatus::Downloading { .. }
                | UpdateStatus::Installing
                | UpdateStatus::Ready(_)
        ) {
            return;
        }
    }

    if std::env::var("CODESCOPE_DEV_FAKE_UPDATE_TOAST").is_ok() {
        *state.write() = UpdateStatus::Available(fake_release_info());
        return;
    }

    *state.write() = UpdateStatus::Checking;
    match update_check::check_latest(&current_version()) {
        Ok(Some(info)) => {
            *state.write() = UpdateStatus::Available(info);
        }
        Ok(None) => {
            *state.write() = UpdateStatus::UpToDate;
        }
        Err(err) => {
            *state.write() = UpdateStatus::Failed {
                message: format!("Update check failed: {err:#}"),
            };
        }
    }
}

fn fake_release_info() -> ReleaseInfo {
    ReleaseInfo {
        version: Version::parse("99.0.0").unwrap(),
        tag: "v99.0.0".into(),
        archive_url: "http://127.0.0.1:0/fake".into(),
        archive_name: format!(
            "CodeScope-v99.0.0{}",
            update_check::target_archive_suffix()
        ),
        release_notes_url: "https://github.com/maui1911/CodeScope/releases".into(),
    }
}

/// Spawn the install thread. Downloads the archive, atomic-swaps the
/// running binary, and transitions the state to `Ready` on success
/// or `Failed` on error.
///
/// Dev override: `CODESCOPE_DEV_UPDATE_URL=https://example/file.zip`
/// overrides the archive URL — point it at a hand-staged archive
/// served by a local HTTP server for end-to-end testing without
/// publishing a real release.
pub fn start_install(state: UpdateState, info: ReleaseInfo) {
    std::thread::Builder::new()
        .name("update-install".into())
        .spawn(move || {
            run_install(&state, info);
        })
        .expect("spawn update-install thread");
}

#[cfg(not(target_os = "macos"))]
fn run_install(state: &UpdateState, info: ReleaseInfo) {
    *state.write() = UpdateStatus::Downloading {
        received: 0,
        total: None,
    };

    let download_url = std::env::var("CODESCOPE_DEV_UPDATE_URL")
        .unwrap_or_else(|_| info.archive_url.clone());

    let temp_dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(err) => {
            *state.write() = UpdateStatus::Failed {
                message: format!("Could not create temp dir: {err:#}"),
            };
            return;
        }
    };

    let archive_path = temp_dir.path().join(&info.archive_name);

    let archive_file = match std::fs::File::create(&archive_path) {
        Ok(f) => f,
        Err(err) => {
            *state.write() = UpdateStatus::Failed {
                message: format!(
                    "Could not create archive file {}: {err:#}",
                    archive_path.display()
                ),
            };
            return;
        }
    };

    if let Err(err) = self_update::Download::from_url(&download_url)
        .show_progress(false)
        .download_to(&archive_file)
    {
        *state.write() = UpdateStatus::Failed {
            message: format!("Download failed: {err:#}"),
        };
        return;
    }

    *state.write() = UpdateStatus::Installing;

    // Extract the new binary from the archive, then self_replace it
    // over the running exe. self_replace::self_replace handles the
    // rename-then-write dance per-platform.
    let extracted = match extract_binary(&archive_path) {
        Ok(p) => p,
        Err(err) => {
            *state.write() = UpdateStatus::Failed {
                message: format!("Extract failed: {err:#}"),
            };
            return;
        }
    };

    if let Err(err) = self_update::self_replace::self_replace(&extracted) {
        *state.write() = UpdateStatus::Failed {
            message: format!("Self-replace failed: {err:#}"),
        };
        return;
    }

    *state.write() = UpdateStatus::Ready(info);
}

#[cfg(target_os = "macos")]
fn run_install(state: &UpdateState, _info: ReleaseInfo) {
    // macOS: no in-app apply path in v1. The toast action that would
    // normally land here is replaced upstream with an
    // OpenReleasesPage action — so this branch should never fire. We
    // surface a Failed state if it does, so the failure is visible
    // rather than silent.
    *state.write() = UpdateStatus::Failed {
        message: "In-app apply is not supported on macOS yet — \
                  open the releases page to download manually."
            .into(),
    };
}

#[cfg(not(target_os = "macos"))]
fn extract_binary(archive_path: &std::path::Path) -> anyhow::Result<std::path::PathBuf> {
    use anyhow::{Context, anyhow};

    let parent = archive_path
        .parent()
        .ok_or_else(|| anyhow!("archive path has no parent: {}", archive_path.display()))?;
    let extract_dir = parent.join("extracted");
    std::fs::create_dir_all(&extract_dir).context("create extract dir")?;

    self_update::Extract::from_source(archive_path)
        .extract_into(&extract_dir)
        .context("extract archive")?;

    // Candidate binary names, in preference order. The Windows zip is
    // staged by release.yml which renames the binary to `CodeScope.exe`,
    // but cargo-dist's Linux tar.gz packs the cargo bin name as-built —
    // `codescope` (lowercase). List both per platform so a change to
    // either packaging path doesn't silently break self-update.
    let exe_names: &[&str] = if cfg!(target_os = "windows") {
        &["CodeScope.exe", "codescope.exe"]
    } else {
        &["codescope", "CodeScope"]
    };

    // Look for the binary at the archive root, then one level down
    // (tar.gz may nest the binary inside a versioned folder by
    // cargo-dist convention; Windows zip is flat).
    for exe_name in exe_names {
        let direct = extract_dir.join(exe_name);
        if direct.exists() {
            return Ok(direct);
        }
    }
    for entry in std::fs::read_dir(&extract_dir)? {
        let entry = entry?;
        for exe_name in exe_names {
            let candidate = entry.path().join(exe_name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }
    Err(anyhow!(
        "could not find any of {:?} inside extracted archive at {}",
        exe_names,
        extract_dir.display()
    ))
}

/// Open the GitHub releases page in the default browser. macOS apply
/// fallback + general "view release notes" entry point.
pub fn open_releases_page() -> anyhow::Result<()> {
    let url = format!(
        "https://github.com/{}/{}/releases/latest",
        update_check::REPO_OWNER,
        update_check::REPO_NAME
    );
    open::that(&url).map_err(Into::into)
}
