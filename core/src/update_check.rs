//! GitHub Releases polling for in-app update detection.
//!
//! This module is the version-checking half of the updater. The
//! download + atomic-swap half lives in the binary crate
//! (`src/update.rs`) because it needs the gpui binary's full
//! `self_update` feature set (archive-zip / archive-tar /
//! compression). Keeping the poll here means it stays testable
//! without dragging gpui into core's dep graph — see lib.rs
//! rationale.

use anyhow::{Context, Result};
use semver::Version;

/// Repository in `owner/name` form. Public so the binary crate can
/// reference it when composing the "release notes" URL.
pub const REPO_OWNER: &str = "maui1911";
pub const REPO_NAME: &str = "CodeScope";

/// Metadata about a single GitHub Release that's newer than the
/// running binary. Carried from the poll to the toast surface and
/// then to the apply step. `archive_url` is the platform-specific
/// download (zip on Windows, tar.gz elsewhere); `release_notes_url`
/// is the human-readable page we open if the user clicks through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleaseInfo {
    pub version: Version,
    pub tag: String,
    pub archive_url: String,
    pub archive_name: String,
    pub release_notes_url: String,
}

/// Filename suffix of the archive we expect to find on a Release for
/// the current platform. `self_update` matches assets by substring,
/// so a release like `CodeScope-v0.3.1-windows.zip` is hit by the
/// `-windows.zip` suffix.
///
/// Unix uses `.tar.gz` (not `.tar.xz`) because `self_update` 0.41 has
/// no xz/lzma feature; cargo-dist was configured to emit gzip-compressed
/// tarballs in dist-workspace.toml's `unix-archive = ".tar.gz"`.
pub fn target_archive_suffix() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "-windows.zip"
    }
    #[cfg(target_os = "linux")]
    {
        "-x86_64-unknown-linux-gnu.tar.gz"
    }
    #[cfg(target_os = "macos")]
    {
        #[cfg(target_arch = "aarch64")]
        {
            "-aarch64-apple-darwin.tar.gz"
        }
        #[cfg(target_arch = "x86_64")]
        {
            "-x86_64-apple-darwin.tar.gz"
        }
    }
}

#[cfg(test)]
mod target_archive_suffix_tests {
    use super::*;

    #[test]
    fn returns_nonempty_suffix_for_host_platform() {
        let s = target_archive_suffix();
        assert!(!s.is_empty());
        assert!(s.starts_with('-'));
    }
}

/// Poll GitHub Releases and return `Some(ReleaseInfo)` when the
/// latest release's tag parses to a higher semver than the running
/// binary AND has an asset matching the current platform's archive
/// suffix. Returns `None` when up-to-date.
///
/// Sync (not async) — called from a dedicated background thread, not
/// the gpui executor. A 10-second timeout caps the worst case.
///
/// `current` is the running *binary's* version. The caller passes it
/// in because `env!("CARGO_PKG_VERSION")` here would resolve to
/// `codescope-core`'s package version (this is a library crate),
/// which is independent of the application's release version. The
/// binary crate evaluates `env!` in its own context and hands the
/// answer to us.
pub fn check_latest(current: &Version) -> Result<Option<ReleaseInfo>> {
    let releases = self_update::backends::github::ReleaseList::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .build()
        .context("configure github release list")?
        .fetch()
        .context("fetch github releases")?;

    let suffix = target_archive_suffix();

    // Parse every release tag to a semver, keeping the release struct
    // alongside it. Unparseable tags are dropped.
    let candidates: Vec<_> = releases
        .into_iter()
        .filter_map(|r| {
            let v = Version::parse(r.version.trim_start_matches('v')).ok()?;
            Some((v, r))
        })
        .collect();

    // Project to the (version, has-matching-asset) view that the pure
    // decision function consumes, then let `select_update_target` make
    // the call. Routing the production path through the same function
    // the unit tests exercise means the gating logic (pre-release
    // filtering, max-select, version gate, asset gate) has exactly one
    // implementation rather than a tested copy and a shipped copy.
    let decision_input: Vec<(Version, bool)> = candidates
        .iter()
        .map(|(v, r)| {
            let has_asset = r.assets.iter().any(|a| a.name.ends_with(suffix));
            (v.clone(), has_asset)
        })
        .collect();

    let Some(target_version) = select_update_target(&decision_input, current) else {
        return Ok(None);
    };
    let target_version = target_version.clone();

    // `select_update_target` only returns a version that is present in
    // the candidate list AND carries a matching asset, so both lookups
    // below are guaranteed to succeed.
    let (_, release) = candidates
        .iter()
        .find(|(v, _)| *v == target_version)
        .expect("select_update_target returned a version not in the candidate list");
    let asset = release
        .assets
        .iter()
        .find(|a| a.name.ends_with(suffix))
        .expect("select_update_target only returns versions with a matching asset");

    Ok(Some(ReleaseInfo {
        version: target_version,
        tag: release.version.clone(),
        archive_url: asset.download_url.clone(),
        archive_name: asset.name.clone(),
        release_notes_url: format!(
            "https://github.com/{}/{}/releases/tag/{}",
            REPO_OWNER, REPO_NAME, release.version
        ),
    }))
}

/// Pure version of `check_latest`'s decision logic — given a list of
/// (version, has_matching_asset) tuples and a `current` version,
/// decide which (if any) to surface. Extracted so the network-free
/// tests can exercise the gating without HTTP.
#[doc(hidden)]
pub fn select_update_target<'a>(
    candidates: &'a [(Version, bool)],
    current: &Version,
) -> Option<&'a Version> {
    // Stable clients never see pre-releases. The filter must run
    // BEFORE the max-select: otherwise a newer pre-release (e.g.
    // 0.4.0-rc.1) wins the max and then gets rejected by the gate,
    // shadowing a newer *stable* release (0.3.1) that the stable user
    // should have been offered. Pre-release clients keep everything.
    let want_prerelease = !current.pre.is_empty();
    let latest = candidates
        .iter()
        .filter(|(v, _)| want_prerelease || v.pre.is_empty())
        .max_by(|a, b| a.0.cmp(&b.0))?;
    if latest.0 <= *current {
        return None;
    }
    if !latest.1 {
        return None;
    }
    Some(&latest.0)
}

#[cfg(test)]
mod select_update_target_tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn newer_stable_is_surfaced() {
        let current = v("0.3.0");
        let cands = vec![(v("0.3.1"), true)];
        assert_eq!(select_update_target(&cands, &current), Some(&v("0.3.1")));
    }

    #[test]
    fn equal_returns_none() {
        let current = v("0.3.0");
        let cands = vec![(v("0.3.0"), true)];
        assert_eq!(select_update_target(&cands, &current), None);
    }

    #[test]
    fn older_returns_none() {
        let current = v("0.3.1");
        let cands = vec![(v("0.3.0"), true)];
        assert_eq!(select_update_target(&cands, &current), None);
    }

    #[test]
    fn picks_highest_when_multiple() {
        let current = v("0.3.0");
        let cands = vec![
            (v("0.3.1"), true),
            (v("0.4.0"), true),
            (v("0.3.5"), true),
        ];
        assert_eq!(select_update_target(&cands, &current), Some(&v("0.4.0")));
    }

    #[test]
    fn stable_user_skips_prerelease() {
        let current = v("0.3.0");
        let cands = vec![(v("0.3.1-rc.1"), true)];
        assert_eq!(select_update_target(&cands, &current), None);
    }

    #[test]
    fn stable_user_sees_stable_behind_newer_prerelease() {
        // Regression: a newer pre-release must not shadow a newer
        // stable release for a stable client. Highest semver here is
        // 0.4.0-rc.1, but the stable user on 0.3.0 should be offered
        // 0.3.1 — the pre-release is filtered before the max-select.
        let current = v("0.3.0");
        let cands = vec![(v("0.4.0-rc.1"), true), (v("0.3.1"), true)];
        assert_eq!(select_update_target(&cands, &current), Some(&v("0.3.1")));
    }

    #[test]
    fn prerelease_user_sees_prerelease() {
        let current = v("0.3.0-rc.1");
        let cands = vec![(v("0.3.0-rc.2"), true)];
        assert_eq!(select_update_target(&cands, &current), Some(&v("0.3.0-rc.2")));
    }

    #[test]
    fn prerelease_user_sees_stable() {
        let current = v("0.3.0-rc.5");
        let cands = vec![(v("0.3.0"), true)];
        assert_eq!(select_update_target(&cands, &current), Some(&v("0.3.0")));
    }

    #[test]
    fn missing_asset_returns_none() {
        let current = v("0.3.0");
        let cands = vec![(v("0.3.1"), false)];
        assert_eq!(select_update_target(&cands, &current), None);
    }

    #[test]
    fn missing_asset_on_latest_does_not_fall_back_to_older() {
        // If the newest tag has no platform asset (release in
        // progress), we explicitly do NOT fall back to an older tag
        // that does — the user sees nothing this poll, the next poll
        // will reconsider. This matches the "don't half-update"
        // posture from the velopack post-mortem.
        let current = v("0.3.0");
        let cands = vec![(v("0.3.1"), false), (v("0.3.2"), false)];
        assert_eq!(select_update_target(&cands, &current), None);
    }
}
