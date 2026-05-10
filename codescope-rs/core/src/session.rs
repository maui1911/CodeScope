//! Session lifecycle layer — Rust port of
//! `src/CodeScope.Core/Services/{SessionManager,SessionStore (session
//! mutators),SessionRetentionPolicy,SessionDescriptor}.cs`.
//!
//! **Foundation-only PR.** This module introduces the data shape and
//! the in-memory session-lifecycle primitives (open / soft-close /
//! reopen / hard-remove / rename / retention sweep) so a follow-up PR
//! can plumb the existing ad-hoc `Tab` layer through it without also
//! having to invent the data layer at the same time. Nothing in this
//! file is wired into the runtime yet — every public symbol is
//! intentionally `#[allow(dead_code)]` until a future PR consumes it.
//!
//! ## Where sessions live on disk
//!
//! In the C# build, sessions are persisted *inside* projects via
//! `projects.json` (under each `Project.Sessions`), not in a separate
//! `sessions.json`. The Rust port already mirrors this 1:1 — see
//! [`crate::projects::Session`] and [`crate::projects::ProjectsConfig`].
//! `SessionManager` therefore operates against a `&mut ProjectsConfig`
//! rather than introducing a second persistence root: a single source
//! of truth keeps round-trip with the C# binary intact and avoids the
//! data-loss footgun documented in `docs/HANDOFF.md` session 33.
//!
//! ## C# → Rust field mapping
//!
//! | C# (`Models/Session.cs`) | Rust (`projects::Session`) |
//! |---|---|
//! | `Id`              | `id`                |
//! | `WorktreePath`    | `worktree_path`     |
//! | `Branch`          | `branch`            |
//! | `AgentId`         | `agent_id`          |
//! | `DisplayName`     | `display_name`      |
//! | `WorktreeId`      | `worktree_id`       |
//! | `LastOpened`      | `last_opened` (ISO 8601 string) |
//! | `AgentSessionId`  | `agent_session_id`  |
//! | `ClosedAt`        | `closed_at` (ISO 8601 string) |
//!
//! ## Retention policy
//!
//! Mirrors `Services/SessionRetentionPolicy.cs`:
//!
//! * **TTL** — closed sessions older than [`RetentionPolicy::MAX_AGE_DAYS`]
//!   are dropped.
//! * **Cap** — for each `worktree_id` bucket, only the newest
//!   [`RetentionPolicy::MAX_PER_WORKTREE`] closed sessions survive.
//!
//! Live sessions (`closed_at = None`) are never pruned. Sweep is run
//! one-shot on load and after every soft-close, just like the C# build.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result, anyhow};

use crate::paths::AppPaths;
use crate::projects::{ProjectsConfig, Session};
use crate::time::parse_iso8601_secs;

pub use crate::time::now_iso8601;

/// Closed-session retention policy. Mirrors C#
/// `NoScope.CodeScope.Core.Services.SessionRetentionPolicy`.
pub struct RetentionPolicy;

impl RetentionPolicy {
    /// Hard cap on closed-session count per worktree. Oldest beyond
    /// this drop. Matches C# `MaxPerWorktree = 100`.
    pub const MAX_PER_WORKTREE: usize = 100;

    /// Closed sessions older than this are dropped on the next prune
    /// sweep. Matches C# `MaxAge = TimeSpan.FromDays(90)`.
    pub const MAX_AGE_DAYS: u32 = 90;
}

/// Pure-data parameters for spawning a session. Mirrors C#
/// `Services/SessionDescriptor.cs` 1:1: pure data, no terminal-control
/// dependency, so `core` stays UI-free.
///
/// Producers (the future Rust analogue of C# `SessionManager.Create*`)
/// build one of these, the UI layer consumes it to wire up the pty.
/// Not consumed yet on the Rust side — kept here so the agent-launch
/// port can land in a separate PR without re-litigating the shape.
#[allow(dead_code)] // Consumed by a follow-up PR (agent-launch port).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescriptor {
    pub id: String,
    pub working_directory: String,
    pub shell: String,
    pub shell_args: Vec<String>,
    pub title: String,
}

/// In-memory orchestration over the persisted `Session` rows in a
/// [`ProjectsConfig`]. Mirrors the session-lifecycle slice of C#
/// `SessionStore` (`AddSessionAsync` / `SoftCloseSessionAsync` /
/// `RestoreSessionAsync` / `RemoveSessionAsync` / `RenameSessionAsync`
/// / `UpdateAgentSessionIdAsync` plus the retention sweep).
///
/// Project / worktree mutations stay where they already are
/// ([`ProjectsConfig`] + the existing `Project::new` helper) — this
/// type is deliberately session-only so tabs can be plumbed through it
/// in a small follow-up PR without dragging in worktree lifecycle.
///
/// This layer is intentionally *not* event-driven: callers pass a
/// `&mut ProjectsConfig`, mutate, and persist via
/// [`ProjectsConfig::save`]. The C# `SessionStoreChange` event surface
/// will land alongside the future tab refactor (where it actually has
/// listeners). Keeping that out of foundation work avoids exposing an
/// API the runtime can't yet observe.
pub struct SessionManager;

impl SessionManager {
    // ---- load / save ------------------------------------------------

    /// Load `projects.json` and run the retention sweep once on the
    /// post-load snapshot. Mirrors the migration sweep in C#
    /// `SessionStore.LoadAsync`.
    ///
    /// `now_iso` is the current wall-clock time as an ISO-8601 UTC
    /// string. Tests inject a fixed value so retention is deterministic.
    /// In production callers pass the result of [`now_iso8601`].
    ///
    /// Missing files surface as an empty config, matching
    /// [`ProjectsConfig::load_from`] semantics — the legacy upgrade
    /// path the C# `LoadAsync` walks for first-launch users.
    pub fn load_with_sweep(paths: &AppPaths, now_iso: &str) -> Result<ProjectsConfig> {
        let mut cfg = ProjectsConfig::load(paths)?;
        Self::apply_retention(&mut cfg, now_iso, None);
        Ok(cfg)
    }

    /// Same as [`Self::load_with_sweep`] but reads from an explicit
    /// path. Used by tests to avoid hitting the user's real
    /// `%APPDATA%`.
    pub fn load_from_with_sweep(path: &Path, now_iso: &str) -> Result<ProjectsConfig> {
        let mut cfg = ProjectsConfig::load_from(path)?;
        Self::apply_retention(&mut cfg, now_iso, None);
        Ok(cfg)
    }

    // ---- session lifecycle -----------------------------------------

    /// Append a new session to a project. Stamps `last_opened = now`,
    /// matching C# `SessionStore.AddSessionAsync`. Returns an error
    /// (without mutating) if the project is unknown.
    pub fn open(
        cfg: &mut ProjectsConfig,
        project_id: &str,
        mut session: Session,
        now_iso: &str,
    ) -> Result<Session> {
        let project = cfg
            .projects
            .iter_mut()
            .find(|p| p.id == project_id)
            .ok_or_else(|| anyhow!("project '{project_id}' not found"))?;
        session.last_opened = Some(now_iso.to_string());
        // Cleared on (re)open even if the caller passed a closed-at by
        // mistake — open semantically means "this is a live row".
        session.closed_at = None;
        project.sessions.push(session.clone());
        Ok(session)
    }

    /// Mark a live session as closed without dropping the row. Stamps
    /// `closed_at = now`, runs the retention sweep scoped to the
    /// affected project. Returns the list of session ids pruned by the
    /// sweep (which may be empty). Mirrors C#
    /// `SessionStore.SoftCloseSessionAsync` minus the persistence /
    /// event-raise (callers persist).
    ///
    /// Idempotent: re-closing an already-closed session is a no-op
    /// success, returning an empty `pruned` list.
    pub fn soft_close(
        cfg: &mut ProjectsConfig,
        session_id: &str,
        now_iso: &str,
    ) -> Result<Vec<String>> {
        let mut affected_project: Option<String> = None;
        for project in cfg.projects.iter_mut() {
            if let Some(s) = project.sessions.iter_mut().find(|s| s.id == session_id) {
                if s.closed_at.is_some() {
                    return Ok(Vec::new());
                }
                s.closed_at = Some(now_iso.to_string());
                affected_project = Some(project.id.clone());
                break;
            }
        }
        let project_id = affected_project
            .ok_or_else(|| anyhow!("session '{session_id}' not found"))?;
        Ok(Self::apply_retention(cfg, now_iso, Some(&project_id)))
    }

    /// Clear `closed_at` on a soft-closed session and bump
    /// `last_opened = now`. Mirrors C#
    /// `SessionStore.RestoreSessionAsync`.
    pub fn reopen(
        cfg: &mut ProjectsConfig,
        session_id: &str,
        now_iso: &str,
    ) -> Result<Session> {
        for project in cfg.projects.iter_mut() {
            if let Some(s) = project.sessions.iter_mut().find(|s| s.id == session_id) {
                s.closed_at = None;
                s.last_opened = Some(now_iso.to_string());
                return Ok(s.clone());
            }
        }
        Err(anyhow!("session '{session_id}' not found"))
    }

    /// Drop a session row entirely. Mirrors C#
    /// `SessionStore.RemoveSessionAsync`. No retention sweep — this is
    /// the explicit forget-this-row path.
    pub fn hard_remove(cfg: &mut ProjectsConfig, session_id: &str) -> Result<()> {
        for project in cfg.projects.iter_mut() {
            let before = project.sessions.len();
            project.sessions.retain(|s| s.id != session_id);
            if project.sessions.len() != before {
                return Ok(());
            }
        }
        Err(anyhow!("session '{session_id}' not found"))
    }

    /// Update the user-visible display name on a session. Empty /
    /// whitespace `new_name` clears the override (so the auto-derived
    /// title kicks back in), matching C#
    /// `SessionStore.RenameSessionAsync`.
    pub fn rename(
        cfg: &mut ProjectsConfig,
        session_id: &str,
        new_name: Option<&str>,
    ) -> Result<()> {
        let normalized = new_name
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        for project in cfg.projects.iter_mut() {
            if let Some(s) = project.sessions.iter_mut().find(|s| s.id == session_id) {
                s.display_name = normalized;
                return Ok(());
            }
        }
        Err(anyhow!("session '{session_id}' not found"))
    }

    /// Update the persisted `agent_session_id` — typically called by
    /// the future agent-discovery layer once a CLI has reported its
    /// own session UUID. Mirrors C#
    /// `SessionStore.UpdateAgentSessionIdAsync`. Returns `true` when
    /// the value actually changed, `false` for a no-op (caller can
    /// skip persisting). Errors when the session is unknown.
    pub fn update_agent_session_id(
        cfg: &mut ProjectsConfig,
        session_id: &str,
        agent_session_id: Option<&str>,
    ) -> Result<bool> {
        for project in cfg.projects.iter_mut() {
            if let Some(s) = project.sessions.iter_mut().find(|s| s.id == session_id) {
                let next = agent_session_id.map(|x| x.to_string());
                if s.agent_session_id == next {
                    return Ok(false);
                }
                s.agent_session_id = next;
                return Ok(true);
            }
        }
        Err(anyhow!("session '{session_id}' not found"))
    }

    // ---- queries ---------------------------------------------------

    /// All sessions across all projects with `closed_at = None`.
    pub fn live(cfg: &ProjectsConfig) -> Vec<&Session> {
        cfg.projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .filter(|s| s.closed_at.is_none())
            .collect()
    }

    /// All sessions across all projects with `closed_at = Some(_)`,
    /// newest-first by `closed_at`. Sessions with unparseable
    /// timestamps sort last (treated as "really old").
    pub fn closed(cfg: &ProjectsConfig) -> Vec<&Session> {
        let mut out: Vec<&Session> = cfg
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .filter(|s| s.closed_at.is_some())
            .collect();
        out.sort_by(|a, b| {
            let ka = a.closed_at.as_deref().and_then(parse_iso8601_secs);
            let kb = b.closed_at.as_deref().and_then(parse_iso8601_secs);
            kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
        });
        out
    }

    // ---- retention -------------------------------------------------

    /// Apply the closed-session retention policy in place. Returns the
    /// ids of sessions dropped from `cfg` so the caller can raise
    /// removal events / invalidate caches. Idempotent: re-running on
    /// already-pruned state returns an empty list.
    ///
    /// `project_filter = Some(id)` scopes the sweep to a single
    /// project (matches C# `SoftCloseSessionAsync`'s narrow sweep).
    /// `None` walks every project (matches the one-time migration
    /// sweep on `LoadAsync`).
    pub fn apply_retention(
        cfg: &mut ProjectsConfig,
        now_iso: &str,
        project_filter: Option<&str>,
    ) -> Vec<String> {
        let now_secs = match parse_iso8601_secs(now_iso) {
            Some(v) => v,
            // Unparseable `now` means we can't run TTL math safely; the
            // cap pass still works without it, but bailing entirely is
            // the safer default — better to leave history intact than
            // to delete rows based on a corrupt `now`.
            None => return Vec::new(),
        };
        let ttl_cutoff = now_secs - (RetentionPolicy::MAX_AGE_DAYS as f64) * 86_400.0;

        let mut pruned = Vec::new();
        for project in cfg.projects.iter_mut() {
            if let Some(filter) = project_filter {
                if project.id != filter {
                    continue;
                }
            }

            let mut keep: Vec<Session> = Vec::with_capacity(project.sessions.len());
            // Bucket by worktree_id (None collapses to a single
            // orphan bucket, matching C#'s `s.WorktreeId ?? string.Empty`).
            // BTreeMap rather than HashMap so iteration is in stable
            // bucket-key order — without this, two consecutive
            // soft-closes that hit the cap would re-shuffle
            // `project.sessions` and write a different `projects.json`
            // each run, generating noisy diffs and hard-to-reproduce
            // ordering bugs (Copilot review on PR #114).
            let mut buckets: BTreeMap<String, Vec<Session>> = BTreeMap::new();

            for s in std::mem::take(&mut project.sessions) {
                let Some(closed_at) = s.closed_at.as_deref() else {
                    // Live session — never pruned.
                    keep.push(s);
                    continue;
                };
                match parse_iso8601_secs(closed_at) {
                    Some(secs) if secs < ttl_cutoff => {
                        pruned.push(s.id);
                        continue;
                    }
                    _ => {}
                }
                let key = s.worktree_id.clone().unwrap_or_default();
                buckets.entry(key).or_default().push(s);
            }

            for (_, mut bucket) in buckets {
                if bucket.len() <= RetentionPolicy::MAX_PER_WORKTREE {
                    keep.extend(bucket);
                    continue;
                }
                // Sort newest-first by closed_at; unparseable last.
                bucket.sort_by(|a, b| {
                    let ka = a.closed_at.as_deref().and_then(parse_iso8601_secs);
                    let kb = b.closed_at.as_deref().and_then(parse_iso8601_secs);
                    kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
                });
                for (i, s) in bucket.into_iter().enumerate() {
                    if i < RetentionPolicy::MAX_PER_WORKTREE {
                        keep.push(s);
                    } else {
                        pruned.push(s.id);
                    }
                }
            }

            project.sessions = keep;
        }

        pruned
    }
}

// Timestamp helpers live in [`crate::time`] so [`crate::claude_telemetry`]
// can share them — both call sites need the same narrow ISO-8601
// subset and were on the verge of drifting before consolidation
// (Copilot review on PR #114). [`now_iso8601`] is re-exported above.

/// Convenience: persist `cfg` via the standard
/// [`ProjectsConfig::save`] path. Wrapped here so future call sites
/// can switch to a session-only file (or anything else) without the
/// integration sites changing.
pub fn save(cfg: &ProjectsConfig, paths: &AppPaths) -> Result<()> {
    cfg.save(paths).context("save projects.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{Project, Session, Worktree};
    use crate::time::iso8601_from_unix_secs;

    fn fixed_now() -> &'static str { "2026-05-10T12:00:00Z" }

    fn iso_days_ago(days: u32) -> String {
        let secs_now = parse_iso8601_secs(fixed_now()).unwrap() as i64;
        let secs = secs_now - (days as i64) * 86_400;
        iso8601_from_unix_secs(secs)
    }

    fn make_project(id: &str) -> Project {
        Project {
            id: id.to_string(),
            name: "Repo".into(),
            path: "C:\\repo".into(),
            default_branch: "main".into(),
            worktree_root: None,
            default_agent_id: None,
            sessions: Vec::new(),
            worktrees: vec![Worktree {
                id: "primary".into(),
                path: "C:\\repo".into(),
                branch: None,
                is_primary: true,
            }],
        }
    }

    fn make_session(id: &str, worktree_id: Option<&str>) -> Session {
        Session {
            id: id.to_string(),
            worktree_path: "C:\\repo".into(),
            branch: None,
            agent_id: None,
            display_name: None,
            worktree_id: worktree_id.map(String::from),
            last_opened: None,
            agent_session_id: None,
            closed_at: None,
        }
    }

    // ---- open / soft_close / reopen / hard_remove --------------

    #[test]
    fn open_appends_and_stamps_last_opened() {
        let mut cfg = ProjectsConfig::default();
        cfg.projects.push(make_project("p1"));
        let s = SessionManager::open(
            &mut cfg,
            "p1",
            make_session("s1", Some("primary")),
            fixed_now(),
        )
        .unwrap();
        assert_eq!(s.last_opened.as_deref(), Some(fixed_now()));
        assert_eq!(cfg.projects[0].sessions.len(), 1);
    }

    #[test]
    fn open_clears_closed_at_even_if_caller_passed_one() {
        let mut cfg = ProjectsConfig::default();
        cfg.projects.push(make_project("p1"));
        let mut s = make_session("s1", Some("primary"));
        s.closed_at = Some("2026-04-01T10:00:00Z".into());
        let stored = SessionManager::open(&mut cfg, "p1", s, fixed_now()).unwrap();
        assert!(stored.closed_at.is_none());
    }

    #[test]
    fn open_unknown_project_errors() {
        let mut cfg = ProjectsConfig::default();
        let err = SessionManager::open(
            &mut cfg,
            "nope",
            make_session("s1", None),
            fixed_now(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("nope"));
    }

    #[test]
    fn soft_close_marks_closed_at() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        p.sessions.push(make_session("s1", Some("primary")));
        cfg.projects.push(p);

        let pruned = SessionManager::soft_close(&mut cfg, "s1", fixed_now()).unwrap();
        assert!(pruned.is_empty());
        assert_eq!(
            cfg.projects[0].sessions[0].closed_at.as_deref(),
            Some(fixed_now())
        );
    }

    #[test]
    fn soft_close_is_idempotent_on_already_closed() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        let mut s = make_session("s1", Some("primary"));
        s.closed_at = Some("2026-04-01T10:00:00Z".into());
        p.sessions.push(s);
        cfg.projects.push(p);

        let pruned = SessionManager::soft_close(&mut cfg, "s1", fixed_now()).unwrap();
        assert!(pruned.is_empty());
        // Original closed_at preserved — not overwritten with `now`.
        assert_eq!(
            cfg.projects[0].sessions[0].closed_at.as_deref(),
            Some("2026-04-01T10:00:00Z")
        );
    }

    #[test]
    fn soft_close_unknown_session_errors() {
        let mut cfg = ProjectsConfig::default();
        cfg.projects.push(make_project("p1"));
        let err = SessionManager::soft_close(&mut cfg, "ghost", fixed_now()).unwrap_err();
        assert!(err.to_string().contains("ghost"));
    }

    #[test]
    fn reopen_clears_closed_at_and_bumps_last_opened() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        let mut s = make_session("s1", Some("primary"));
        s.closed_at = Some("2026-04-01T10:00:00Z".into());
        s.last_opened = Some("2026-04-01T10:00:00Z".into());
        p.sessions.push(s);
        cfg.projects.push(p);

        let restored = SessionManager::reopen(&mut cfg, "s1", fixed_now()).unwrap();
        assert!(restored.closed_at.is_none());
        assert_eq!(restored.last_opened.as_deref(), Some(fixed_now()));
        assert!(cfg.projects[0].sessions[0].closed_at.is_none());
    }

    #[test]
    fn hard_remove_drops_the_row() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        p.sessions.push(make_session("s1", Some("primary")));
        p.sessions.push(make_session("s2", Some("primary")));
        cfg.projects.push(p);

        SessionManager::hard_remove(&mut cfg, "s1").unwrap();
        assert_eq!(cfg.projects[0].sessions.len(), 1);
        assert_eq!(cfg.projects[0].sessions[0].id, "s2");
    }

    #[test]
    fn hard_remove_unknown_errors() {
        let mut cfg = ProjectsConfig::default();
        cfg.projects.push(make_project("p1"));
        let err = SessionManager::hard_remove(&mut cfg, "ghost").unwrap_err();
        assert!(err.to_string().contains("ghost"));
    }

    #[test]
    fn rename_sets_and_clears_display_name() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        p.sessions.push(make_session("s1", None));
        cfg.projects.push(p);

        SessionManager::rename(&mut cfg, "s1", Some("My tab")).unwrap();
        assert_eq!(
            cfg.projects[0].sessions[0].display_name.as_deref(),
            Some("My tab")
        );
        // Whitespace-only clears the override.
        SessionManager::rename(&mut cfg, "s1", Some("   ")).unwrap();
        assert!(cfg.projects[0].sessions[0].display_name.is_none());
        // Explicit None also clears.
        SessionManager::rename(&mut cfg, "s1", Some("Re")).unwrap();
        SessionManager::rename(&mut cfg, "s1", None).unwrap();
        assert!(cfg.projects[0].sessions[0].display_name.is_none());
    }

    #[test]
    fn update_agent_session_id_no_op_when_unchanged() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        let mut s = make_session("s1", None);
        s.agent_session_id = Some("abc".into());
        p.sessions.push(s);
        cfg.projects.push(p);

        let changed = SessionManager::update_agent_session_id(&mut cfg, "s1", Some("abc"))
            .unwrap();
        assert!(!changed);

        let changed = SessionManager::update_agent_session_id(&mut cfg, "s1", Some("xyz"))
            .unwrap();
        assert!(changed);
        assert_eq!(
            cfg.projects[0].sessions[0].agent_session_id.as_deref(),
            Some("xyz")
        );

        let changed = SessionManager::update_agent_session_id(&mut cfg, "s1", None)
            .unwrap();
        assert!(changed);
        assert!(cfg.projects[0].sessions[0].agent_session_id.is_none());
    }

    // ---- queries ----------------------------------------------

    #[test]
    fn live_and_closed_partition_sessions() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        let mut a = make_session("a", None);
        let mut b = make_session("b", None);
        let c = make_session("c", None);
        a.closed_at = Some("2026-04-01T10:00:00Z".into());
        b.closed_at = Some("2026-04-02T10:00:00Z".into());
        p.sessions.push(a);
        p.sessions.push(b);
        p.sessions.push(c);
        cfg.projects.push(p);

        let live = SessionManager::live(&cfg);
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, "c");

        let closed = SessionManager::closed(&cfg);
        assert_eq!(closed.len(), 2);
        // Newest-first: b (Apr 2) before a (Apr 1).
        assert_eq!(closed[0].id, "b");
        assert_eq!(closed[1].id, "a");
    }

    // ---- retention --------------------------------------------

    #[test]
    fn retention_drops_ttl_expired_sessions() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        let mut old = make_session("old", Some("primary"));
        old.closed_at = Some(iso_days_ago(91));
        let mut fresh = make_session("fresh", Some("primary"));
        fresh.closed_at = Some(iso_days_ago(10));
        let live_one = make_session("live", Some("primary"));
        p.sessions.push(old);
        p.sessions.push(fresh);
        p.sessions.push(live_one);
        cfg.projects.push(p);

        let pruned = SessionManager::apply_retention(&mut cfg, fixed_now(), None);
        assert_eq!(pruned, vec!["old".to_string()]);
        let ids: Vec<_> = cfg.projects[0].sessions.iter().map(|s| &s.id).collect();
        assert!(ids.iter().any(|id| *id == "fresh"));
        assert!(ids.iter().any(|id| *id == "live"));
        assert!(!ids.iter().any(|id| *id == "old"));
    }

    #[test]
    fn retention_caps_per_worktree_keeping_newest() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        // Build MAX_PER_WORKTREE + 5 closed sessions on the same
        // worktree, all comfortably inside the TTL window so only the
        // cap pass should fire. Each one minute older than the next so
        // their `closed_at` ordering is total. Newest first: s0 most
        // recent, s104 the oldest of the batch (still < 90 days old).
        let now_secs = parse_iso8601_secs(fixed_now()).unwrap() as i64;
        for i in 0..(RetentionPolicy::MAX_PER_WORKTREE + 5) {
            let mut s = make_session(&format!("s{i}"), Some("primary"));
            s.closed_at = Some(iso8601_from_unix_secs(now_secs - (i as i64) * 60));
            p.sessions.push(s);
        }
        // Plus a row on a *different* worktree bucket that must NOT
        // count against the primary's cap.
        let mut other = make_session("other-1", Some("feat-x"));
        other.closed_at = Some(iso_days_ago(1));
        p.sessions.push(other);
        cfg.projects.push(p);

        let mut pruned = SessionManager::apply_retention(&mut cfg, fixed_now(), None);
        pruned.sort();
        assert_eq!(pruned.len(), 5, "only the cap overflow should be pruned");
        // The five oldest IDs are s100..s104 (cap is 100).
        let mut expected: Vec<String> = (0..5)
            .map(|i| format!("s{}", RetentionPolicy::MAX_PER_WORKTREE + i))
            .collect();
        expected.sort();
        assert_eq!(pruned, expected);
        // Other worktree bucket survives.
        assert!(
            cfg.projects[0]
                .sessions
                .iter()
                .any(|s| s.id == "other-1")
        );
    }

    #[test]
    fn retention_never_prunes_live_sessions() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        for i in 0..(RetentionPolicy::MAX_PER_WORKTREE + 10) {
            let mut s = make_session(&format!("live{i}"), Some("primary"));
            s.last_opened = Some(iso_days_ago(180));
            // closed_at intentionally None.
            p.sessions.push(s);
        }
        cfg.projects.push(p);

        let pruned = SessionManager::apply_retention(&mut cfg, fixed_now(), None);
        assert!(pruned.is_empty());
        assert_eq!(
            cfg.projects[0].sessions.len(),
            RetentionPolicy::MAX_PER_WORKTREE + 10
        );
    }

    #[test]
    fn retention_is_idempotent() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        let mut s = make_session("s1", Some("primary"));
        s.closed_at = Some(iso_days_ago(91));
        p.sessions.push(s);
        cfg.projects.push(p);

        let first = SessionManager::apply_retention(&mut cfg, fixed_now(), None);
        let second = SessionManager::apply_retention(&mut cfg, fixed_now(), None);
        assert_eq!(first, vec!["s1".to_string()]);
        assert!(second.is_empty());
    }

    #[test]
    fn retention_filter_scoped_to_one_project() {
        let mut cfg = ProjectsConfig::default();
        let mut p1 = make_project("p1");
        let mut p2 = make_project("p2");
        let mut s_p1 = make_session("p1-old", Some("primary"));
        s_p1.closed_at = Some(iso_days_ago(91));
        let mut s_p2 = make_session("p2-old", Some("primary"));
        s_p2.closed_at = Some(iso_days_ago(91));
        p1.sessions.push(s_p1);
        p2.sessions.push(s_p2);
        cfg.projects.push(p1);
        cfg.projects.push(p2);

        let pruned = SessionManager::apply_retention(&mut cfg, fixed_now(), Some("p1"));
        assert_eq!(pruned, vec!["p1-old".to_string()]);
        // p2 untouched because the filter scoped to p1.
        assert_eq!(cfg.projects[1].sessions.len(), 1);
    }

    #[test]
    fn retention_buckets_orphans_when_worktree_id_is_none() {
        // No `worktree_id` should still get cap-enforced under a
        // single shared "orphan" bucket — matches C#'s
        // `s.WorktreeId ?? string.Empty` collapse. All inside the TTL
        // window so only the cap pass fires.
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        let now_secs = parse_iso8601_secs(fixed_now()).unwrap() as i64;
        for i in 0..(RetentionPolicy::MAX_PER_WORKTREE + 3) {
            let mut s = make_session(&format!("orphan{i}"), None);
            s.closed_at = Some(iso8601_from_unix_secs(now_secs - (i as i64) * 60));
            p.sessions.push(s);
        }
        cfg.projects.push(p);

        let pruned = SessionManager::apply_retention(&mut cfg, fixed_now(), None);
        assert_eq!(pruned.len(), 3);
    }

    // ---- soft_close + retention interaction --------------------

    #[test]
    fn soft_close_runs_scoped_retention_and_returns_pruned_ids() {
        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        // MAX_PER_WORKTREE older closed rows already on disk, all
        // safely inside the TTL window so the cap pass is the only
        // thing firing on this soft-close. One more soft-close pushes
        // the bucket past the cap.
        let now_secs = parse_iso8601_secs(fixed_now()).unwrap() as i64;
        for i in 0..RetentionPolicy::MAX_PER_WORKTREE {
            let mut s = make_session(&format!("s{i}"), Some("primary"));
            s.closed_at =
                Some(iso8601_from_unix_secs(now_secs - ((i + 1) as i64) * 60));
            p.sessions.push(s);
        }
        // The live row we're about to soft-close.
        p.sessions.push(make_session("new", Some("primary")));
        cfg.projects.push(p);

        let pruned = SessionManager::soft_close(&mut cfg, "new", fixed_now()).unwrap();
        // Exactly one prune — the oldest closed row in the bucket.
        assert_eq!(pruned.len(), 1);
        // The newest (`new`) survives.
        assert!(
            cfg.projects[0]
                .sessions
                .iter()
                .any(|s| s.id == "new" && s.closed_at.is_some())
        );
    }

    // ---- load + sweep --------------------------------------------

    #[test]
    fn load_with_sweep_handles_missing_file_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = SessionManager::load_from_with_sweep(
            &dir.path().join("nope.json"),
            fixed_now(),
        )
        .unwrap();
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn load_with_sweep_prunes_legacy_overflow_on_first_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");

        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        let mut old = make_session("old", Some("primary"));
        old.closed_at = Some(iso_days_ago(180));
        p.sessions.push(old);
        p.sessions.push(make_session("live", Some("primary")));
        cfg.projects.push(p);
        cfg.save_to(&path).unwrap();

        let loaded = SessionManager::load_from_with_sweep(&path, fixed_now()).unwrap();
        let ids: Vec<_> = loaded.projects[0]
            .sessions
            .iter()
            .map(|s| s.id.clone())
            .collect();
        assert!(!ids.iter().any(|id| id == "old"));
        assert!(ids.iter().any(|id| id == "live"));
    }

    // ---- dev-mode path separation -------------------------------

    #[test]
    fn dev_and_prod_paths_are_separate_so_session_files_dont_collide() {
        let root = tempfile::tempdir().unwrap();
        let prod = crate::paths::rooted_for_tests(false, root.path());
        let dev = crate::paths::rooted_for_tests(true, root.path());

        prod.ensure_dirs().unwrap();
        dev.ensure_dirs().unwrap();

        // Save distinct configs through each AppPaths.
        let mut prod_cfg = ProjectsConfig::default();
        prod_cfg.projects.push(make_project("prod-only"));
        prod_cfg.save(&prod).unwrap();

        let mut dev_cfg = ProjectsConfig::default();
        dev_cfg.projects.push(make_project("dev-only"));
        dev_cfg.save(&dev).unwrap();

        // Each side reads back only its own data.
        let reloaded_prod = SessionManager::load_with_sweep(&prod, fixed_now()).unwrap();
        assert_eq!(reloaded_prod.projects.len(), 1);
        assert_eq!(reloaded_prod.projects[0].id, "prod-only");

        let reloaded_dev = SessionManager::load_with_sweep(&dev, fixed_now()).unwrap();
        assert_eq!(reloaded_dev.projects.len(), 1);
        assert_eq!(reloaded_dev.projects[0].id, "dev-only");

        // Path-level guarantee: the dev file is under a `*.Dev` folder
        // and the two persisted files are in different directories.
        assert_ne!(prod.projects_file(), dev.projects_file());
        assert!(
            dev.projects_file()
                .to_string_lossy()
                .contains("CodeScope.Dev")
        );
    }

    // ---- round-trip serialisation -------------------------------

    #[test]
    fn round_trip_preserves_session_lifecycle_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");

        let mut cfg = ProjectsConfig::default();
        let mut p = make_project("p1");
        let mut closed = make_session("closed", Some("primary"));
        closed.closed_at = Some("2026-04-01T10:00:00Z".into());
        closed.last_opened = Some("2026-03-31T09:30:00Z".into());
        closed.agent_session_id = Some("uuid-abc".into());
        closed.display_name = Some("Renamed".into());
        let mut live = make_session("live", Some("primary"));
        live.last_opened = Some(fixed_now().to_string());
        p.sessions.push(closed);
        p.sessions.push(live);
        cfg.projects.push(p);
        cfg.save_to(&path).unwrap();

        let reloaded = SessionManager::load_from_with_sweep(&path, fixed_now()).unwrap();
        let p = &reloaded.projects[0];
        let closed = p.sessions.iter().find(|s| s.id == "closed").unwrap();
        assert_eq!(closed.closed_at.as_deref(), Some("2026-04-01T10:00:00Z"));
        assert_eq!(closed.agent_session_id.as_deref(), Some("uuid-abc"));
        assert_eq!(closed.display_name.as_deref(), Some("Renamed"));
        let live = p.sessions.iter().find(|s| s.id == "live").unwrap();
        assert!(live.closed_at.is_none());
    }

    // ---- timestamp helpers -------------------------------------

    #[test]
    fn iso8601_round_trip_is_stable() {
        let s = "2026-05-10T12:00:00Z";
        let secs = parse_iso8601_secs(s).unwrap();
        let back = iso8601_from_unix_secs(secs as i64);
        assert_eq!(back, s);
    }

    #[test]
    fn iso8601_handles_offset_and_fractional() {
        let with_z = parse_iso8601_secs("2026-05-10T12:00:00Z").unwrap();
        let with_offset = parse_iso8601_secs("2026-05-10T12:00:00+00:00").unwrap();
        let with_frac = parse_iso8601_secs("2026-05-10T12:00:00.500Z").unwrap();
        assert!((with_z - with_offset).abs() < 1.0e-9);
        assert!((with_frac - with_z - 0.5).abs() < 1.0e-9);
    }

    #[test]
    fn iso8601_unparseable_returns_none() {
        assert!(parse_iso8601_secs("nope").is_none());
        assert!(parse_iso8601_secs("2026-13-99T99:99:99Z").is_none());
    }

    #[test]
    fn now_iso8601_is_parseable_back() {
        // Smoke test: the value we synthesise as "now" must round-trip
        // through our own parser, otherwise the production retention
        // sweep would early-return on every call.
        let now = now_iso8601();
        assert!(parse_iso8601_secs(&now).is_some(), "produced: {now}");
    }

    // ---- AppShell ↔ SessionManager persistence wiring ----------
    //
    // These tests exercise the load_with_sweep → open → save →
    // load → soft_close → save round-trip the AppShell relies on
    // (Big-Step-2). The integration site itself is gpui-bound and
    // hard to unit-test; the persistence shape is what matters and
    // can be tested through the public API alone.

    #[test]
    fn appshell_lifecycle_round_trip_through_disk() {
        let root = tempfile::tempdir().unwrap();
        let paths = crate::paths::rooted_for_tests(false, root.path());
        paths.ensure_dirs().unwrap();

        // Seed: one project with a primary worktree, no sessions yet.
        let mut seed = ProjectsConfig::default();
        seed.projects.push(make_project("p1"));
        seed.save(&paths).unwrap();

        // 1. AppShell launch: load with sweep — empty list, no prune.
        let cfg = SessionManager::load_with_sweep(&paths, fixed_now()).unwrap();
        assert_eq!(cfg.projects[0].sessions.len(), 0);

        // 2. spawn_tab → SessionManager::open → save.
        let mut cfg = ProjectsConfig::load(&paths).unwrap();
        let session = make_session("s1", Some("primary"));
        SessionManager::open(&mut cfg, "p1", session, fixed_now()).unwrap();
        save(&cfg, &paths).unwrap();

        // 3. Reload and verify the row persisted with last_opened set.
        let reloaded = ProjectsConfig::load(&paths).unwrap();
        assert_eq!(reloaded.projects[0].sessions.len(), 1);
        let row = &reloaded.projects[0].sessions[0];
        assert_eq!(row.id, "s1");
        assert_eq!(row.last_opened.as_deref(), Some(fixed_now()));
        assert!(row.closed_at.is_none());

        // 4. close_tab → soft_close → save.
        let mut cfg = ProjectsConfig::load(&paths).unwrap();
        let pruned = SessionManager::soft_close(&mut cfg, "s1", fixed_now()).unwrap();
        assert!(pruned.is_empty());
        save(&cfg, &paths).unwrap();

        // 5. Reload — row still present, now with closed_at.
        let reloaded = ProjectsConfig::load(&paths).unwrap();
        assert_eq!(reloaded.projects[0].sessions.len(), 1);
        assert_eq!(
            reloaded.projects[0].sessions[0].closed_at.as_deref(),
            Some(fixed_now())
        );
    }

    #[test]
    fn appshell_dev_mode_keeps_session_files_separate() {
        // Mirrors the in-module dev/prod test, but goes through the
        // open + save lifecycle the AppShell would actually drive,
        // not just save → load. Catches a future regression where a
        // stray hard-coded path ignores the dev-mode redirection.
        let root = tempfile::tempdir().unwrap();
        let prod = crate::paths::rooted_for_tests(false, root.path());
        let dev = crate::paths::rooted_for_tests(true, root.path());
        prod.ensure_dirs().unwrap();
        dev.ensure_dirs().unwrap();

        let mut seed = ProjectsConfig::default();
        seed.projects.push(make_project("p1"));
        seed.save(&prod).unwrap();
        let mut seed = ProjectsConfig::default();
        seed.projects.push(make_project("p1"));
        seed.save(&dev).unwrap();

        // Open a session under each side and persist independently.
        let mut prod_cfg = SessionManager::load_with_sweep(&prod, fixed_now()).unwrap();
        SessionManager::open(
            &mut prod_cfg,
            "p1",
            make_session("prod-s", Some("primary")),
            fixed_now(),
        )
        .unwrap();
        save(&prod_cfg, &prod).unwrap();

        let mut dev_cfg = SessionManager::load_with_sweep(&dev, fixed_now()).unwrap();
        SessionManager::open(
            &mut dev_cfg,
            "p1",
            make_session("dev-s", Some("primary")),
            fixed_now(),
        )
        .unwrap();
        save(&dev_cfg, &dev).unwrap();

        // Each side reads back only its own session.
        let prod_back = ProjectsConfig::load(&prod).unwrap();
        let dev_back = ProjectsConfig::load(&dev).unwrap();
        assert_eq!(prod_back.projects[0].sessions[0].id, "prod-s");
        assert_eq!(dev_back.projects[0].sessions[0].id, "dev-s");
    }

    #[test]
    fn appshell_close_after_concurrent_sidebar_reload_does_not_lose_data() {
        // Reproduces the AppShell pattern: reload-from-disk before
        // each session mutation so a sidebar write between two of
        // ours doesn't get clobbered.
        let root = tempfile::tempdir().unwrap();
        let paths = crate::paths::rooted_for_tests(false, root.path());
        paths.ensure_dirs().unwrap();

        let mut seed = ProjectsConfig::default();
        seed.projects.push(make_project("p1"));
        seed.save(&paths).unwrap();

        // AppShell opens session s1.
        let mut cfg = ProjectsConfig::load(&paths).unwrap();
        SessionManager::open(
            &mut cfg,
            "p1",
            make_session("s1", Some("primary")),
            fixed_now(),
        )
        .unwrap();
        save(&cfg, &paths).unwrap();

        // Sidebar (out-of-band) appends a second project to disk
        // — simulating an `add_project` write between AppShell ticks.
        let mut sidebar_view = ProjectsConfig::load(&paths).unwrap();
        sidebar_view.projects.push(make_project("p2"));
        sidebar_view.save(&paths).unwrap();

        // AppShell soft-closes s1, going through reload-from-disk
        // first (mirrors `AppShell::soft_close_session`).
        let mut cfg = ProjectsConfig::load(&paths).unwrap();
        SessionManager::soft_close(&mut cfg, "s1", fixed_now()).unwrap();
        save(&cfg, &paths).unwrap();

        // Final state: both projects present, s1 closed.
        let final_cfg = ProjectsConfig::load(&paths).unwrap();
        assert_eq!(final_cfg.projects.len(), 2);
        assert!(
            final_cfg.projects.iter().any(|p| p.id == "p2"),
            "sidebar's p2 must survive the AppShell close"
        );
        let s1 = final_cfg
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .find(|s| s.id == "s1")
            .expect("s1 must remain on disk");
        assert!(s1.closed_at.is_some(), "s1 must be soft-closed");
    }
}
