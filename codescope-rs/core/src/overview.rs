//! Pure-data helpers for the Overview panel.
//!
//! The Rust Overview view (see `codescope-rs/src/overview.rs`) shows
//! every open + recently-closed session across every project in one
//! grid. The on-screen sort + filter belong on the gpui side, but the
//! row-building + sort key derivation are pure-data so they live here
//! and ship with unit tests.
//!
//! Mirrors the C# `OverviewViewModel.Rebuild` flow: walk every project,
//! enumerate live tabs (`closed_at = None`) plus closed rows
//! (`closed_at = Some`), and surface them ordered "live first, closed
//! sorted newest-first by `closed_at`". The C# view only renders live
//! sessions; the Rust port surfaces closed history too because the
//! brief explicitly asks for "open + recently-closed sessions" in one
//! panel.

use crate::projects::{Project, ProjectsConfig, Session};
use crate::time::parse_iso8601_secs;

/// Live vs. closed discriminator for an Overview row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverviewLifecycle {
    /// `closed_at == None` — the session row is live on disk; the
    /// runtime may or may not have a matching tab open.
    Live,
    /// `closed_at == Some(_)` — soft-closed session row, eligible for
    /// reopen via [`crate::SessionManager::reopen`].
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
    /// Branch label — falls back to the worktree id when `branch` is
    /// `None`. Matches C# `WorktreeViewModel.DisplayBranch`'s shape.
    pub branch_label: String,
    /// Absolute path the session was opened in. Used by the renderer
    /// to look up the matching live tab (group + tab index).
    pub working_directory: String,
    /// The persisted `agent_id` ("claude", "codex", "copilot",
    /// "opencode", "pi") or `None` for plain shell sessions.
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
        let branch_label = session
            .branch
            .clone()
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

/// Default cap on the number of closed-session rows surfaced in the
/// Overview. Mirrors the "show the last 20 closed sessions" heuristic
/// the brief calls out — the on-disk `RetentionPolicy` keeps a much
/// larger window (60 days / 200 entries per project) so the user can
/// reopen old work, but dumping every retained closed row into a
/// single grid drowns the panel. Live rows are never capped.
pub const DEFAULT_MAX_CLOSED_ROWS: usize = 20;

/// Closed-row dedup window. Two closed sessions for the same
/// `(project_name, agent_id)` whose `closed_at` stamps fall within
/// this window are treated as the same logical tab restarted and
/// only the newest row is kept. The 5-minute span was picked to
/// match the brief — long enough to swallow a quick crash + respawn
/// loop, short enough that two genuinely separate working sessions
/// from the same agent on the same project don't get folded into
/// one row.
pub const CLOSED_DEDUP_WINDOW_SECS: f64 = 5.0 * 60.0;

/// Build the flat row list from a [`ProjectsConfig`] snapshot,
/// already sorted via [`sort_rows`] and capped at
/// [`DEFAULT_MAX_CLOSED_ROWS`] closed rows. Mirrors C#
/// `OverviewViewModel.Rebuild` minus the per-card preview lines (the
/// Rust port builds those in the gpui layer so it can fold in live
/// telemetry without re-running this pass).
pub fn build_rows(cfg: &ProjectsConfig) -> Vec<OverviewRow> {
    build_rows_capped(cfg, DEFAULT_MAX_CLOSED_ROWS)
}

/// Build the flat row list with an explicit cap on the number of
/// closed-session rows. Live rows are never capped; closed rows are
/// sorted newest-first, deduped by `(project_name, agent_id)` within
/// a [`CLOSED_DEDUP_WINDOW_SECS`] window (so a quick respawn loop
/// only surfaces the most recent row), and then truncated to
/// `max_closed` entries.
pub fn build_rows_capped(cfg: &ProjectsConfig, max_closed: usize) -> Vec<OverviewRow> {
    let mut rows: Vec<OverviewRow> = cfg
        .projects
        .iter()
        .flat_map(|p| p.sessions.iter().map(move |s| OverviewRow::from_session(p, s)))
        .collect();
    sort_rows(&mut rows);

    // Split into live (no cap) + closed (sorted newest-first by
    // virtue of `sort_rows`). The live partition keeps its sort order
    // because `sort_rows` already puts live before closed.
    let split = rows
        .iter()
        .position(|r| r.lifecycle == OverviewLifecycle::Closed)
        .unwrap_or(rows.len());
    let closed_tail = rows.split_off(split);
    let mut live = rows;

    // Dedup near-identical closed rows. For each kept row we record
    // the (project_name, agent_id, closed_at_secs) key. A candidate
    // row is suppressed when an earlier (newer) row shares the same
    // (project, agent) and its `closed_at` is within the dedup
    // window. We keep the newest row of each cluster which falls out
    // naturally because `closed_tail` is already newest-first.
    let mut deduped: Vec<OverviewRow> = Vec::with_capacity(closed_tail.len().min(max_closed));
    let mut kept_keys: Vec<(String, Option<String>, Option<f64>)> = Vec::new();
    for row in closed_tail {
        let key_proj = row.project_name.clone();
        let key_agent = row.agent_id.clone();
        let row_secs = row.closed_at.as_deref().and_then(parse_iso8601_secs);
        let is_dup = kept_keys.iter().any(|(p, a, t)| {
            p == &key_proj
                && a == &key_agent
                && match (row_secs, *t) {
                    (Some(a_secs), Some(b_secs)) => {
                        (a_secs - b_secs).abs() <= CLOSED_DEDUP_WINDOW_SECS
                    }
                    // Without a parseable timestamp on either side we
                    // can't bound the cluster, so we don't dedup.
                    _ => false,
                }
        });
        if is_dup {
            continue;
        }
        kept_keys.push((key_proj, key_agent, row_secs));
        deduped.push(row);
        if deduped.len() >= max_closed {
            break;
        }
    }

    live.extend(deduped);
    live
}

/// Sort an Overview row list in display order:
///
/// 1. Live rows first (live before closed),
/// 2. within live: newest `last_opened` first (missing values sort last),
/// 3. within closed: newest `closed_at` first (matches
///    [`crate::SessionManager::closed`]).
///
/// Mirrors the visual reading order of the C# Overview's
/// `FilteredCards` — active sessions read top-to-bottom, closed
/// history trails below.
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
    use crate::projects::{Project, Session, Worktree};

    fn mk_project(id: &str, name: &str, sessions: Vec<Session>) -> Project {
        Project {
            id: id.into(),
            name: name.into(),
            path: format!("C:\\dev\\{name}"),
            default_branch: "main".into(),
            worktree_root: None,
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
    fn live_rows_sort_before_closed_rows() {
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project(
                "p1",
                "alpha",
                vec![
                    // closed but recent
                    mk_session("closed_recent", None, Some("2026-05-11T12:30:00Z")),
                    // live but stale
                    mk_session("live_old", Some("2026-05-01T08:00:00Z"), None),
                ],
            )],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows[0].session_id, "live_old"); // live always before closed
        assert_eq!(rows[0].lifecycle, OverviewLifecycle::Live);
        assert_eq!(rows[1].session_id, "closed_recent");
        assert_eq!(rows[1].lifecycle, OverviewLifecycle::Closed);
    }

    #[test]
    fn closed_rows_sort_newest_first() {
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project(
                "p1",
                "alpha",
                vec![
                    mk_session("oldest", None, Some("2026-05-09T09:00:00Z")),
                    mk_session("newest", None, Some("2026-05-11T09:00:00Z")),
                    mk_session("middle", None, Some("2026-05-10T09:00:00Z")),
                ],
            )],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows[0].session_id, "newest");
        assert_eq!(rows[1].session_id, "middle");
        assert_eq!(rows[2].session_id, "oldest");
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
    fn branch_label_falls_back_to_worktree_id() {
        let mut s = mk_session("s1", None, None);
        s.branch = None;
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project("p1", "alpha", vec![s])],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows[0].branch_label, "primary");
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

    #[test]
    fn build_rows_caps_closed_at_default_limit() {
        // 25 closed sessions, one project — only the newest 20 survive
        // and the live row is always retained even though it'd sort
        // below all the recent closed rows by raw timestamp (live
        // rows have a separate sort domain).
        let mut sessions = Vec::new();
        sessions.push(mk_session("live", Some("2026-01-01T00:00:00Z"), None));
        for i in 0..25 {
            // Stagger closed_at stamps an hour apart so the dedup
            // window can't fold them together, and vary the agent
            // slightly per row so the (project, agent) dedup key is
            // unique.
            let stamp = format!("2026-05-11T{:02}:00:00Z", i);
            let mut s = mk_session(&format!("c{i}"), None, Some(&stamp));
            s.agent_id = Some(format!("agent-{i}"));
            sessions.push(s);
        }
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project("p1", "alpha", sessions)],
        };

        let rows = build_rows(&cfg);
        let live_count = rows
            .iter()
            .filter(|r| r.lifecycle == OverviewLifecycle::Live)
            .count();
        let closed_count = rows
            .iter()
            .filter(|r| r.lifecycle == OverviewLifecycle::Closed)
            .count();
        assert_eq!(live_count, 1, "live row must always be retained");
        assert_eq!(
            closed_count, DEFAULT_MAX_CLOSED_ROWS,
            "closed cap honoured"
        );
    }

    #[test]
    fn build_rows_under_cap_returns_everything() {
        let mut sessions = Vec::new();
        for i in 0..5 {
            let stamp = format!("2026-05-11T{:02}:00:00Z", i);
            let mut s = mk_session(&format!("c{i}"), None, Some(&stamp));
            s.agent_id = Some(format!("agent-{i}"));
            sessions.push(s);
        }
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project("p1", "alpha", sessions)],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows.len(), 5);
    }

    #[test]
    fn build_rows_capped_keeps_newest_first_when_over_cap() {
        // 5 closed rows, cap to 2 — must keep the two newest by
        // closed_at, in newest-first order.
        let sessions = vec![
            {
                let mut s = mk_session("oldest", None, Some("2026-05-01T00:00:00Z"));
                s.agent_id = Some("a".into());
                s
            },
            {
                let mut s = mk_session("mid1", None, Some("2026-05-02T00:00:00Z"));
                s.agent_id = Some("b".into());
                s
            },
            {
                let mut s = mk_session("mid2", None, Some("2026-05-03T00:00:00Z"));
                s.agent_id = Some("c".into());
                s
            },
            {
                let mut s = mk_session("newer", None, Some("2026-05-04T00:00:00Z"));
                s.agent_id = Some("d".into());
                s
            },
            {
                let mut s = mk_session("newest", None, Some("2026-05-05T00:00:00Z"));
                s.agent_id = Some("e".into());
                s
            },
        ];
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project("p1", "alpha", sessions)],
        };

        let rows = build_rows_capped(&cfg, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].session_id, "newest");
        assert_eq!(rows[1].session_id, "newer");
    }

    #[test]
    fn build_rows_dedups_near_duplicate_closed_rows() {
        // Three closed rows for the same (project, agent) clustered
        // within ~4 minutes of each other plus one fully separate row
        // an hour later. Dedup must collapse the cluster to the
        // newest entry; the separated row stays.
        let sessions = vec![
            mk_session("crash1", None, Some("2026-05-11T10:00:00Z")),
            mk_session("crash2", None, Some("2026-05-11T10:02:00Z")),
            mk_session("crash3", None, Some("2026-05-11T10:04:00Z")),
            mk_session("separate", None, Some("2026-05-11T11:00:00Z")),
        ];
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project("p1", "alpha", sessions)],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows.len(), 2, "near-duplicate cluster collapsed to newest");
        assert_eq!(rows[0].session_id, "separate");
        assert_eq!(rows[1].session_id, "crash3");
    }

    #[test]
    fn build_rows_dedup_does_not_collapse_different_agents() {
        // Same project + same closed_at, different agents → not a
        // duplicate, both rows survive.
        let mut s1 = mk_session("a_row", None, Some("2026-05-11T10:00:00Z"));
        s1.agent_id = Some("claude".into());
        let mut s2 = mk_session("b_row", None, Some("2026-05-11T10:00:00Z"));
        s2.agent_id = Some("codex".into());
        let cfg = ProjectsConfig {
            version: 1,
            agents: vec![],
            projects: vec![mk_project("p1", "alpha", vec![s1, s2])],
        };

        let rows = build_rows(&cfg);
        assert_eq!(rows.len(), 2);
    }
}
