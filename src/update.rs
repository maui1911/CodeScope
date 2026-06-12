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
        /// Total expected bytes. `None` when the server doesn't
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

/// True when the user has an install in flight or awaiting restart —
/// states the background poll must never overwrite.
fn install_in_flight(status: &UpdateStatus) -> bool {
    matches!(
        status,
        UpdateStatus::Downloading { .. } | UpdateStatus::Installing | UpdateStatus::Ready(_)
    )
}

fn run_one_poll(state: &UpdateState) {
    if std::env::var("CODESCOPE_DEV_FAKE_UPDATE_TOAST").is_ok() {
        // Dev path. Still respect an in-flight install so a fake-toast
        // poll can't stomp a dev install test mid-flight.
        let mut guard = state.write();
        if !install_in_flight(&guard) {
            *guard = UpdateStatus::Available(fake_release_info());
        }
        return;
    }

    // Atomically: bail if an install is in flight, otherwise mark
    // Checking. Doing both under one write lock closes the race where
    // the user clicks Update between a separate read-guard and the
    // Checking write — the install thread's Downloading state would
    // otherwise be clobbered.
    {
        let mut guard = state.write();
        if install_in_flight(&guard) {
            return;
        }
        *guard = UpdateStatus::Checking;
    }

    let next = match update_check::check_latest(&current_version()) {
        Ok(Some(info)) => UpdateStatus::Available(info),
        Ok(None) => UpdateStatus::UpToDate,
        Err(err) => UpdateStatus::Failed {
            message: format!("Update check failed: {err:#}"),
        },
    };

    // Commit the result only if we're still Checking. A click during
    // the (blocking, possibly multi-second) check_latest call may have
    // transitioned us to Downloading/Installing; the check-and-set
    // under the write lock ensures we never overwrite that newer state.
    let mut guard = state.write();
    if matches!(*guard, UpdateStatus::Checking) {
        *guard = next;
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

/// Dev-only override for the download URL (`CODESCOPE_DEV_UPDATE_URL`),
/// used by the RELEASE-VALIDATION.md end-to-end loop to point the
/// installer at a locally-served archive. Compiled out of release
/// builds: honoring an arbitrary env-supplied URL in a shipped binary
/// would let anyone who can set the process environment redirect the
/// self-update to an attacker-controlled archive. Release builds always
/// download from the GitHub release asset URL.
#[cfg(all(not(target_os = "macos"), debug_assertions))]
fn dev_update_url_override() -> Option<String> {
    std::env::var("CODESCOPE_DEV_UPDATE_URL").ok()
}

#[cfg(all(not(target_os = "macos"), not(debug_assertions)))]
fn dev_update_url_override() -> Option<String> {
    None
}

/// How many times to (re)attempt the archive download before giving
/// up. Truncation by a middlebox is usually transient, so a couple of
/// fresh attempts clear it; 3 keeps the worst-case wait bounded.
#[cfg(not(target_os = "macos"))]
const DOWNLOAD_ATTEMPTS: u32 = 3;

/// Verdict on whether a finished download received the whole body.
/// Pure so the truncation rule is unit-testable without a socket. A
/// `None` total means the server sent no `Content-Length` and we
/// can't verify — we accept it rather than reject a legitimate
/// chunked response.
#[cfg(not(target_os = "macos"))]
fn verify_complete(received: u64, total: Option<u64>) -> Result<(), String> {
    match total {
        // Short read — the common, retryable case: a middlebox cutting
        // a large HTTPS download ends the body early with a clean EOF.
        Some(total) if received < total => Err(format!(
            "Download incomplete: received {received} of {total} bytes \
             (connection truncated — check a VPN/proxy or antivirus that \
             inspects HTTPS, then retry)"
        )),
        // Over-read — the server sent more than it advertised. Not a
        // truncation; the archive is suspect either way, so reject with
        // an honest, distinct message rather than the "incomplete" one.
        Some(total) if received > total => Err(format!(
            "Download size mismatch: received {received} bytes but the \
             server advertised {total} (unexpected — retry)"
        )),
        _ => Ok(()),
    }
}

/// Download `url` into `archive_path`, streaming byte-progress into
/// `state`, with completeness verification and a small retry budget.
/// Returns the user-facing failure message on the final failure.
#[cfg(not(target_os = "macos"))]
fn download_archive(
    client: &reqwest::blocking::Client,
    url: &str,
    archive_path: &std::path::Path,
    state: &UpdateState,
) -> Result<(), String> {
    let mut last_err = String::from("Download failed");
    for attempt in 1..=DOWNLOAD_ATTEMPTS {
        match download_once(client, url, archive_path, state) {
            Ok(()) => return Ok(()),
            Err(err) => {
                last_err = err;
                if attempt < DOWNLOAD_ATTEMPTS {
                    // Linear backoff; the file is recreated (truncated)
                    // on the next attempt by `download_once`.
                    std::thread::sleep(std::time::Duration::from_millis(
                        750 * attempt as u64,
                    ));
                }
            }
        }
    }
    Err(last_err)
}

/// One download attempt: (re)create the file, stream the body, verify
/// the full length arrived. Each call truncates `archive_path` so a
/// retry never appends onto a partial file.
#[cfg(not(target_os = "macos"))]
fn download_once(
    client: &reqwest::blocking::Client,
    url: &str,
    archive_path: &std::path::Path,
    state: &UpdateState,
) -> Result<(), String> {
    let mut archive_file = std::fs::File::create(archive_path).map_err(|err| {
        format!("Could not create archive file {}: {err:#}", archive_path.display())
    })?;

    let mut response = client
        .get(url)
        .send()
        .map_err(|err| format!("Download request failed: {err:#}"))?;
    response
        .error_for_status_ref()
        .map_err(|err| format!("Download failed: {err:#}"))?;

    let total = response.content_length();
    let mut received: u64 = 0;
    *state.write() = UpdateStatus::Downloading { received, total };

    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = match std::io::Read::read(&mut response, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(err) => return Err(format!("Download read error: {err:#}")),
        };
        std::io::Write::write_all(&mut archive_file, &buf[..n])
            .map_err(|err| format!("Write error: {err:#}"))?;
        received += n as u64;
        *state.write() = UpdateStatus::Downloading { received, total };
    }
    std::io::Write::flush(&mut archive_file)
        .map_err(|err| format!("Flush error: {err:#}"))?;
    drop(archive_file); // close before Extract reads it

    verify_complete(received, total)
}

#[cfg(not(target_os = "macos"))]
fn run_install(state: &UpdateState, info: ReleaseInfo) {
    // The first Downloading{received, total} state is written once the
    // HTTP response is in hand (with the real Content-Length below), so
    // there's no separate received=0/total=None pre-write here — it
    // would only ever be overwritten before a frame could render it.
    let download_url = dev_update_url_override().unwrap_or_else(|| info.archive_url.clone());

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

    // Stream the download ourselves so we can drive byte-progress into
    // the state slot. self_update::Download has no programmatic progress
    // hook in 0.41. reqwest::blocking is safe here — run_install is on a
    // plain std thread, not a tokio runtime.
    let client = match reqwest::blocking::Client::builder()
        .user_agent(concat!("CodeScope/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(c) => c,
        Err(err) => {
            *state.write() = UpdateStatus::Failed {
                message: format!("Could not build HTTP client: {err:#}"),
            };
            return;
        }
    };

    // Download with completeness verification + a few retries. A
    // TLS-inspecting proxy / middlebox cutting a large download short
    // ends the body with a clean EOF, so a naive read loop accepts a
    // truncated zip and the failure only surfaces later as a baffling
    // "Could not find EOCD" extract error (#270-follow-up). Verifying
    // received == Content-Length here turns that into an honest
    // "download incomplete", and retrying self-heals transient
    // truncation.
    if let Err(message) = download_archive(&client, &download_url, &archive_path, state) {
        *state.write() = UpdateStatus::Failed { message };
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

#[cfg(all(test, not(target_os = "macos")))]
mod tests {
    use super::verify_complete;

    #[test]
    fn full_download_is_complete() {
        assert!(verify_complete(5_717_213, Some(5_717_213)).is_ok());
    }

    #[test]
    fn truncated_download_is_rejected() {
        // A middlebox cutting the stream short: fewer bytes than the
        // advertised Content-Length must fail here, before extract.
        let err = verify_complete(1_000, Some(5_717_213)).unwrap_err();
        assert!(err.contains("received 1000 of 5717213"), "{err}");
        assert!(err.to_lowercase().contains("incomplete"), "{err}");
    }

    #[test]
    fn unknown_total_cannot_be_verified_so_accepts() {
        // No Content-Length header → we can't prove truncation, so we
        // don't reject a legitimate chunked response.
        assert!(verify_complete(1_000, None).is_ok());
    }

    #[test]
    fn over_read_is_rejected_with_a_distinct_message() {
        // More bytes than advertised must fail, but not as a
        // "truncated/incomplete" download — that wording would be wrong.
        let err = verify_complete(6_000_000, Some(5_717_213)).unwrap_err();
        assert!(err.contains("size mismatch"), "{err}");
        assert!(!err.to_lowercase().contains("incomplete"), "{err}");
    }
}
