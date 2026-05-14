//! GitHub release polling — Velopack-adjacent update notifier.
//!
//! Velopack handles the actual download / apply path in
//! `src/velopack_bridge.rs`. This module is the visible signal: poll
//! the GitHub release list every 3 hours, compare each entry against
//! the version baked in by `build.rs`, and push a notification entry
//! when a newer release exists. The user then either lets Velopack
//! stage + apply the update on next exit or upgrades manually.
//!
//! # Endpoint
//!
//! `https://api.github.com/repos/maui1911/CodeScope/releases?per_page=30`.
//! We poll the *list* endpoint, not `/releases/latest`. GitHub's
//! `/latest` only returns the most recent non-prerelease, and every
//! `0.3.0-rc.N` release is marked `prerelease: true`, so `/latest`
//! would either return nothing (no final release published yet) or
//! the legacy C# `v0.2.6` stable tag we want to *exclude*. The list
//! endpoint includes prereleases; we filter client-side on a version
//! floor (see [`MIN_PUBLISHED_TRIPLE`]) so the C# build's `v0.X.Y`
//! tags don't pollute our comparison. Unauthenticated requests get
//! 60/h per source IP — far above our once-every-3-hours cadence
//! even with multiple processes on the same network.
//!
//! # Comparison rule
//!
//! Prerelease-aware semver: `major.minor.patch[-rc.N]`. The triple is
//! compared numerically; on a tie, a final release (`None` prerelease)
//! outranks any rc, and within rcs the numeric rc index decides
//! (`rc.4 < rc.5`). Mirrors the relevant slice of semver 2.0 §11
//! without pulling the full identifier-comparison table in — the only
//! prerelease shape we ship is `rc.N`, anything else parses as "no
//! recognised prerelease" and is treated as a final release on the
//! published side (defensive: an unrecognised tag would have to lose
//! to no-prerelease to be considered older).
//!
//! Both sides go through [`strip_v_prefix`] which peels `v`/`V`. The
//! current-version slug from `build.rs` can also carry `git describe`
//! noise (`-N-gXXXX`, `-dirty`); the parser extracts the `rc.N`
//! segment if present and ignores the rest, so a developer build
//! three commits past `0.3.0-rc.5-dirty` still parses as `0.3.0-rc.5`
//! for "is the published release newer?" purposes.
//!
//! # Notification surface
//!
//! `app.rs` reads `UpdateStatus::Available` and pushes a `Generic`
//! notification titled `CodeScope <version> available` with a detail
//! line containing the release URL. The C# build uses a
//! `ToastSeverity.Ok` snackbar plus a confirmation dialog; we use the
//! existing Notification ring buffer because (a) the Rust port has no
//! restart-and-apply path, so the dialog wouldn't have anything
//! actionable to do, and (b) the bell button already collects "passive
//! event" entries, which is exactly what an "update available" hint
//! is.
//!
//! # Dev-mode behaviour
//!
//! `should_poll(paths)` returns `false` under `CODESCOPE_DEV=1` —
//! mirrors the C# `IsDevMode` early return in `UpdateService.CheckAsync`.
//! A dev build runs from `cargo run`, has no installer to upgrade, and
//! the `0.0-unknown` git-describe slug compares falsely-newer against
//! every published tag.
//!
//! # Failure semantics
//!
//! Two failure modes, two outcomes:
//!
//! * **Hard failure** — network error, non-2xx HTTP response, malformed
//!   JSON, oversized body. Collapses to `UpdateStatus::Unknown`. One
//!   `eprintln!` line at the failure site; caller retries on the next
//!   interval.
//! * **Ambiguous-but-recoverable** — running version is the
//!   `0.0-unknown` `build.rs` fallback, `Version::parse` can't make
//!   sense of the current slug, or the polled list contains no
//!   candidate at or above [`MIN_PUBLISHED_TRIPLE`]. Collapses to
//!   `UpdateStatus::UpToDate` because turning ambiguity into a
//!   notification storm is worse than the missed update — the next
//!   interval re-evaluates from scratch.
//!
//! In both cases we never panic, never propagate a `Result`, and never
//! re-toast the same version twice in a single process lifetime (the
//! caller dedupes by holding the last-announced version in `AppShell`
//! state).

use std::cmp::Ordering;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;

use crate::AppPaths;

/// Public GitHub releases list endpoint we poll. Bumped from
/// `/releases/latest` to the paged list (see module docs) so we see
/// prerelease tags too — the current cadence ships only rc.N tags,
/// all of which are marked `prerelease: true` on GitHub. `per_page=30`
/// is more than enough headroom for the rc cadence plus the
/// historical C# `v0.2.X` releases we filter out client-side.
pub const RELEASES_LIST_URL: &str =
    "https://api.github.com/repos/maui1911/CodeScope/releases?per_page=30";

/// Minimum published triple we consider a candidate for the running
/// build. Anything below this is a legacy release (the C# `v0.1` /
/// `v0.2.X` line, retired on 2026-05-14 — see ADR-0022 and
/// `docs/MIGRATION-csharp-to-rust.md`) and would compare false-newer
/// against a clean `0.3.0-rc.X` running build. Compared on triple only
/// so `v0.3.0-rc.N` (triple `(0, 3, 0)`) passes the floor.
pub const MIN_PUBLISHED_TRIPLE: (u64, u64, u64) = (0, 3, 0);

/// Initial delay before the first poll fires — keeps the network call
/// off the startup-critical path. Mirrors the 10 s delay in
/// `App.xaml.cs`.
pub const INITIAL_DELAY: Duration = Duration::from_secs(10);

/// Cadence between polls. Mirrors the 3 h interval in `App.xaml.cs`.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3 * 60 * 60);

/// HTTP timeout for a single poll. We're hitting `api.github.com`,
/// which is normally <1 s round-trip — anything longer than 30 s
/// almost always means a captive portal or DNS hijack we can't recover
/// from anyway, so we bail and try again on the next interval.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// User-agent header sent with the poll. GitHub's API rejects requests
/// without one (returns 403). Format follows common conventions —
/// `<product>/<version>` — using the slug the caller passes in (the
/// top-level binary bakes `CODESCOPE_VERSION_DISPLAY` via `build.rs`;
/// the `core` crate doesn't see that env var directly).
fn user_agent(current_version: &str) -> String {
    format!("CodeScope/{current_version}")
}

/// One entry from GitHub's `/releases` list endpoint (see
/// [`RELEASES_LIST_URL`]). We deserialise only the three fields we
/// surface; GitHub adds new fields regularly and we don't want a
/// future addition to break the parse.
#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseInfo {
    /// Git tag of the published release, e.g. `v0.3.0-rc.5` for a
    /// current release or `v0.2.6` for the retired C# `v0.2.X` line.
    pub tag_name: String,
    /// HTML URL the user clicks to view the release notes / download.
    pub html_url: String,
    /// Release notes body (Markdown). Optional because some early
    /// releases have an empty body and GitHub returns `null` rather
    /// than `""` in that case.
    #[serde(default)]
    pub body: Option<String>,
}

/// Result of one poll cycle.
///
/// `UpToDate` and `Unknown` look the same to the user (no
/// notification), but the caller may use them differently — e.g.
/// `Unknown` could trigger a faster retry. We currently treat them
/// identically and just keep the next-tick cadence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// A newer release is published. Caller should surface a
    /// notification with the supplied fields.
    Available {
        /// Version string with any `v`/`V` prefix stripped.
        version: String,
        /// HTML URL of the GitHub release page.
        url: String,
        /// Release notes (Markdown, possibly empty).
        body: String,
    },
    /// The latest published release is the same as or older than the
    /// running build.
    UpToDate,
    /// We couldn't determine the answer (network failure, parse
    /// failure, version comparison ambiguous). Caller logs and retries
    /// on the next tick.
    Unknown,
}

/// Should the background poll run for this process?
///
/// Skips dev builds (`CODESCOPE_DEV=1`) — mirrors the
/// `AppPaths.IsDevMode` early return in C# `UpdateService.CheckAsync`.
/// A dev build has no installer to upgrade, and the `0.0-unknown`
/// git-describe fallback would compare falsely-newer against every
/// published tag.
pub fn should_poll(paths: &AppPaths) -> bool {
    !paths.dev_mode
}

/// Run one poll cycle synchronously: fetch the GitHub release list,
/// pick the highest tag at or above [`MIN_PUBLISHED_TRIPLE`], compare
/// against `current_version`.
///
/// Outcomes follow the "Failure semantics" table at the top of this
/// module: hard failures (network, HTTP, JSON parse) return
/// `UpdateStatus::Unknown`; ambiguity cases (unparseable current
/// version, no in-floor candidate in the list) return
/// `UpdateStatus::UpToDate` to avoid a notification storm.
///
/// Designed to be called from `cx.background_spawn` — blocking work
/// that must not run on the UI thread.
pub fn check_once(current_version: &str) -> UpdateStatus {
    let body = match fetch_release_list_json(RELEASES_LIST_URL, current_version) {
        Ok(body) => body,
        Err(err) => {
            eprintln!("[update_check] fetch failed: {err}");
            return UpdateStatus::Unknown;
        }
    };
    evaluate(&body, current_version)
}

/// Pure logic: given the JSON body of a `/releases` (list) response
/// and the currently-running version slug, decide what to surface.
///
/// Split out from `check_once` so unit tests can exercise the full
/// filter + version-comparison logic without touching the network.
///
/// The body is parsed as a JSON *array* of `ReleaseInfo` entries.
/// Each tag parses as a [`Version`]; we keep only those at or above
/// [`MIN_PUBLISHED_TRIPLE`] (so the retired C# `v0.2.X` tags drop
/// out), take the maximum, and compare against the running build.
/// Mirrors the way `pr::parse_pr_url_json` is split from
/// `pr::detect_pr_url`.
pub fn evaluate(json_body: &str, current_version: &str) -> UpdateStatus {
    let releases: Vec<ReleaseInfo> = match serde_json::from_str(json_body) {
        Ok(r) => r,
        Err(err) => {
            eprintln!("[update_check] release list parse failed: {err}");
            return UpdateStatus::Unknown;
        }
    };
    // Reject the build.rs `0.0-unknown` fallback explicitly: a dev
    // build that couldn't resolve git would otherwise parse as
    // `(0, 0, 0)`, compare false-newer against every published tag,
    // and get spammed with notifications on every poll.
    if is_unknown_fallback(current_version) {
        return UpdateStatus::UpToDate;
    }
    let Some(current) = Version::parse(current_version) else {
        // Same defensive bail as the `is_unknown_fallback` branch:
        // if we can't even parse our own version we can't compare
        // safely, so pretend up-to-date until the next tick.
        return UpdateStatus::UpToDate;
    };

    // Find the highest published release at or above the floor. We
    // can't rely on the GitHub list being ordered by version (it's
    // ordered by `created_at` which is wall-clock; a back-published
    // older tag would jump ahead). Walk every candidate, compare,
    // keep the running max — at ~5 rc tags this is trivial.
    let mut best: Option<(Version, &ReleaseInfo)> = None;
    for release in releases.iter() {
        let tag = release.tag_name.trim();
        let Some(parsed) = Version::parse(tag) else { continue };
        if (parsed.major, parsed.minor, parsed.patch) < MIN_PUBLISHED_TRIPLE {
            // Legacy C# `v0.2.X` (and earlier) — out of update flow.
            continue;
        }
        best = match best {
            None => Some((parsed, release)),
            Some((cur, _)) if parsed > cur => Some((parsed, release)),
            Some(existing) => Some(existing),
        };
    }
    let Some((latest, release)) = best else {
        // No tags at or above the floor in the window we polled —
        // treat as up-to-date until one shows up (instead of looping
        // the notification on every tick).
        return UpdateStatus::UpToDate;
    };

    match latest.cmp(&current) {
        Ordering::Greater => UpdateStatus::Available {
            version: latest.to_display_string(),
            url: release.html_url.clone(),
            body: release.body.clone().unwrap_or_default(),
        },
        Ordering::Equal | Ordering::Less => UpdateStatus::UpToDate,
    }
}

/// Maximum response body we'll buffer from the release endpoint.
/// Real GitHub `/releases/latest` payloads are ~10 KiB; anything
/// larger almost certainly means we got an HTML error page or a
/// MITM is feeding us garbage. Refuse to read beyond this so a
/// hostile (or just broken) endpoint can't OOM the process.
const MAX_RESPONSE_BYTES: u64 = 1 << 20;

/// HTTP GET against GitHub's API. Returns the response body on 2xx,
/// `Err(message)` for any failure — including non-2xx, network error,
/// timeout, or response larger than `MAX_RESPONSE_BYTES`. Caller
/// treats every error as "skip this tick".
///
/// Private (not `pub(crate)`) because the only legitimate caller is
/// `check_once`; tests that need to exercise parse + version logic
/// go through `evaluate` with a fixture JSON string.
fn fetch_release_list_json(url: &str, current_version: &str) -> Result<String> {
    use std::io::Read;

    let response = ureq::get(url)
        .set("User-Agent", &user_agent(current_version))
        .set("Accept", "application/vnd.github+json")
        .timeout(REQUEST_TIMEOUT)
        .call()
        .context("request failed")?;
    if response.status() < 200 || response.status() >= 300 {
        return Err(anyhow!("HTTP {}", response.status()));
    }
    // Read at most MAX_RESPONSE_BYTES + 1 so we can tell the
    // difference between "exactly the cap" and "exceeded the cap".
    // The `+ 1` byte read is the canary: if the limited reader
    // delivered anything past the cap, we know the source had more
    // and we refuse to deserialise a truncated JSON document.
    let mut buf = Vec::with_capacity(16 * 1024);
    let mut limited = response.into_reader().take(MAX_RESPONSE_BYTES + 1);
    limited.read_to_end(&mut buf).context("read body failed")?;
    if buf.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(anyhow!("response exceeded {} bytes", MAX_RESPONSE_BYTES));
    }
    String::from_utf8(buf).context("response not utf-8")
}

/// Strip a leading `v` or `V` and trim whitespace from a version
/// string. Mirrors `build.rs::strip_v_prefix` so the same rule
/// applies on both sides of the comparison. Tags from GitHub arrive
/// as `v0.3.0-rc.5`; the embedded current-version slug (from
/// `build.rs`) already had the `v` stripped, but a manual config
/// might reintroduce it.
fn strip_v_prefix(s: &str) -> &str {
    let trimmed = s.trim();
    trimmed
        .strip_prefix('v')
        .or_else(|| trimmed.strip_prefix('V'))
        .unwrap_or(trimmed)
}

/// Recognise the `build.rs` last-resort fallback (`0.0-unknown`).
/// A dev build that can't resolve git gets stamped with this slug;
/// treating it as a real version would compare false-newer against
/// every published tag.
fn is_unknown_fallback(s: &str) -> bool {
    let s = strip_v_prefix(s);
    s == "0.0-unknown" || s.ends_with("-unknown")
}

/// Parsed semver-ish version with prerelease awareness for the only
/// shape the Rust port actually ships: `major.minor.patch[-rc.N]`.
///
/// Anything past the `rc.N` segment (`-dirty`, `-3-gHASH`, …) is
/// ignored — that's `git describe` noise on the current-version side
/// and is never emitted by the GitHub tag side. An unrecognised
/// prerelease shape (`-beta.1`, `-alpha`) parses with `rc = None`,
/// so it ranks alongside a final release. We don't ship those shapes
/// today; if we ever do, extend this enum rather than relying on the
/// fall-through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// `None` means "final release" (no prerelease segment). On a
    /// triple tie, `None` outranks `Some(_)` because semver 2.0
    /// gives final releases higher precedence than prereleases on
    /// the same triple.
    ///
    /// Field ordering inside this struct matters: `#[derive(Ord)]`
    /// produces a lexicographic compare across fields in declaration
    /// order, and `Option<u64>::None < Some(_)` by default — which
    /// is the *opposite* of what we want. The Ord impl below flips
    /// it so the derive order doesn't drive the result.
    rc: Option<u64>,
}

impl Version {
    /// Parse a tag or current-version slug into a [`Version`].
    /// Returns `None` for shapes we can't make sense of — caller
    /// treats those as "ambiguous, don't notify".
    fn parse(s: &str) -> Option<Self> {
        let stripped = strip_v_prefix(s);
        // Split into `triple` and optional `tail` at the first `-`.
        let (triple, tail) = match stripped.find('-') {
            Some(idx) => (&stripped[..idx], Some(&stripped[idx + 1..])),
            None => (stripped, None),
        };
        let (major, minor, patch) = parse_triple(triple)?;
        let rc = tail.and_then(parse_rc_segment);
        Some(Self { major, minor, patch, rc })
    }

    /// Render back to the canonical published form (`X.Y.Z` or
    /// `X.Y.Z-rc.N`). Used when surfacing `UpdateStatus::Available`
    /// so the notification shows the same shape as the GitHub tag.
    fn to_display_string(self) -> String {
        match self.rc {
            None => format!("{}.{}.{}", self.major, self.minor, self.patch),
            Some(n) => format!("{}.{}.{}-rc.{n}", self.major, self.minor, self.patch),
        }
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        // Triple first.
        let triple_cmp = (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch));
        if triple_cmp != Ordering::Equal {
            return triple_cmp;
        }
        // Same triple → final (None) outranks any rc (Some). Default
        // Option ordering would give Some > None, hence the manual
        // table.
        match (self.rc, other.rc) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Greater,
            (Some(_), None) => Ordering::Less,
            (Some(a), Some(b)) => a.cmp(&b),
        }
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> { Some(self.cmp(other)) }
}

fn parse_triple(s: &str) -> Option<(u64, u64, u64)> {
    let mut parts = s.split('.');
    let major: u64 = parts.next()?.parse().ok()?;
    let minor: u64 = parts.next().unwrap_or("0").parse().ok()?;
    let patch: u64 = parts.next().unwrap_or("0").parse().ok()?;
    // A 4th segment (`a.b.c.d`) is rejected — we don't ship 4-part
    // versions and accepting them would mask malformed tags.
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Pull an `rc.N` number out of a post-`-` tail. The tail may be:
///
/// * `rc.5` — exact tag with prerelease
/// * `rc.5-dirty` — same plus dirty
/// * `rc.5-3-gABC` — 3 commits past `rc.5`
/// * `rc.5-3-gABC-dirty` — both
/// * `dirty` / `3-gABC` / `dirty-3-gABC` — describe noise on a
///   final release (rc = None)
/// * anything else — no recognised prerelease (rc = None)
///
/// We split on `-` and search for the first token of the shape
/// `rc.<digits>`. Conservative on purpose: an unrecognised
/// prerelease shape (`beta.1`, `alpha`) falls through to `None` and
/// the caller treats the version as a final release, which is the
/// safer default for a comparison ("don't downgrade") than treating
/// it as some kind of rc.
fn parse_rc_segment(tail: &str) -> Option<u64> {
    for token in tail.split('-') {
        if let Some(num_str) = token.strip_prefix("rc.")
            && let Ok(n) = num_str.parse::<u64>() {
                return Some(n);
            }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── strip_v_prefix ──────────────────────────────────────────────────

    #[test]
    fn strip_v_prefix_handles_real_tag_shape() {
        assert_eq!(strip_v_prefix("v0.3.0-rc.5"), "0.3.0-rc.5");
        assert_eq!(strip_v_prefix("V0.3.0"), "0.3.0");
        assert_eq!(strip_v_prefix("v0.2.6"), "0.2.6");
        assert_eq!(strip_v_prefix("V0.2.6-dirty"), "0.2.6-dirty");
    }

    #[test]
    fn strip_v_prefix_preserves_internal_v() {
        // Don't accidentally double-strip a `v` later in the string.
        assert_eq!(strip_v_prefix("0.3.0-rc.5-v-suffix"), "0.3.0-rc.5-v-suffix");
    }

    #[test]
    fn strip_v_prefix_trims_whitespace() {
        assert_eq!(strip_v_prefix("  v0.3.0-rc.5  "), "0.3.0-rc.5");
    }

    // ── Version::parse ──────────────────────────────────────────────────

    #[test]
    fn parse_clean_triple_no_prerelease() {
        let v = Version::parse("0.3.0").unwrap();
        assert_eq!(v.major, 0);
        assert_eq!(v.minor, 3);
        assert_eq!(v.patch, 0);
        assert_eq!(v.rc, None);
    }

    #[test]
    fn parse_with_rc_prerelease() {
        let v = Version::parse("0.3.0-rc.5").unwrap();
        assert_eq!(v.rc, Some(5));
    }

    #[test]
    fn parse_strips_describe_noise_around_rc() {
        // `git describe` for a dev clone three commits past `rc.5`
        // with uncommitted changes still resolves to rc.5 — we treat
        // ahead-of-tag-but-dirty as the tag itself for "is published
        // version newer?" purposes, same as the pre-rc behaviour.
        assert_eq!(Version::parse("0.3.0-rc.5-dirty").unwrap().rc, Some(5));
        assert_eq!(Version::parse("0.3.0-rc.5-3-g1a2b3c4").unwrap().rc, Some(5));
        assert_eq!(
            Version::parse("0.3.0-rc.5-3-g1a2b3c4-dirty").unwrap().rc,
            Some(5)
        );
    }

    #[test]
    fn parse_strips_describe_noise_on_final_release() {
        // Same noise patterns on a final-release tag — no rc segment
        // present, so rc stays None.
        let v = Version::parse("0.3.0-dirty").unwrap();
        assert_eq!(v.rc, None);
        let v = Version::parse("0.3.0-3-g1a2b3c4").unwrap();
        assert_eq!(v.rc, None);
    }

    #[test]
    fn parse_strips_full_github_tag() {
        // End-to-end: real published tag flows through.
        let v = Version::parse("v0.3.0-rc.5").unwrap();
        assert_eq!((v.major, v.minor, v.patch, v.rc), (0, 3, 0, Some(5)));
    }

    #[test]
    fn parse_unrecognised_prerelease_falls_through_to_none() {
        // We don't ship beta/alpha today; the parser refuses to
        // guess so the caller treats them as final releases on the
        // *published* side (defensive: would otherwise rank a
        // future "v0.3.0-beta.1" as newer than a `0.3.0-rc.5` dev
        // build).
        let v = Version::parse("0.3.0-beta.1").unwrap();
        assert_eq!(v.rc, None);
        let v = Version::parse("0.3.0-alpha").unwrap();
        assert_eq!(v.rc, None);
    }

    #[test]
    fn parse_rejects_too_many_segments() {
        assert!(Version::parse("1.0.0.0").is_none());
    }

    #[test]
    fn parse_rejects_non_numeric() {
        assert!(Version::parse("abc.def.ghi").is_none());
        assert!(Version::parse("..").is_none());
    }

    #[test]
    fn parse_short_form_treats_missing_as_zero() {
        assert_eq!(Version::parse("0.2").unwrap().patch, 0);
        assert_eq!(Version::parse("1").unwrap().minor, 0);
        assert_eq!(Version::parse("1").unwrap().patch, 0);
    }

    // ── Version::cmp (prerelease-aware) ─────────────────────────────────

    fn v(s: &str) -> Version { Version::parse(s).expect("parse") }

    #[test]
    fn cmp_triple_drives_comparison_first() {
        assert!(v("0.2.6") < v("0.3.0"));
        assert!(v("0.3.0") > v("0.2.6"));
    }

    #[test]
    fn cmp_final_outranks_any_rc_on_same_triple() {
        // The bug that motivated this PR: `0.3.0-rc.5` must NOT be
        // equal to `0.3.0`. semver 2.0 §11 — final > any prerelease.
        assert!(v("0.3.0") > v("0.3.0-rc.5"));
        assert!(v("0.3.0-rc.5") < v("0.3.0"));
    }

    #[test]
    fn cmp_rc_index_decides_within_same_triple() {
        // The other bug: rc.4 → rc.5 should produce Greater so the
        // user actually sees a notification.
        assert!(v("0.3.0-rc.5") > v("0.3.0-rc.4"));
        assert!(v("0.3.0-rc.4") < v("0.3.0-rc.5"));
    }

    #[test]
    fn cmp_describe_noise_on_current_doesnt_change_rank() {
        // A dev build three commits ahead of rc.5 with dirty still
        // compares equal to a clean rc.5 published tag, so the
        // notification doesn't spam developers in their working
        // tree.
        assert_eq!(
            v("0.3.0-rc.5"),
            v("0.3.0-rc.5-3-g1a2b3c4-dirty")
        );
    }

    // ── evaluate (full pipeline, JSON list → UpdateStatus) ──────────────

    fn release_obj(tag: &str, url: &str, body: Option<&str>) -> String {
        let body_field = match body {
            Some(b) => format!("\"{}\"", b.replace('\"', "\\\"")),
            None => "null".to_string(),
        };
        format!("{{\"tag_name\":\"{tag}\",\"html_url\":\"{url}\",\"body\":{body_field}}}")
    }

    fn release_list(entries: &[String]) -> String {
        format!("[{}]", entries.join(","))
    }

    #[test]
    fn evaluate_available_when_higher_rc_published() {
        // The exact scenario from PR #205's diagnostic chase: running
        // rc.4, list contains rc.5 (newest), rc.4, rc.3, plus legacy
        // `v0.2.6` we must filter out via the floor.
        let json = release_list(&[
            release_obj("v0.3.0-rc.5", "https://example.invalid/rc5", Some("rc5 notes")),
            release_obj("v0.3.0-rc.4", "https://example.invalid/rc4", None),
            release_obj("v0.3.0-rc.3", "https://example.invalid/rc3", None),
            release_obj("v0.2.6",          "https://example.invalid/csharp", None),
        ]);
        match evaluate(&json, "0.3.0-rc.4") {
            UpdateStatus::Available { version, url, body } => {
                assert_eq!(version, "0.3.0-rc.5");
                assert_eq!(url, "https://example.invalid/rc5");
                assert_eq!(body, "rc5 notes");
            }
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_available_when_final_published_against_rc() {
        // `0.3.0` (final) outranks `0.3.0-rc.5` (running), so a stable
        // release after the rc cadence ends still notifies.
        let json = release_list(&[
            release_obj("v0.3.0", "https://example.invalid/final", None),
            release_obj("v0.3.0-rc.5", "https://example.invalid/rc5", None),
        ]);
        match evaluate(&json, "0.3.0-rc.5") {
            UpdateStatus::Available { version, .. } => assert_eq!(version, "0.3.0"),
            other => panic!("expected Available, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_uptodate_when_running_latest_rc() {
        let json = release_list(&[
            release_obj("v0.3.0-rc.5", "https://example.invalid", None),
            release_obj("v0.3.0-rc.4", "https://example.invalid", None),
        ]);
        assert_eq!(
            evaluate(&json, "0.3.0-rc.5"),
            UpdateStatus::UpToDate
        );
    }

    #[test]
    fn evaluate_uptodate_when_running_dirty_at_same_rc() {
        let json = release_list(&[
            release_obj("v0.3.0-rc.5", "https://example.invalid", None),
        ]);
        // Dev build three commits past rc.5 + dirty — same rc rank.
        assert_eq!(
            evaluate(&json, "0.3.0-rc.5-3-g1a2b3c4-dirty"),
            UpdateStatus::UpToDate
        );
    }

    #[test]
    fn evaluate_uptodate_when_running_ahead_of_release() {
        // Developer on rc.6 (unreleased) shouldn't get pinged about
        // the already-published rc.5.
        let json = release_list(&[
            release_obj("v0.3.0-rc.5", "https://example.invalid", None),
        ]);
        assert_eq!(
            evaluate(&json, "0.3.0-rc.6"),
            UpdateStatus::UpToDate
        );
    }

    #[test]
    fn evaluate_uptodate_when_only_below_floor_tags_in_list() {
        // GitHub returned only legacy `v0.2.X` (C#) tags — all below
        // MIN_PUBLISHED_TRIPLE. We filter them out and end up with no
        // candidate. Treat as UpToDate rather than spamming Unknown
        // forever.
        let json = release_list(&[
            release_obj("v0.2.6", "https://example.invalid", None),
            release_obj("v0.2.5", "https://example.invalid", None),
            release_obj("v0.1.0", "https://example.invalid", None),
        ]);
        assert_eq!(evaluate(&json, "0.3.0-rc.4"), UpdateStatus::UpToDate);
    }

    #[test]
    fn evaluate_floor_lets_rc_pass() {
        // `v0.3.0-rc.5` has triple `(0, 3, 0)` which equals the
        // floor; it must pass the filter.
        let json = release_list(&[
            release_obj("v0.3.0-rc.5", "https://example.invalid/rc5", None),
            release_obj("v0.2.6",      "https://example.invalid/legacy", None),
        ]);
        match evaluate(&json, "0.3.0-rc.4") {
            UpdateStatus::Available { version, .. } => assert_eq!(version, "0.3.0-rc.5"),
            other => panic!("expected Available rc.5, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_uptodate_when_current_is_unknown_fallback() {
        // The build.rs `0.0-unknown` fallback gets short-circuited
        // so it doesn't compare false-newer than every published tag.
        let json = release_list(&[
            release_obj("v0.3.0-rc.5", "https://example.invalid", None),
        ]);
        assert_eq!(evaluate(&json, "0.0-unknown"), UpdateStatus::UpToDate);
    }

    #[test]
    fn evaluate_picks_highest_not_first_in_list() {
        // GitHub's `/releases` is ordered by `created_at`, which the
        // user can manipulate (back-publishing). The evaluator MUST
        // walk every candidate and keep the highest version, not
        // trust list order. This test scrambles the order to prove it.
        let json = release_list(&[
            release_obj("v0.3.0-rc.3", "https://example.invalid/rc3", None),
            release_obj("v0.3.0-rc.5", "https://example.invalid/rc5", None),
            release_obj("v0.3.0-rc.4", "https://example.invalid/rc4", None),
        ]);
        match evaluate(&json, "0.3.0-rc.2") {
            UpdateStatus::Available { version, url, .. } => {
                assert_eq!(version, "0.3.0-rc.5");
                assert_eq!(url, "https://example.invalid/rc5");
            }
            other => panic!("expected Available rc.5, got {other:?}"),
        }
    }

    #[test]
    fn evaluate_unknown_on_malformed_json() {
        assert_eq!(evaluate("not json", "0.3.0-rc.4"), UpdateStatus::Unknown);
        assert_eq!(evaluate("", "0.3.0-rc.4"), UpdateStatus::Unknown);
    }

    #[test]
    fn evaluate_uptodate_on_empty_list() {
        assert_eq!(evaluate("[]", "0.3.0-rc.4"), UpdateStatus::UpToDate);
    }

    #[test]
    fn evaluate_handles_null_body() {
        let json = release_list(&[
            release_obj("v0.3.0-rc.5", "https://example.invalid", None),
        ]);
        match evaluate(&json, "0.3.0-rc.4") {
            UpdateStatus::Available { body, .. } => assert_eq!(body, ""),
            other => panic!("expected Available, got {other:?}"),
        }
    }

    // ── should_poll (dev-mode gate) ─────────────────────────────────────

    #[test]
    fn should_poll_skips_dev_mode() {
        let mut paths = AppPaths::detect();
        paths.dev_mode = true;
        assert!(!should_poll(&paths));
    }

    #[test]
    fn should_poll_allows_production() {
        let mut paths = AppPaths::detect();
        paths.dev_mode = false;
        assert!(should_poll(&paths));
    }
}
