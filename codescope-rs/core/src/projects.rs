//! Project / Worktree / Session models — Rust port of
//! `src/CodeScope.Core/Models/{Project,Session,Worktree,ProjectsConfig}.cs`.
//!
//! Field names match the C# build's JSON 1:1 so a `projects.json`
//! written by the v0.x C# binary can be read by this build (and vice
//! versa, once the Rust port stabilises). The schema is deliberately
//! tolerant: every field except the bare minimum is optional in serde,
//! so partial files survive and forward-compat fields don't break us.
//!
//! Identifiers are stable strings (typically UUIDs in the C# build but
//! we don't enforce a format — anything unique is fine). The `id`
//! belongs to the project / worktree / session, not its disk path,
//! because users can rename or relocate a worktree without breaking
//! cross-references.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;

/// Latest schema version we write. Bump when the on-disk shape needs
/// a non-additive migration. Additive changes (new optional fields)
/// stay at the same version.
pub const CURRENT_VERSION: u32 = 1;

/// One git repository as it appears in the sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// Stable id used by sessions to refer to this project.
    pub id: String,
    /// Display name in the sidebar. Usually the folder leaf.
    pub name: String,
    /// Absolute path to the primary working tree.
    pub path: String,
    /// Default branch — used as the base when creating new worktrees.
    #[serde(default = "default_branch", alias = "default_branch")]
    pub default_branch: String,
    /// Where new worktrees go. `None` = `"{path}.worktrees"`.
    #[serde(default, alias = "worktree_root")]
    pub worktree_root: Option<String>,
    /// Per-project agent override. `None` = use the global default.
    #[serde(default, alias = "default_agent_id")]
    pub default_agent_id: Option<String>,
    /// Sessions persisted across restarts.
    #[serde(default)]
    pub sessions: Vec<Session>,
    /// Tracked worktrees. The primary worktree at `path` is implicit.
    #[serde(default)]
    pub worktrees: Vec<Worktree>,
}

fn default_branch() -> String { "main".to_string() }

impl Project {
    /// Build a fresh project pointing at `path`. Name defaults to the
    /// folder leaf (or `"project"` if the path has no leaf for some
    /// reason), id is a fresh UUIDv4. The default branch is `"main"`
    /// — callers can override by mutating the returned struct before
    /// saving. No validation that `path` is actually a git repo: the
    /// sidebar lists everything the user adds; bad paths surface when
    /// the worktree code tries to use them.
    pub fn new(path: String) -> Self {
        let name = std::path::Path::new(&path)
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "project".to_string());
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            path,
            default_branch: default_branch(),
            worktree_root: None,
            default_agent_id: None,
            sessions: Vec::new(),
            worktrees: Vec::new(),
        }
    }

    /// Effective worktree root for this project. Mirrors the C# rule
    /// in `NewWorktreeDialog`: explicit `worktree_root` wins, otherwise
    /// fall back to `"{path}.worktrees"` next to the primary tree.
    pub fn worktree_root_path(&self) -> String {
        self.worktree_root
            .clone()
            .unwrap_or_else(|| format!("{}.worktrees", self.path))
    }
}

/// One git worktree under a [`Project`]. Every project has an
/// implicit primary worktree at `Project::path`; additional ones live
/// here and get `is_primary = false`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Worktree {
    pub id: String,
    pub path: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default, alias = "is_primary")]
    pub is_primary: bool,
}

/// A persisted tab. Live pty / process state lives in the runtime
/// session manager, not here — this is the "what should be restored
/// at launch" record.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub id: String,
    #[serde(alias = "worktree_path")]
    pub worktree_path: String,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default, alias = "agent_id")]
    pub agent_id: Option<String>,
    #[serde(default, alias = "display_name")]
    pub display_name: Option<String>,
    #[serde(default, alias = "worktree_id")]
    pub worktree_id: Option<String>,
    /// ISO 8601 UTC.
    #[serde(default, alias = "last_opened")]
    pub last_opened: Option<String>,
    #[serde(default, alias = "agent_session_id")]
    pub agent_session_id: Option<String>,
    #[serde(default, alias = "closed_at")]
    pub closed_at: Option<String>,
}

/// Root object persisted to `%APPDATA%\CodeScope\projects.json`.
///
/// `agents` is round-tripped opaquely via [`serde_json::Value`]: the C#
/// build persists per-user `AgentProfile` overrides here, and the Rust
/// port doesn't consume them yet but must not erase them. Once the
/// Rust port grows its own agent registry the field can be replaced
/// with a typed `Vec<AgentProfile>` mirror — until then "preserve
/// what's there, write back what was read" is the safe behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectsConfig {
    pub version: u32,
    pub agents: Vec<serde_json::Value>,
    pub projects: Vec<Project>,
}

impl Default for ProjectsConfig {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            agents: Vec::new(),
            projects: Vec::new(),
        }
    }
}

impl ProjectsConfig {
    pub fn load(paths: &AppPaths) -> Result<Self> {
        Self::load_from(&paths.projects_file())
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
        self.save_to(&paths.projects_file())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut json = serde_json::to_string_pretty(self)
            .context("serialise projects.json")?;
        json.push('\n');
        std::fs::write(path, json)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    /// Look up a project by its stable id. `None` if unknown.
    pub fn project(&self, id: &str) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_empty_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        let cfg = ProjectsConfig::load_from(&path).unwrap();
        assert_eq!(cfg.version, CURRENT_VERSION);
        assert!(cfg.projects.is_empty());
    }

    #[test]
    fn round_trip_preserves_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        let cfg = ProjectsConfig {
            version: CURRENT_VERSION,
            agents: Vec::new(),
            projects: vec![Project {
                id: "p1".into(),
                name: "Repo".into(),
                path: "C:\\repos\\repo".into(),
                default_branch: "main".into(),
                worktree_root: Some("C:\\repos\\repo.worktrees".into()),
                default_agent_id: Some("claude-code".into()),
                sessions: vec![Session {
                    id: "s1".into(),
                    worktree_path: "C:\\repos\\repo".into(),
                    branch: Some("main".into()),
                    agent_id: Some("claude-code".into()),
                    display_name: None,
                    worktree_id: Some("w-primary".into()),
                    last_opened: None,
                    agent_session_id: None,
                    closed_at: None,
                }],
                worktrees: vec![Worktree {
                    id: "w-primary".into(),
                    path: "C:\\repos\\repo".into(),
                    branch: Some("main".into()),
                    is_primary: true,
                }],
            }],
        };
        cfg.save_to(&path).unwrap();
        let loaded = ProjectsConfig::load_from(&path).unwrap();
        assert_eq!(loaded.projects.len(), 1);
        let p = &loaded.projects[0];
        assert_eq!(p.id, "p1");
        assert_eq!(p.sessions.len(), 1);
        assert_eq!(p.worktrees.len(), 1);
        assert!(p.worktrees[0].is_primary);
    }

    #[test]
    fn new_project_uses_folder_leaf_as_name() {
        let p = Project::new("/home/me/codescope".into());
        assert_eq!(p.name, "codescope");
        assert_eq!(p.default_branch, "main");
        // UUIDv4 is 36 chars including the dashes.
        assert_eq!(p.id.len(), 36);
    }

    #[test]
    fn new_project_falls_back_when_path_has_no_leaf() {
        let p = Project::new("/".into());
        assert_eq!(p.name, "project");
    }

    #[test]
    fn forward_compatible_unknown_field_survives_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        std::fs::write(
            &path,
            r#"{ "version": 1, "projects": [], "future_field": "ok" }"#,
        )
        .unwrap();
        // Unknown root-level fields are tolerated thanks to serde's
        // default behaviour (no `deny_unknown_fields`).
        let cfg = ProjectsConfig::load_from(&path).unwrap();
        assert_eq!(cfg.version, 1);
    }

    /// Real-shape fixture matching what the C# build's `ProjectStore`
    /// writes (camelCase keys, top-level `agents`, nested
    /// `defaultBranch` / `worktreePath` / `isPrimary` / etc.). If this
    /// stops parsing cleanly the data-loss footgun from session 33 is
    /// back: the Rust port would default to an empty config and the
    /// next mutation would overwrite the user's file.
    #[test]
    fn loads_csharp_shape_fixture_without_data_loss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        let csharp_json = r#"{
          "version": 1,
          "agents": [
            { "id": "claude", "displayName": "Claude Code", "command": "claude", "isDefault": true }
          ],
          "projects": [
            {
              "id": "p1",
              "name": "Repo",
              "path": "C:\\repos\\repo",
              "defaultBranch": "main",
              "worktreeRoot": "C:\\repos\\repo.worktrees",
              "defaultAgentId": "claude",
              "sessions": [
                {
                  "id": "s1",
                  "worktreePath": "C:\\repos\\repo",
                  "branch": "main",
                  "agentId": "claude",
                  "worktreeId": "primary",
                  "agentSessionId": "abc-123",
                  "lastOpened": "2026-05-09T10:00:00+00:00"
                }
              ],
              "worktrees": [
                { "id": "primary", "path": "C:\\repos\\repo", "branch": "main", "isPrimary": true },
                { "id": "feat-x",  "path": "C:\\repos\\repo.worktrees\\feat-x", "branch": "feat/x", "isPrimary": false }
              ]
            }
          ]
        }"#;
        std::fs::write(&path, csharp_json).unwrap();

        let cfg = ProjectsConfig::load_from(&path).unwrap();
        assert_eq!(cfg.version, 1);
        assert_eq!(cfg.agents.len(), 1, "agent overrides must survive load");
        let p = &cfg.projects[0];
        assert_eq!(p.default_branch, "main");
        assert_eq!(p.worktree_root.as_deref(), Some("C:\\repos\\repo.worktrees"));
        assert_eq!(p.default_agent_id.as_deref(), Some("claude"));
        assert_eq!(p.sessions.len(), 1);
        let s = &p.sessions[0];
        assert_eq!(s.worktree_path, "C:\\repos\\repo");
        assert_eq!(s.agent_id.as_deref(), Some("claude"));
        assert_eq!(s.worktree_id.as_deref(), Some("primary"));
        assert_eq!(s.agent_session_id.as_deref(), Some("abc-123"));
        assert!(s.last_opened.is_some());
        assert_eq!(p.worktrees.len(), 2);
        assert!(p.worktrees[0].is_primary);
        assert!(!p.worktrees[1].is_primary);
    }

    #[test]
    fn save_uses_camelcase_keys_matching_csharp_build() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        let cfg = ProjectsConfig {
            version: CURRENT_VERSION,
            agents: Vec::new(),
            projects: vec![Project {
                id: "p1".into(),
                name: "Repo".into(),
                path: "C:\\repo".into(),
                default_branch: "main".into(),
                worktree_root: Some("C:\\repo.worktrees".into()),
                default_agent_id: Some("claude".into()),
                sessions: vec![Session {
                    id: "s1".into(),
                    worktree_path: "C:\\repo".into(),
                    branch: None,
                    agent_id: None,
                    display_name: None,
                    worktree_id: Some("primary".into()),
                    last_opened: None,
                    agent_session_id: None,
                    closed_at: None,
                }],
                worktrees: vec![Worktree {
                    id: "primary".into(),
                    path: "C:\\repo".into(),
                    branch: None,
                    is_primary: true,
                }],
            }],
        };
        cfg.save_to(&path).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        // Spot-check the keys that diverge between snake_case (what
        // serde writes by default) and the C# build's camelCase. If
        // any of these flip back to snake_case we'd silently fail to
        // round-trip with the installed C# binary.
        assert!(written.contains("\"defaultBranch\""), "{written}");
        assert!(written.contains("\"worktreeRoot\""), "{written}");
        assert!(written.contains("\"defaultAgentId\""), "{written}");
        assert!(written.contains("\"worktreePath\""), "{written}");
        assert!(written.contains("\"worktreeId\""), "{written}");
        assert!(written.contains("\"isPrimary\""), "{written}");
        assert!(!written.contains("\"default_branch\""), "{written}");
        assert!(!written.contains("\"worktree_path\""), "{written}");
    }

    /// Pre-PR-58 the Rust port wrote snake_case keys. A `projects.json`
    /// produced by that older binary must still load cleanly after
    /// flipping to camelCase, otherwise users running the dev build
    /// would see all renamed fields silently default to their zero
    /// values and the next save would overwrite the file with the
    /// defaults — exactly the data-loss footgun this PR is closing.
    /// `#[serde(alias = "...")]` on each renamed field accepts both
    /// shapes during deserialization; saves still write camelCase.
    #[test]
    fn loads_legacy_snake_case_fixture_without_data_loss() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        let snake_json = r#"{
          "version": 1,
          "projects": [
            {
              "id": "p1",
              "name": "Repo",
              "path": "C:\\repos\\repo",
              "default_branch": "develop",
              "worktree_root": "C:\\repos\\repo.worktrees",
              "default_agent_id": "claude",
              "sessions": [
                {
                  "id": "s1",
                  "worktree_path": "C:\\repos\\repo",
                  "branch": "develop",
                  "agent_id": "claude",
                  "display_name": "main shell",
                  "worktree_id": "primary",
                  "last_opened": "2026-04-01T10:00:00+00:00",
                  "agent_session_id": "abc-123",
                  "closed_at": null
                }
              ],
              "worktrees": [
                { "id": "primary", "path": "C:\\repos\\repo", "branch": "develop", "is_primary": true }
              ]
            }
          ]
        }"#;
        std::fs::write(&path, snake_json).unwrap();

        let cfg = ProjectsConfig::load_from(&path).unwrap();
        let p = &cfg.projects[0];
        assert_eq!(p.default_branch, "develop", "default_branch must survive");
        assert_eq!(
            p.worktree_root.as_deref(),
            Some("C:\\repos\\repo.worktrees"),
            "worktree_root must survive"
        );
        assert_eq!(p.default_agent_id.as_deref(), Some("claude"));
        let s = &p.sessions[0];
        assert_eq!(s.worktree_path, "C:\\repos\\repo", "worktree_path must survive");
        assert_eq!(s.agent_id.as_deref(), Some("claude"));
        assert_eq!(s.display_name.as_deref(), Some("main shell"));
        assert_eq!(s.worktree_id.as_deref(), Some("primary"));
        assert_eq!(s.last_opened.as_deref(), Some("2026-04-01T10:00:00+00:00"));
        assert_eq!(s.agent_session_id.as_deref(), Some("abc-123"));
        assert!(p.worktrees[0].is_primary, "is_primary must survive");
    }

    /// Load a file containing agent overrides → mutate projects → save
    /// → reload. The agent array must come back identical. This is the
    /// regression net for the data-loss class from session 33.
    #[test]
    fn round_trip_preserves_unknown_agents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        std::fs::write(
            &path,
            r#"{
              "version": 1,
              "agents": [
                { "id": "claude", "displayName": "Claude Code", "command": "claude" },
                { "id": "codex",  "displayName": "Codex",       "command": "codex" }
              ],
              "projects": []
            }"#,
        )
        .unwrap();

        let mut cfg = ProjectsConfig::load_from(&path).unwrap();
        assert_eq!(cfg.agents.len(), 2);
        cfg.projects.push(Project::new("C:\\new".into()));
        cfg.save_to(&path).unwrap();

        let reloaded = ProjectsConfig::load_from(&path).unwrap();
        assert_eq!(reloaded.agents.len(), 2);
        assert_eq!(reloaded.agents[0]["id"], "claude");
        assert_eq!(reloaded.agents[1]["id"], "codex");
        assert_eq!(reloaded.projects.len(), 1);
    }
}
