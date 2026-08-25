//! Pure-data helpers for the Overview panel.
//!
//! The Overview view (see `src/overview.rs`) is an
//! "active sessions" dashboard — one row per currently open session
//! across every project. The on-screen sort + filter belong on the
//! gpui side, but the row-building + sort key derivation are pure-data
//! so they live here and ship with unit tests.
//!
//! Mirrors the C# `OverviewViewModel.Rebuild` flow: walk every project,
//! enumerate live tabs (`closed_at = None`), and surface them sorted
//! newest-first by `last_opened`. Closed (soft-deleted) sessions are
//! deliberately *not* surfaced here — earlier revisions of the Rust
//! port appended a closed-history block to this grid, but it was just
//! noise next to live work. The reopen flow still exists via the
//! sidebar's history menu; Overview is for what's open right now.

use crate::projects::{Project, ProjectsConfig, Session};
use crate::time::parse_iso8601_secs;

/// Lifecycle discriminator for an Overview row. Today only `Live`
/// rows are surfaced by [`build_rows`] — the panel is an active-
/// sessions dashboard. The enum is kept (rather than collapsed away)
/// so the gpui-side render path can stay shape-stable if a future
/// "show recently closed" toggle reintroduces closed rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewLifecycle {
    /// `closed_at == None` — the session row is live on disk; the
    /// runtime may or may not have a matching tab open.
    Live,
    /// `closed_at == Some(_)` — soft-closed session row, eligible for
    /// reopen via [`crate::SessionManager::reopen`]. Not currently
    /// emitted by [`build_rows`]; reserved for a future toggle.
    Closed,
}

/// One row in the Overview grid. Built from a [`Session`] + its
/// owning [`Project`]; the gpui-side renderer joins live rows with
/// the runtime `Tab` state (for "Focus" actions) and the telemetry
/// tail (for tokens / duration / live state).
#[derive(Debug, Clone)]
pub struct OverviewRow {
    /// Stable session id — the join key against the runtime tab list
    /// and the closed-session reopen flow.
    pub session_id: String,
    /// Display name for the project that owns the session. Mirrors
    /// C# `OverviewCardViewModel.ProjectName`.
    pub project_name: String,
    /// Branch label — when the session's `branch` is `None` this
    /// falls back to the owning worktree's branch, then the worktree
    /// folder leaf, and only then the raw worktree id. Sessions are
    /// persisted with `branch: None` until the first git poll lands,
    /// so an id-first fallback rendered UUID hex as the "worktree
    /// name" in the Overview (#319).
    pub branch_label: String,
    /// Absolute path the session was opened in. Used by the renderer
    /// to look up the matching live tab (group + tab index).
    pub working_directory: String,
    /// The persisted `agent_id` ("claude", "codex", "copilot",
    /// "opencode", "pi", "gemini") or `None` for plain shell sessions.
    pub agent_id: Option<String>,
    /// `closed_at` raw ISO 8601 string. `None` for live rows; the
    /// renderer formats this via
    /// [`crate::session::format_closed_at_relative`].
    pub closed_at: Option<String>,
    /// `last_opened` ISO 8601 string. Used as the live-row tiebreaker
    /// in [`sort_rows`] so the newest-opened session lands at the top.
    pub last_opened: Option<String>,
    pub lifecycle: OverviewLifecycle,
}

impl OverviewRow {
    fn from_session(project: &Project, session: &Session) -> Self {
        // Resolve a human-readable label: session branch → owning
        // worktree's branch → worktree folder leaf → session path
        // leaf → raw worktree id (last resort; a UUID, so only when
        // every path/branch slot is empty).
        let worktree = session.worktree_id.as_deref().and_then(|id| {
            project.worktrees.iter().find(|wt| wt.id == id)
        });
        let branch_label = session
            .branch
            .clone()
            .or_else(|| worktree.and_then(|wt| wt.branch.clone()))
            .or_else(|| worktree.and_then(|wt| path_leaf(&wt.path)))
            .or_else(|| path_leaf(&session.worktree_path))
            .or_else(|| session.worktree_id.clone())
            .unwrap_or_default();
        let lifecycle = if session.closed_at.is_some() {
            OverviewLifecycle::Closed
        } else {
            OverviewLifecycle::Live
        };
        Self {
            session_id: session.id.clone(),
            project_name: project.name.clone(),
            branch_label,
            working_directory: session.worktree_path.clone(),
            agent_id: session.agent_id.clone(),
            closed_at: session.closed_at.clone(),
            last_opened: session.last_opened.clone(),
            lifecycle,
        }
    }
}

/// Last path component of `path`, splitting on both `/` and `\` so
/// Windows-written `projects.json` entries resolve on every platform
/// (mirrors the tab-title logic's tolerance for foreign separators).
/// `None` for an empty result so callers can keep falling back.
fn path_leaf(path: &str) -> Option<String> {
    let leaf = path
        .trim_end_matches(['/', '\\'])
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or_default();
    if leaf.is_empty() { None } else { Some(leaf.to_string()) }
}

/// Build the flat row list from a [`ProjectsConfig`] snapshot —
/// **live sessions only**, sorted newest-first by `last_opened`.
/// Mirrors C# `OverviewViewModel.Rebuild` minus the per-card preview
/// lines (the Rust port builds those in the gpui layer so it can fold
/// in live telemetry without re-running this pass).
///
/// Closed sessions are filtered out here — the Overview panel is an
/// "active sessions" dashboard and closed-history noise was hurting
/// the signal-to-noise ratio. The reopen flow still exists via the
/// sidebar's per-project history menu.
pub fn build_rows(cfg: &ProjectsConfig) -> Vec<OverviewRow> {
    let mut rows: Vec<OverviewRow> = cfg
        .projects
        .iter()
        .flat_map(|p| {
            p.sessions
                .iter()
                .filter(|s| s.closed_at.is_none())
                .map(move |s| OverviewRow::from_session(p, s))
        })
        .collect();
    sort_rows(&mut rows);
    rows
}

/// Like [`build_rows`], but additionally filters to sessions whose
/// `id` is in `live_session_ids` — i.e. tabs that are currently open
/// in the running app, not just any persisted record with
/// `closed_at = None`. The persistence layer can drift from live
/// state (crashes leave orphan rows, layout-restored sessions that
/// were never actually reopened, …); the Overview should reflect the
/// running tab strip, mirroring C# `MainViewModel.OpenTabs`.
pub fn build_rows_for_live<S>(cfg: &ProjectsConfig, live_session_ids: &S) -> Vec<OverviewRow>
where
    S: LiveSessionLookup + ?Sized,
{
    let mut rows: Vec<OverviewRow> = cfg
        .projects
        .iter()
        .flat_map(|p| {
            p.sessions
                .iter()
                .filter(|s| s.closed_at.is_none() && live_session_ids.contains(&s.id))
                .map(move |s| OverviewRow::from_session(p, s))
        })
        .collect();
    sort_rows(&mut rows);
    rows
}

/// Abstraction so the filter helper works with `HashSet<&str>`,
/// `HashSet<String>`, or `&[String]` without forcing the caller into
/// one concrete shape.
pub trait LiveSessionLookup {
    fn contains(&self, id: &str) -> bool;
}

impl LiveSessionLookup for std::collections::HashSet<String> {
    fn contains(&self, id: &str) -> bool { self.contains(id) }
}

impl LiveSessionLookup for [String] {
    fn contains(&self, id: &str) -> bool { self.iter().any(|s| s == id) }
}

/// Sort an Overview row list in display order:
///
/// 1. Live rows first (live before closed),
/// 2. within live: newest `last_opened` first (missing values sort last),
/// 3. within closed (only reachable if a future caller surfaces them):
///    newest `closed_at` first (matches
///    [`crate::SessionManager::closed`]).
///
/// Mirrors the visual reading order of the C# Overview's
/// `FilteredCards` — active sessions read top-to-bottom.
pub fn sort_rows(rows: &mut [OverviewRow]) {
    rows.sort_by(|a, b| {
        match (a.lifecycle, b.lifecycle) {
            (OverviewLifecycle::Live, OverviewLifecycle::Closed) => std::cmp::Ordering::Less,
            (OverviewLifecycle::Closed, OverviewLifecycle::Live) => std::cmp::Ordering::Greater,
            (OverviewLifecycle::Live, OverviewLifecycle::Live) => {
                // Newest last_opened first; missing values sort last
                // (treated as "really old") so a freshly-spawned tab
                // without a stamped `last_opened` doesn't accidentally
                // outrank a recently-opened row.
                let ka = a.last_opened.as_deref().and_then(parse_iso8601_secs);
                let kb = b.last_opened.as_deref().and_then(parse_iso8601_secs);
                cmp_desc_with_none_last(ka, kb)
            }
            (OverviewLifecycle::Closed, OverviewLifecycle::Closed) => {
                let ka = a.closed_at.as_deref().and_then(parse_iso8601_secs);
                let kb = b.closed_at.as_deref().and_then(parse_iso8601_secs);
                cmp_desc_with_none_last(ka, kb)
            }
        }
    });
}

/// Compare two `Option<f64>` keys with "newer first" semantics
/// (descending) where `None` sorts *last* rather than the
/// `partial_cmp` default of "None < Some". Without this any row
/// missing the key would float to the top and bury rows with real
/// timestamps.
fn cmp_desc_with_none_last(a: Option<f64>, b: Option<f64>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(ka), Some(kb)) => kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::{Project, ProjectKind, Session, Worktree};

    fn mk_project(id: &str, name: &str, sessions: Vec<Session>) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("C:\\dev\\{name}"),
            default_branch: "main".into(),
            worktree_root: None,
            kind: ProjectKind::Local,
            command: None,
            remote_agent_id: None,
            default_agent_id: None,
            sessions,
            worktrees: vec![Worktree {
                id: "primary".into(),
                path: format!("C:\\dev\\{name}"),
                branch: None,
                is_primary: true,
            }],
        }
    }

    fn mk_session(id: &str, last_opened: Option<&str>, closed_at: Option<&str>) -> Session {
        Session {
            id: id.into(),
            worktree_path: "C:\\dev\\demo".into(),
            branch: Some("main".into()),
            agent_id: Some("claude".into()),
            display_name: None,
            worktree_id: Some("primary".into()),
            last_opened: last_opened.map(|s| s.to_string()),
            agent_session_id: None,
            closed_at: closed_at.map(|s| s.to_string()),
        }
    }

    #[test]
    fn build_rows_flattens_every_project() {
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![
                mk_project("p1", "alpha", vec![mk_session("s1", Some("2026-05-11T12:00:00Z"), None)]),
                mk_project("p2", "beta", vec![mk_session("s2", Some("2026-05-11T11:00:00Z"), None)]),
            ],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].project_name, "alpha"); // newer last_opened
        assert_eq!(rows[1].project_name, "beta");
    }

    #[test]
    fn closed_rows_are_filtered_out() {
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project(
                "p1",
                "alpha",
                vec![
                    // closed but recent — must NOT appear
                    mk_session("closed_recent", None, Some("2026-05-11T12:30:00Z")),
                    // live but stale — must appear
                    mk_session("live_old", Some("2026-05-01T08:00:00Z"), None),
                ],
            )],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows.len(), 1, "Overview surfaces live sessions only");
        assert_eq!(rows[0].session_id, "live_old");
        assert_eq!(rows[0].lifecycle, OverviewLifecycle::Live);
    }

    #[test]
    fn build_rows_excludes_every_closed_session() {
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project(
                "p1",
                "alpha",
                vec![
                    mk_session("c1", None, Some("2026-05-09T09:00:00Z")),
                    mk_session("c2", None, Some("2026-05-11T09:00:00Z")),
                    mk_session("c3", None, Some("2026-05-10T09:00:00Z")),
                ],
            )],
        };

        let rows = build_rows(&cfg);
        assert!(rows.is_empty(), "no closed rows must leak into Overview");
    }

    #[test]
    fn missing_last_opened_sorts_after_real_timestamps() {
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project(
                "p1",
                "alpha",
                vec![
                    mk_session("no_ts", None, None),
                    mk_session("with_ts", Some("2026-05-11T10:00:00Z"), None),
                ],
            )],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows[0].session_id, "with_ts");
        assert_eq!(rows[1].session_id, "no_ts");
    }

    #[test]
    fn branch_label_falls_back_to_worktree_folder_leaf_not_id() {
        // Sessions are persisted with `branch: None` until the first
        // git poll backfills it — the label must resolve through the
        // owning worktree's path, never the raw worktree id (#319).
        let mut s = mk_session("s1", None, None);
        s.branch = None;
        s.worktree_id = Some("3f9a1c2e-77aa-4bd0-9c55-0d6f2b8e4a11".into());
        let mut p = mk_project("p1", "alpha", vec![]);
        p.worktrees.push(Worktree {
            id: "3f9a1c2e-77aa-4bd0-9c55-0d6f2b8e4a11".into(),
            path: "C:\\dev\\alpha.worktrees\\profit-wt-1".into(),
            branch: None,
            is_primary: false,
        });
        p.sessions = vec![s];
        let cfg = ProjectsConfig { version: 1, agents: vec![], projects: vec![p] };

        let rows = build_rows(&cfg);
        assert_eq!(rows[0].branch_label, "profit-wt-1");
    }

    #[test]
    fn branch_label_prefers_owning_worktree_branch_over_paths() {
        let mut s = mk_session("s1", None, None);
        s.branch = None;
        s.worktree_id = Some("wt1".into());
        let mut p = mk_project("p1", "alpha", vec![]);
        p.worktrees.push(Worktree {
            id: "wt1".into(),
            path: "/home/u/alpha.worktrees/feat-x-dir".into(),
            branch: Some("feat/x".into()),
            is_primary: false,
        });
        p.sessions = vec![s];
        let cfg = ProjectsConfig { version: 1, agents: vec![], projects: vec![p] };

        let rows = build_rows(&cfg);
        assert_eq!(rows[0].branch_label, "feat/x");
    }

    #[test]
    fn branch_label_unknown_worktree_uses_session_path_leaf() {
        // Worktree id points at nothing (stale record) — the session's
        // own working directory still beats showing the UUID.
        let mut s = mk_session("s1", None, None);
        s.branch = None;
        s.worktree_id = Some("gone-uuid".into());
        s.worktree_path = "/home/u/alpha.worktrees/focus-wt-2".into();
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project("p1", "alpha", vec![s])],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows[0].branch_label, "focus-wt-2");
    }

    #[test]
    fn path_leaf_handles_both_separators_and_empty() {
        assert_eq!(path_leaf("C:\\a\\b\\leaf"), Some("leaf".into()));
        assert_eq!(path_leaf("/a/b/leaf"), Some("leaf".into()));
        assert_eq!(path_leaf("/a/b/leaf/"), Some("leaf".into()));
        assert_eq!(path_leaf(""), None);
        assert_eq!(path_leaf("///"), None);
    }

    #[test]
    fn lifecycle_derives_from_closed_at() {
        let live = OverviewRow::from_session(
            &mk_project("p1", "a", vec![]),
            &mk_session("s1", None, None),
        );
        assert_eq!(live.lifecycle, OverviewLifecycle::Live);

        let closed = OverviewRow::from_session(
            &mk_project("p1", "a", vec![]),
            &mk_session("s2", None, Some("2026-05-11T10:00:00Z")),
        );
        assert_eq!(closed.lifecycle, OverviewLifecycle::Closed);
    }

}
