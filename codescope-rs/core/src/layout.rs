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
    /// Open tabs to rehydrate at next launch. Each entry binds a
    /// terminal session to a group + active flag. We don't try to
    /// restore the *running process* (the pty was killed at app
    /// shutdown) — `auto_type` re-runs whatever command was used to
    /// spawn the original tab so "New Claude session" comes back as
    /// claude and plain shells come back as plain shells. Tabs
    /// whose `working_directory` no longer exists are silently
    /// dropped on rehydrate. Empty → no tabs at last save (fresh
    /// install / migration from older layout.json), AppShell falls
    /// back to a single cold-start tab.
    pub open_tabs: Vec<RestoreTab>,
}

/// One tab worth of restore metadata. Light enough to round-trip
/// cleanly — full C# `Session` parity (agent_session_id,
/// last_opened timestamps, history bookkeeping) is out of scope
/// until the projects.json side moves over.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RestoreTab {
    /// Working directory the pty was spawned in. Missing this and
    /// the user wakes up to a tab that ran in a different folder
    /// than they remember.
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
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            sidebar_visible: true,
            sidebar_width: 240.0,
            selected_project_id: None,
            group_weights: Vec::new(),
            focused_group_index: 0,
            open_tabs: Vec::new(),
        }
    }
}

impl LayoutState {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        Self::load_from(&paths.layout_file())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) if bytes.is_empty() => Ok(Self::default()),
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
        }
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
            open_tabs: vec![
                RestoreTab {
                    working_directory: "C:\\repos\\foo".into(),
                    title: "foo · main".into(),
                    auto_type: None,
                    group_index: 0,
                    active_in_group: true,
                },
                RestoreTab {
                    working_directory: "C:\\repos\\foo.worktrees\\branch-x".into(),
                    title: "foo · branch-x · claude".into(),
                    auto_type: Some("claude".into()),
                    group_index: 1,
                    active_in_group: true,
                },
            ],
        };
        state.save_to(&path).unwrap();
        let loaded = LayoutState::load_from(&path).unwrap();
        assert!(!loaded.sidebar_visible);
        assert_eq!(loaded.selected_project_id.as_deref(), Some("proj-1"));
        assert_eq!(loaded.group_weights, vec![1.5, 1.0]);
        assert_eq!(loaded.focused_group_index, 1);
        assert_eq!(loaded.open_tabs.len(), 2);
        assert_eq!(loaded.open_tabs[1].auto_type.as_deref(), Some("claude"));
        assert!(loaded.open_tabs[0].active_in_group);
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
