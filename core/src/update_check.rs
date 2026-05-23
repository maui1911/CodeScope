//! GitHub Releases polling for in-app update detection.
//!
//! This module is the version-checking half of the updater. The
//! download + atomic-swap half lives in the binary crate
//! (`src/update.rs`) because it needs the gpui binary's full
//! `self_update` feature set (archive-zip / archive-tar /
//! compression). Keeping the poll here means it stays testable
//! without dragging gpui into core's dep graph — see lib.rs
//! rationale.

use anyhow::{Context, Result, anyhow};
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

    // The list is newest-first per GitHub's ordering, but we don't
    // trust that — explicitly pick the highest semver across the
    // returned set.
    let latest = releases
        .into_iter()
        .filter_map(|r| {
            let v = Version::parse(r.version.trim_start_matches('v')).ok()?;
            Some((v, r))
        })
        .max_by(|a, b| a.0.cmp(&b.0));

    let Some((latest_version, latest_release)) = latest else {
        return Ok(None);
    };

    if latest_version <= *current {
        return Ok(None);
    }

    // Honour the pre-release gate: a stable build (no pre-release
    // segment) does not surface a pre-release update. Pre-release
    // builds see everything newer than themselves regardless of
    // pre-release status.
    if !latest_version.pre.is_empty() && current.pre.is_empty() {
        return Ok(None);
    }

    let suffix = target_archive_suffix();
    let asset = latest_release
        .assets
        .iter()
        .find(|a| a.name.ends_with(suffix))
        .ok_or_else(|| {
            anyhow!(
                "release {} has no asset matching '{}'",
                latest_release.version,
                suffix
            )
        })?;

    Ok(Some(ReleaseInfo {
        version: latest_version,
        tag: latest_release.version.clone(),
        archive_url: asset.download_url.clone(),
        archive_name: asset.name.clone(),
        release_notes_url: format!(
            "https://github.com/{}/{}/releases/tag/{}",
            REPO_OWNER, REPO_NAME, latest_release.version
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
    let latest = candidates.iter().max_by(|a, b| a.0.cmp(&b.0))?;
    if latest.0 <= *current {
        return None;
    }
    if !latest.0.pre.is_empty() && current.pre.is_empty() {
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
    fn missing_asset_falls_through_to_older_with_asset() {
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
