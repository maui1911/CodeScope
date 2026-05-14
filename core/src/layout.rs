//! Persisted UI layout state.
//!
//! Captures _UI_ state that should survive an app restart but isn't
//! either user config (`settings.json`) or business data
//! (`projects.json`). Things that go here:
//!
//! * sidebar visibility and width
//! * which project is currently selected
//! * (later) open tab list, active tab index
//!
//! Distinct from `window.json` because the user might want layout
//! to roam between machines (Dropbox the `%APPDATA%` folder, …)
//! while window coordinates stay local.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutState {
    pub sidebar_visible: bool,
    pub sidebar_width: f32,
    /// Stable id of the project last opened. Sidebar selects this on
    /// startup if the project still exists.
    pub selected_project_id: Option<String>,
    /// Per-group flex weights for the work area. Length matches the
    /// number of groups; restored on launch so drag-resized columns
    /// keep their proportions across restarts. Mirrors C#'s
    /// `WorkspaceLayout.GroupWidths`. Empty / mismatched → AppShell
    /// falls back to equal weights.
    pub group_weights: Vec<f32>,
    /// Index of the focused group at last save. Restored when the
    /// group count matches; out-of-range falls back to 0. Mirrors
    /// `WorkspaceLayout.FocusedGroupIndex`.
    pub focused_group_index: usize,
    /// Placement metadata for the live sessions persisted in
    /// `projects.json`. Each entry binds a `Session.id` to the group
    /// it should rehydrate into. The authoritative list of *which*
    /// sessions are open lives in `projects.json` (rows with
    /// `closed_at = None`); this field only decides where each one
    /// lands. Mirrors C# `LayoutStore.Layout.SessionToGroup` plus an
    /// extra `active_in_group` flag because Rust has no per-group
    /// "active tab" map elsewhere.
    ///
    /// Missing entries fall back to group 0 + non-active on
    /// rehydrate. Stale entries (no matching live session) are
    /// silently ignored. Empty → fresh install / pre-migration
    /// layout.json; the legacy `open_tabs` field below feeds the
    /// one-shot upgrade in [`LayoutState::migrate`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_placements: Vec<SessionPlacement>,
    /// Legacy "open tabs" snapshot — Rust port's pre-parity bag of
    /// (working_directory, title, auto_type, session_id, …) per
    /// tab. New writes leave this empty (it's never serialised when
    /// `session_placements` is the source); old writes are read
    /// once and converted to [`session_placements`] via
    /// [`LayoutState::migrate`] so users upgrading don't see their
    /// tab layout shuffle. Deserialise-only after the migration
    /// step runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub open_tabs: Vec<RestoreTab>,
    /// Project ids the user has collapsed in the sidebar tree.
    /// Restored on launch so chevron state survives a restart;
    /// project ids not in this list render expanded (the default).
    /// Stored as a `Vec<String>` rather than the in-memory
    /// `HashSet` so the on-disk JSON is a stable, ordered array —
    /// nicer to diff and hand-edit. Sidebar prunes ids that no
    /// longer match a known project before saving so the file
    /// can't grow stale entries across many sessions.
    pub collapsed_projects: Vec<String>,
}

/// Where a single live session rehydrates on next launch. The
/// session row itself lives in `projects.json`; this just tells the
/// AppShell which group the matching tab should land in and whether
/// it was the active tab in its group at save time. Mirrors C#
/// `LayoutStore.Layout.SessionToGroup` plus an active-tab marker.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionPlacement {
    /// `Session.id` from `projects.json`. Stale ids (no matching live
    /// session at rehydrate time) are silently ignored.
    pub session_id: String,
    /// Group index this tab belongs to in `group_weights`'s
    /// numbering. Restore clamps out-of-range values to 0.
    pub group_index: usize,
    /// Was this tab the active one in its group at save time?
    /// Restore picks the first `true` per group; if none, the first
    /// spawned tab in the group wins.
    #[serde(default)]
    pub active_in_group: bool,
}

/// One tab worth of restore metadata. Light enough to round-trip
/// cleanly — full C# `Session` parity (agent_session_id,
/// last_opened timestamps, history bookkeeping) is out of scope
/// until the projects.json side moves over.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestoreTab {
    /// Working directory the pty was spawned in.
    pub working_directory: String,
    /// Tab strip label. Free-form string the user originally saw —
    /// usually `{project} · {branch}` or `{project} · {branch} · claude`.
    pub title: String,
    /// Optional command that was auto-typed at the prompt when this
    /// tab spawned. `Some("claude")` for "New Claude session" rows;
    /// `None` for plain shells. Re-runs at next launch so the agent
    /// is back even though the pty itself is fresh.
    #[serde(default)]
    pub auto_type: Option<String>,
    /// Index of the group this tab belongs to in `group_weights`'s
    /// numbering. Restore clamps out-of-range values to 0.
    pub group_index: usize,
    /// Was this tab the active one in its group at save time?
    /// Restore picks the first `true` per group; if none, the first
    /// tab wins.
    #[serde(default)]
    pub active_in_group: bool,
    /// Persisted [`crate::projects::Session`] id this tab was bound
    /// to at save time. `Some` lets the rehydrate path look up the
    /// stored `agent_session_id` and reattach to the *specific*
    /// conversation via `resume_by_id_args` instead of the agent's
    /// "most recent" fallback. `None` for older layout.json files
    /// (pre-resume-by-id) and for tabs that never made it into the
    /// session store (no project context at spawn time) — those
    /// rehydrate as a fresh agent launch, matching the previous
    /// behaviour.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            sidebar_visible: true,
            sidebar_width: 240.0,
            selected_project_id: None,
            group_weights: Vec::new(),
            focused_group_index: 0,
            session_placements: Vec::new(),
            open_tabs: Vec::new(),
            collapsed_projects: Vec::new(),
        }
    }
}

impl LayoutState {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        Self::load_from(&paths.layout_file())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let mut state: Self = match std::fs::read(path) {
            Ok(bytes) if bytes.is_empty() => Self::default(),
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        state.migrate();
        Ok(state)
    }

    /// Convert a legacy `open_tabs` snapshot into the slim
    /// `session_placements` form. Idempotent: a no-op once
    /// `session_placements` is populated (post-migration writes never
    /// emit `open_tabs` again). Legacy entries without `session_id`
    /// are dropped — they have no `projects.json` row to bind to and
    /// the new rehydrate path drives off live sessions, not free-
    /// floating descriptors.
    ///
    /// Runs unconditionally on load so old layout.json files upgrade
    /// in place. The migration explicitly does NOT touch
    /// `session_placements` when it's already non-empty so a manually
    /// edited file with both fields stays under user control.
    fn migrate(&mut self) {
        if !self.session_placements.is_empty() {
            // New shape already on disk; drop any legacy carry-over
            // so the next save doesn't keep emitting it.
            self.open_tabs.clear();
            return;
        }
        if self.open_tabs.is_empty() {
            return;
        }
        let migrated: Vec<SessionPlacement> = std::mem::take(&mut self.open_tabs)
            .into_iter()
            .filter_map(|tab| {
                let session_id = tab.session_id?;
                Some(SessionPlacement {
                    session_id,
                    group_index: tab.group_index,
                    active_in_group: tab.active_in_group,
                })
            })
            .collect();
        self.session_placements = migrated;
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        paths.ensure_dirs()?;
        self.save_to(&paths.layout_file())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut json = serde_json::to_string_pretty(self)
            .context("serialise layout state")?;
        json.push('\n');
        std::fs::write(path, json)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        let state = LayoutState::load_from(&path).unwrap();
        assert!(state.sidebar_visible);
        assert_eq!(state.sidebar_width, 240.0);
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        let state = LayoutState {
            sidebar_visible: false,
            sidebar_width: 320.0,
            selected_project_id: Some("proj-1".into()),
            group_weights: vec![1.5, 1.0],
            focused_group_index: 1,
            session_placements: vec![SessionPlacement {
                session_id: "sess-1".into(),
                group_index: 0,
                active_in_group: true,
            }],
            open_tabs: Vec::new(),
            collapsed_projects: vec!["proj-2".into(), "proj-3".into()],
        };
        state.save_to(&path).unwrap();
        let loaded = LayoutState::load_from(&path).unwrap();
        assert!(!loaded.sidebar_visible);
        assert_eq!(loaded.selected_project_id.as_deref(), Some("proj-1"));
        assert_eq!(loaded.group_weights, vec![1.5, 1.0]);
        assert_eq!(loaded.focused_group_index, 1);
        assert_eq!(loaded.session_placements.len(), 1);
        assert_eq!(loaded.session_placements[0].session_id, "sess-1");
        assert!(loaded.session_placements[0].active_in_group);
        assert!(loaded.open_tabs.is_empty());
        assert_eq!(loaded.collapsed_projects, vec!["proj-2", "proj-3"]);
    }

    #[test]
    fn save_omits_empty_legacy_open_tabs() {
        // A migrated `LayoutState` has `open_tabs = []`. Serialising it
        // must not emit an `"open_tabs": []` key; otherwise every save
        // would write a redundant empty array and a second `cargo run`
        // would still load (harmlessly) the legacy migration code path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        let state = LayoutState {
            session_placements: vec![SessionPlacement {
                session_id: "sess-1".into(),
                group_index: 0,
                active_in_group: false,
            }],
            ..LayoutState::default()
        };
        state.save_to(&path).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("open_tabs"), "expected open_tabs key to be omitted: {raw}");
        assert!(raw.contains("session_placements"), "{raw}");
    }

    #[test]
    fn migrate_legacy_open_tabs_to_session_placements() {
        // A layout.json written by a pre-stap-2 Rust build carries the
        // wide `open_tabs` shape with `session_id` already present (the
        // resume-by-id work). On load we drop everything except the
        // (session_id, group_index, active_in_group) triple — the rest
        // can be derived from the projects.json Session row at
        // rehydrate time.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        std::fs::write(
            &path,
            r#"{
              "sidebar_visible": true,
              "sidebar_width": 240,
              "open_tabs": [
                {
                  "working_directory": "C:\\repos\\foo",
                  "title": "foo · main · claude",
                  "auto_type": "claude",
                  "group_index": 1,
                  "active_in_group": true,
                  "session_id": "sess-1"
                },
                {
                  "working_directory": "C:\\repos\\bar",
                  "title": "bar",
                  "group_index": 0,
                  "active_in_group": false,
                  "session_id": "sess-2"
                }
              ]
            }"#,
        )
        .unwrap();
        let loaded = LayoutState::load_from(&path).unwrap();
        assert_eq!(loaded.session_placements.len(), 2);
        assert_eq!(loaded.session_placements[0].session_id, "sess-1");
        assert_eq!(loaded.session_placements[0].group_index, 1);
        assert!(loaded.session_placements[0].active_in_group);
        assert_eq!(loaded.session_placements[1].session_id, "sess-2");
        assert!(!loaded.session_placements[1].active_in_group);
        // open_tabs cleared so the next save doesn't keep round-tripping
        // the legacy shape.
        assert!(loaded.open_tabs.is_empty());
    }

    #[test]
    fn migrate_drops_legacy_entries_without_session_id() {
        // Pre-resume-by-id Rust builds wrote `RestoreTab` entries
        // without a `session_id`. Those have no projects.json row to
        // bind to and the new rehydrate path drives off live sessions,
        // so they're silently dropped on migration. The user gets a
        // slightly smaller tab strip on first launch after upgrade,
        // not stale ghost tabs.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        std::fs::write(
            &path,
            r#"{
              "open_tabs": [
                {
                  "working_directory": "C:\\repos\\foo",
                  "title": "foo",
                  "group_index": 0,
                  "active_in_group": true
                },
                {
                  "working_directory": "C:\\repos\\bar",
                  "title": "bar",
                  "group_index": 0,
                  "active_in_group": false,
                  "session_id": "sess-keeper"
                }
              ]
            }"#,
        )
        .unwrap();
        let loaded = LayoutState::load_from(&path).unwrap();
        assert_eq!(loaded.session_placements.len(), 1);
        assert_eq!(loaded.session_placements[0].session_id, "sess-keeper");
    }

    #[test]
    fn migrate_is_noop_when_session_placements_already_present() {
        // A layout.json that already carries `session_placements` is
        // already on the new shape. `open_tabs`, if somehow present
        // (manual edit, partial revert), is dropped without
        // overwriting the authoritative `session_placements`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        std::fs::write(
            &path,
            r#"{
              "session_placements": [
                { "session_id": "sess-real", "group_index": 0, "active_in_group": true }
              ],
              "open_tabs": [
                { "working_directory": "C:\\stale", "title": "stale", "group_index": 0,
                  "active_in_group": false, "session_id": "sess-stale" }
              ]
            }"#,
        )
        .unwrap();
        let loaded = LayoutState::load_from(&path).unwrap();
        assert_eq!(loaded.session_placements.len(), 1);
        assert_eq!(loaded.session_placements[0].session_id, "sess-real");
        assert!(loaded.open_tabs.is_empty());
    }

    #[test]
    fn legacy_layout_without_collapsed_projects_loads() {
        // layout.json files written before sidebar collapse persistence
        // landed don't carry `collapsed_projects`. They must still load
        // and produce an empty list — the struct-level
        // `#[serde(default)]` on `LayoutState` backfills the missing
        // field with `Vec::new()` from `Default`.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        std::fs::write(
            &path,
            r#"{"sidebar_visible":true,"sidebar_width":240,"selected_project_id":"proj-x","group_weights":[],"focused_group_index":0,"open_tabs":[]}"#,
        )
        .unwrap();
        let loaded = LayoutState::load_from(&path).unwrap();
        assert_eq!(loaded.selected_project_id.as_deref(), Some("proj-x"));
        assert!(loaded.collapsed_projects.is_empty());
    }

    #[test]
    fn legacy_layout_without_group_fields_loads() {
        // Older layout.json files (pre-tab-groups) didn't carry
        // group_weights / focused_group_index. They must still load —
        // serde's `default` should fill in empty/zero.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("layout.json");
        std::fs::write(
            &path,
            r#"{"sidebar_visible":true,"sidebar_width":240,"selected_project_id":"proj-x"}"#,
        )
        .unwrap();
        let loaded = LayoutState::load_from(&path).unwrap();
        assert_eq!(loaded.selected_project_id.as_deref(), Some("proj-x"));
        assert!(loaded.group_weights.is_empty());
        assert_eq!(loaded.focused_group_index, 0);
    }
}
