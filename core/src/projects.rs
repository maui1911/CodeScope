//! Project / Worktree / Session models — Rust port of
//! `legacy:CodeScope.Core/Models/{Project,Session,Worktree,ProjectsConfig}.cs`.
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

/// What a sidebar project *is*. Almost everything is a `Local` git
/// checkout; `RemoteShell` is the escape hatch for "I work on a box
/// over SSH and just want a tab that runs `ssh host -t claude`" —
/// no path, no worktrees, no git pollers, no telemetry (#323).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectKind {
    /// A local directory (normally a git repository).
    #[default]
    Local,
    /// A saved command line, run in a fresh local shell with no
    /// working directory. The `Project::command` field carries it.
    RemoteShell,
}

/// One git repository (or remote-shell command) as it appears in the
/// sidebar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    /// Stable id used by sessions to refer to this project.
    pub id: String,
    /// Display name in the sidebar. Usually the folder leaf.
    pub name: String,
    /// Absolute path to the primary working tree. Empty for
    /// [`ProjectKind::RemoteShell`] projects.
    pub path: String,
    /// See [`ProjectKind`]. Missing in files written before #323 —
    /// those are all `Local`.
    #[serde(default)]
    pub kind: ProjectKind,
    /// Command line to run for [`ProjectKind::RemoteShell`] projects
    /// (e.g. `ssh dev`). `None` for local projects. When
    /// [`Self::remote_agent_id`] is set, the agent's launch command is
    /// appended as `<command> -t <agent>` so the tab lands straight in
    /// the agent on the far end.
    #[serde(default)]
    pub command: Option<String>,
    /// Agent profile id to launch on the remote for a
    /// [`ProjectKind::RemoteShell`] project (#323). `None` = run the
    /// command as-is (a bare remote shell). The id is resolved against
    /// the live `AgentRegistry` at spawn time, so an id whose profile
    /// no longer exists degrades gracefully to the raw command.
    #[serde(default)]
    pub remote_agent_id: Option<String>,
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
    /// Tracked worktrees. Includes the primary worktree at `path`
    /// (the one with `is_primary: true`) as an explicit row;
    /// `Project::new` seeds it on creation, and
    /// [`ProjectsConfig::migrate`] back-fills it for legacy configs
    /// that don't have one yet.
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
        // Seed a primary worktree row so the sidebar has a child row
        // to bind to immediately. Mirrors C# `SessionStore.AddProjectAsync`,
        // which synthesises the same primary entry — without it the
        // newly-added project renders as an empty shell. Branch is
        // left None; the dirty / git-status pollers fill it in on the
        // first tick.
        let primary = Worktree {
            id: "primary".to_owned(),
            path: path.clone(),
            branch: None,
            is_primary: true,
        };
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            path,
            kind: ProjectKind::Local,
            command: None,
            remote_agent_id: None,
            default_branch: default_branch(),
            worktree_root: None,
            default_agent_id: None,
            sessions: Vec::new(),
            worktrees: vec![primary],
        }
    }

    /// Build a [`ProjectKind::RemoteShell`] project: a named command
    /// line with no path and no worktrees, optionally launching
    /// `agent_id` on the far end. `name`/`command` are trimmed; the
    /// caller is expected to have rejected empty values already (see
    /// [`is_valid_remote_shell_command`]). `agent_id` `None` runs the
    /// command as a bare remote shell.
    pub fn new_remote_shell(name: String, command: String, agent_id: Option<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            name: name.trim().to_string(),
            path: String::new(),
            kind: ProjectKind::RemoteShell,
            command: Some(command.trim().to_string()),
            remote_agent_id: agent_id.filter(|s| !s.trim().is_empty()),
            default_branch: default_branch(),
            worktree_root: None,
            default_agent_id: None,
            sessions: Vec::new(),
            worktrees: Vec::new(),
        }
    }

    /// `true` for [`ProjectKind::RemoteShell`] projects. Callers use
    /// this to skip every path-shaped feature (git pollers, worktree
    /// menus, reveal-in-explorer, telemetry) — the only thing such a
    /// project can do is open a tab running its command.
    pub fn is_remote_shell(&self) -> bool {
        self.kind == ProjectKind::RemoteShell
    }

    /// The saved command for a remote-shell project, if it has one.
    /// `None` for local projects and for malformed rows (kind says
    /// remote but the command is missing or blank).
    pub fn remote_shell_command(&self) -> Option<&str> {
        if !self.is_remote_shell() {
            return None;
        }
        self.command
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
    }
}

/// Validation for the Add-project dialog's remote-shell mode: the
/// command must be non-blank and single-line. Anything else is the
/// user's business — we hand the string to `pwsh -Command` verbatim.
pub fn is_valid_remote_shell_command(command: &str) -> bool {
    let trimmed = command.trim();
    !trimmed.is_empty() && !trimmed.contains(['\n', '\r'])
}

/// Assemble the command a remote-shell tab actually runs (#323).
/// `command` is the user's base line (e.g. `ssh dev`); `agent_launch`
/// is the agent's own invocation on the far end (e.g. `claude`), or
/// `None` for a bare remote shell.
///
/// When an agent is present the result is
/// `<command> -t '$SHELL -lic "<agent>"'`. Three things matter here:
///
/// - `-t` forces a pty so the interactive CLI renders.
/// - `$SHELL -lic` runs the agent through the remote user's **login +
///   interactive** shell. A bare `ssh host -t claude` runs `claude`
///   in ssh's non-login command shell, whose `PATH` is the stock
///   `/usr/bin:/bin` — agents installed under `~/.local/bin`, nvm,
///   Homebrew, etc. (the common case) are not found and the launch
///   silently fails. Sourcing the login + rc files fixes the `PATH`.
///   `$SHELL` (not a hard-coded `bash`) generalises to zsh on macOS.
/// - The single-quote wrapper keeps `$SHELL` from being expanded
///   locally: it survives both the Windows `pwsh -Command "& { … }"`
///   boot and the macOS/Linux auto-type-into-login-shell path
///   unchanged, and only expands on the far end. The agent is
///   double-quoted inside so an agent invocation with args stays one
///   `-c` string.
///
/// Deliberately ssh-shaped — the agent field only makes sense for an
/// ssh (or ssh-like) base command, which is the whole point of a
/// remote-shell project. A blank `agent_launch` is treated as absent.
pub fn remote_command_with_agent(command: &str, agent_launch: Option<&str>) -> String {
    let base = command.trim();
    match agent_launch.map(str::trim).filter(|a| !a.is_empty()) {
        Some(agent) => format!("{base} -t '$SHELL -lic \"{agent}\"'"),
        None => base.to_string(),
    }
}

impl Project {
    /// Effective worktree root for this project. Mirrors the C# rule
    /// in `NewWorktreeDialog`: explicit `worktree_root` wins, otherwise
    /// fall back to `"{path}.worktrees"` next to the primary tree.
    pub fn worktree_root_path(&self) -> String {
        self.worktree_root
            .clone()
            .unwrap_or_else(|| format!("{}.worktrees", self.path))
    }

    /// Discover existing git worktrees under this project's primary
    /// `path` and adopt any that aren't already tracked in
    /// `self.worktrees`. Errors (path isn't a git repo, `git` missing,
    /// path doesn't exist on disk, etc.) are swallowed — the project
    /// still ends up added with just its primary row, same as before.
    ///
    /// Called once at project-add time so users who already have
    /// worktrees on disk see them in the sidebar immediately, without
    /// having to re-register each one through the "New worktree"
    /// dialog. Idempotent: re-running on an already-up-to-date
    /// project is a no-op via path-equality dedup.
    ///
    /// Primary worktree (the one git's porcelain marks first) is
    /// skipped — `Project::new` already seeded that row; the branch
    /// backfill is the `WorktreeStatusPoller`'s job, not ours.
    pub fn adopt_existing_worktrees(&mut self) {
        if self.is_remote_shell() {
            return;
        }
        let Ok(found) = crate::git::list_worktrees(Path::new(&self.path)) else {
            return;
        };
        for info in found {
            if info.is_primary {
                continue;
            }
            let needle = normalise_project_path(&info.path);
            if self
                .worktrees
                .iter()
                .any(|wt| normalise_project_path(&wt.path) == needle)
            {
                continue;
            }
            self.worktrees.push(Worktree {
                id: uuid::Uuid::new_v4().to_string(),
                path: info.path,
                branch: info.branch,
                is_primary: false,
            });
        }
    }
}

/// Rename a project's display name. Mirrors C# `SessionStore.RenameProjectAsync`
/// — trims whitespace, rejects an empty result, and no-ops when the trimmed
/// value matches the current name. Returns `Ok(true)` when the name actually
/// changed (callers should persist), `Ok(false)` for a no-op, and `Err` when
/// the id is unknown or the trimmed input is empty.
pub fn rename_project(
    cfg: &mut ProjectsConfig,
    project_id: &str,
    new_name: &str,
) -> Result<bool> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() {
        anyhow::bail!("project name cannot be empty");
    }
    let project = cfg
        .projects
        .iter_mut()
        .find(|p| p.id == project_id)
        .ok_or_else(|| anyhow::anyhow!("project '{project_id}' not found"))?;
    if project.name == trimmed {
        return Ok(false);
    }
    project.name = trimmed.to_string();
    Ok(true)
}

/// Update a remote-shell project's stored base command (#327). Trims
/// whitespace, validates through [`is_valid_remote_shell_command`]
/// (non-empty, single-line), and refuses non-remote-shell projects —
/// a local project's `command` slot is meaningless and writing it
/// would only confuse a later migration. Returns `Ok(true)` when the
/// command actually changed (callers should persist), `Ok(false)` for
/// a no-op, and `Err` for an unknown id, invalid command, or a
/// project of the wrong kind.
///
/// Tabs already running keep their old command — the edit applies to
/// the next session opened from the sidebar row, same as editing the
/// command by remove + re-add did before this existed.
pub fn set_remote_shell_command(
    cfg: &mut ProjectsConfig,
    project_id: &str,
    new_command: &str,
) -> Result<bool> {
    let trimmed = new_command.trim();
    // Split the two `is_valid_remote_shell_command` conditions so the
    // surfaced error names the actual problem — a multi-line paste
    // shouldn't be told it's "empty".
    if trimmed.is_empty() {
        anyhow::bail!("command cannot be empty");
    }
    if !is_valid_remote_shell_command(trimmed) {
        anyhow::bail!("command must be a single line");
    }
    let project = cfg
        .projects
        .iter_mut()
        .find(|p| p.id == project_id)
        .ok_or_else(|| anyhow::anyhow!("project '{project_id}' not found"))?;
    if !project.is_remote_shell() {
        anyhow::bail!("project '{project_id}' is not a remote-shell project");
    }
    if project.command.as_deref() == Some(trimmed) {
        return Ok(false);
    }
    project.command = Some(trimmed.to_string());
    Ok(true)
}

/// One git worktree under a [`Project`]. Every project carries an
/// explicit primary row (`is_primary: true`) pointing at
/// `Project::path` plus zero or more additional worktrees with
/// `is_primary: false`. The primary row is seeded by `Project::new`
/// and back-filled by [`ProjectsConfig::migrate`] for legacy
/// configs that predate that rule.
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
        let mut config: Self = match std::fs::read(path) {
            Ok(bytes) if bytes.is_empty() => Self::default(),
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display()))?,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
        };
        config.migrate();
        Ok(config)
    }

    /// Apply schema migrations to an in-memory config. Idempotent —
    /// rerunning is a no-op once every rule has converged. Mirrors
    /// C# `ProjectStore.Migrate`.
    ///
    /// **Rule 1 — primary worktree synthesis.** Every project with a
    /// non-empty `path` and *no* worktree marked `is_primary: true`
    /// gets a synthetic `Worktree { id: "primary", path,
    /// is_primary: true, branch: None }`. Without this, the sidebar
    /// can't render a row for the project's own checkout and the
    /// branch the user is currently on stays invisible.
    ///
    /// We key on the `is_primary` flag rather than "list is empty"
    /// because legacy `projects.json` files written by the Rust UI
    /// before this rule landed could have one or more *non-primary*
    /// rows added later but still no primary entry — hitting only
    /// the empty-list case would leave those projects in the broken
    /// state. The `WorktreeStatusPoller` fills in the branch on its
    /// first tick.
    pub fn migrate(&mut self) {
        for p in &mut self.projects {
            if p.path.is_empty() {
                continue;
            }
            let has_primary = p.worktrees.iter().any(|wt| wt.is_primary);
            if !has_primary {
                p.worktrees.insert(
                    0,
                    Worktree {
                        id: "primary".to_owned(),
                        path: p.path.clone(),
                        branch: None,
                        is_primary: true,
                    },
                );
            }
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

    /// Return the index of the first project whose path normalises to
    /// the same value as `path`. Used by the "Add project" dialog to
    /// reject duplicates before mutating state. The match is path-
    /// normalised (trailing separators stripped, Windows backslashes
    /// folded to `/`, case-insensitive) so the same folder reached
    /// via different spellings is treated as already added. Mirrors
    /// the C# `SessionStore.AddProjectAsync` duplicate check, which
    /// runs against `Path.GetFullPath` of both sides.
    pub fn find_project_index_by_path(&self, path: &str) -> Option<usize> {
        let needle = normalise_project_path(path);
        if needle.is_empty() {
            return None;
        }
        self.projects
            .iter()
            .position(|p| normalise_project_path(&p.path) == needle)
    }
}

/// Normalise a project path for duplicate-detection. Trims trailing
/// path separators, folds backslashes to forward slashes, and lower-
/// cases on Windows (Win32 paths are case-insensitive). Pure helper —
/// extracted so the dialog and the tests can call it directly.
pub fn normalise_project_path(path: &str) -> String {
    let trimmed = path.trim().trim_end_matches(['\\', '/']);
    let unified: String = trimmed.chars().map(|c| if c == '\\' { '/' } else { c }).collect();
    if cfg!(windows) {
        unified.to_lowercase()
    } else {
        unified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::no_window_command;

    /// Skip-aware test helper — initialises a real git repo under a
    /// tempdir and runs an empty seed commit so HEAD is real. Returns
    /// `None` when `git` isn't on PATH so a CI image without git in
    /// scope still goes green (same convention as `core::git::tests`).
    fn init_repo_with_commit() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
        if no_window_command("git").arg("--version").output().is_err() {
            eprintln!("skipping: `git` not on PATH");
            return None;
        }
        let dir = tempfile::tempdir().ok()?;
        let repo = dir.path().join("repo");
        // Pre-create the default worktree root so `git worktree add`
        // doesn't have to (it errors on a missing parent dir).
        let worktrees_root = dir.path().join("repo.worktrees");
        std::fs::create_dir_all(&repo).ok()?;
        std::fs::create_dir_all(&worktrees_root).ok()?;
        for args in [
            &["-c", "init.defaultBranch=main", "init", "-q"][..],
            &["config", "user.email", "test@example.invalid"][..],
            &["config", "user.name", "Test"][..],
            &["commit", "--allow-empty", "-m", "init", "-q"][..],
        ] {
            let out = no_window_command("git")
                .args(args)
                .current_dir(&repo)
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
        }
        Some((dir, repo))
    }

    #[test]
    fn adopt_existing_worktrees_picks_up_non_primary_rows() {
        let Some((_guard, repo)) = init_repo_with_commit() else {
            return;
        };
        let wt_path = repo.parent().unwrap().join("repo.worktrees").join("feat-x");
        crate::git::add_worktree(&repo, &wt_path, "feat/x", None)
            .expect("git worktree add succeeds");

        let mut project = Project::new(repo.to_string_lossy().to_string());
        assert_eq!(project.worktrees.len(), 1, "fresh project = primary only");

        project.adopt_existing_worktrees();
        assert_eq!(project.worktrees.len(), 2, "feat/x should be adopted");
        let feat = &project.worktrees[1];
        assert!(!feat.is_primary, "discovered worktree is non-primary");
        assert!(feat.path.ends_with("feat-x"), "path: {}", feat.path);
        assert_eq!(feat.branch.as_deref(), Some("feat/x"));
        assert!(!feat.id.is_empty() && feat.id != "primary", "real uuid id");

        // Idempotent: a second call must not duplicate the entry.
        project.adopt_existing_worktrees();
        assert_eq!(project.worktrees.len(), 2, "second pass no-ops");
    }

    #[test]
    fn adopt_existing_worktrees_swallows_non_git_path() {
        // Path that isn't a git repo at all — must not panic, must not
        // mutate the worktrees list.
        let dir = tempfile::tempdir().unwrap();
        let mut project = Project::new(dir.path().to_string_lossy().to_string());
        let before = project.worktrees.len();
        project.adopt_existing_worktrees();
        assert_eq!(project.worktrees.len(), before, "no change on non-git path");
    }

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
                kind: ProjectKind::Local,
                command: None,
                remote_agent_id: None,
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
    fn new_project_seeds_a_primary_worktree() {
        let p = Project::new("/home/me/codescope".into());
        assert_eq!(p.worktrees.len(), 1);
        let wt = &p.worktrees[0];
        assert_eq!(wt.id, "primary");
        assert_eq!(wt.path, "/home/me/codescope");
        assert!(wt.is_primary);
        assert!(wt.branch.is_none());
    }

    #[test]
    fn migrate_synthesises_primary_for_legacy_projects_without_worktrees() {
        // Old projects.json from before the seed-on-create rule:
        // a project entry with no `worktrees` field. The migration
        // should add a primary so the sidebar isn't blank for it.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        std::fs::write(
            &path,
            r#"{"version":1,"projects":[{"id":"p1","name":"Repo","path":"C:\\repos\\repo","defaultBranch":"main"}]}"#,
        )
        .unwrap();

        let cfg = ProjectsConfig::load_from(&path).unwrap();
        let p = &cfg.projects[0];
        assert_eq!(p.worktrees.len(), 1, "primary should be synthesised");
        assert!(p.worktrees[0].is_primary);
        assert_eq!(p.worktrees[0].path, "C:\\repos\\repo");
        assert_eq!(p.worktrees[0].id, "primary");
    }

    #[test]
    fn migrate_back_fills_primary_when_only_secondary_worktrees_exist() {
        // Legacy projects.json from a Rust UI that had no
        // primary-seeding rule but did support adding secondary
        // worktrees via "New worktree from branch…". The user could
        // end up with a project that has secondaries but no primary,
        // which the original empty-list-only migration would skip.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        std::fs::write(
            &path,
            r#"{
              "version": 1,
              "projects": [
                {
                  "id": "p1",
                  "name": "Repo",
                  "path": "C:\\repos\\repo",
                  "defaultBranch": "main",
                  "worktrees": [
                    { "id": "feat-x", "path": "C:\\repos\\repo.worktrees\\feat-x", "branch": "feat/x", "isPrimary": false }
                  ]
                }
              ]
            }"#,
        )
        .unwrap();

        let cfg = ProjectsConfig::load_from(&path).unwrap();
        let p = &cfg.projects[0];
        assert_eq!(p.worktrees.len(), 2, "primary back-filled in addition to existing secondary");
        assert!(p.worktrees[0].is_primary, "primary inserted at index 0");
        assert_eq!(p.worktrees[0].id, "primary");
        assert_eq!(p.worktrees[0].path, "C:\\repos\\repo");
        assert!(!p.worktrees[1].is_primary, "secondary preserved");
        assert_eq!(p.worktrees[1].id, "feat-x");
    }

    #[test]
    fn migrate_is_idempotent_on_already_migrated_config() {
        // Project that already has a primary should *not* get a
        // second one. Migration is idempotent.
        let mut cfg = ProjectsConfig {
            version: CURRENT_VERSION,
            agents: Vec::new(),
            projects: vec![Project::new("/home/me/repo".into())],
        };
        let before_len = cfg.projects[0].worktrees.len();
        cfg.migrate();
        cfg.migrate();
        assert_eq!(cfg.projects[0].worktrees.len(), before_len);
    }

    #[test]
    fn migrate_skips_projects_with_empty_path() {
        // No path = no checkout to bind the primary to. Don't
        // synthesise a worktree pointing at "" — that would render
        // a broken row in the sidebar.
        let mut cfg = ProjectsConfig {
            version: CURRENT_VERSION,
            agents: Vec::new(),
            projects: vec![Project {
                id: "p1".into(),
                name: "Empty".into(),
                path: String::new(),
                default_branch: "main".into(),
                worktree_root: None,
                kind: ProjectKind::Local,
                command: None,
                remote_agent_id: None,
                default_agent_id: None,
                sessions: Vec::new(),
                worktrees: Vec::new(),
            }],
        };
        cfg.migrate();
        assert!(cfg.projects[0].worktrees.is_empty());
    }

    #[test]
    fn remote_shell_project_has_no_path_and_no_worktrees() {
        let p = Project::new_remote_shell("  devbox ".into(), " ssh dev@box -t claude ".into(), None);
        assert_eq!(p.name, "devbox");
        assert_eq!(p.path, "");
        assert_eq!(p.kind, ProjectKind::RemoteShell);
        assert!(p.is_remote_shell());
        assert_eq!(p.remote_shell_command(), Some("ssh dev@box -t claude"));
        assert!(p.worktrees.is_empty());
        assert!(p.sessions.is_empty());
    }

    #[test]
    fn local_project_never_reports_a_remote_shell_command() {
        let mut p = Project::new("C:/repo".into());
        assert!(!p.is_remote_shell());
        assert_eq!(p.remote_shell_command(), None);
        // Even a stray `command` on a local row is ignored — `kind`
        // is the discriminator, not the presence of the string.
        p.command = Some("ssh somewhere".into());
        assert_eq!(p.remote_shell_command(), None);
    }

    #[test]
    fn remote_shell_with_blank_command_is_treated_as_missing() {
        let mut p = Project::new_remote_shell("x".into(), "ssh box".into(), None);
        p.command = Some("   ".into());
        assert_eq!(p.remote_shell_command(), None);
        p.command = None;
        assert_eq!(p.remote_shell_command(), None);
    }

    #[test]
    fn set_remote_shell_command_updates_and_reports_change() {
        let p = Project::new_remote_shell("dev".into(), "ssh old".into(), None);
        let id = p.id.clone();
        let mut cfg = ProjectsConfig { version: 1, agents: vec![], projects: vec![p] };

        assert!(set_remote_shell_command(&mut cfg, &id, "  ssh new-box  ").expect("update"));
        assert_eq!(cfg.projects[0].remote_shell_command(), Some("ssh new-box"));
        // Same trimmed value again → no-op.
        assert!(!set_remote_shell_command(&mut cfg, &id, "ssh new-box").expect("no-op"));
    }

    #[test]
    fn set_remote_shell_command_rejects_empty_unknown_and_local() {
        let remote = Project::new_remote_shell("dev".into(), "ssh box".into(), None);
        let remote_id = remote.id.clone();
        let local = Project::new("C:\\repo".into());
        let local_id = local.id.clone();
        let mut cfg =
            ProjectsConfig { version: 1, agents: vec![], projects: vec![remote, local] };

        // Each rejection names its actual cause — a multi-line paste
        // must not be reported as "empty".
        let empty_err = set_remote_shell_command(&mut cfg, &remote_id, "   ")
            .expect_err("blank command must be rejected");
        assert!(format!("{empty_err:#}").contains("empty"), "{empty_err:#}");
        let multiline_err = set_remote_shell_command(&mut cfg, &remote_id, "ssh a\nssh b")
            .expect_err("multi-line command must be rejected");
        assert!(
            format!("{multiline_err:#}").contains("single line"),
            "{multiline_err:#}"
        );
        assert!(set_remote_shell_command(&mut cfg, "nope", "ssh x").is_err());
        assert!(set_remote_shell_command(&mut cfg, &local_id, "ssh x").is_err());
        // Nothing was mutated by the failed calls.
        assert_eq!(cfg.projects[0].remote_shell_command(), Some("ssh box"));
        assert_eq!(cfg.projects[1].command, None);
    }

    #[test]
    fn remote_command_with_agent_wraps_in_login_shell() {
        // The agent runs through the remote login+interactive shell so
        // it inherits the user's real PATH (`~/.local/bin`, nvm, …) —
        // a bare `-t claude` would miss it. See the fn doc for why.
        assert_eq!(
            remote_command_with_agent("ssh dev", Some("claude")),
            "ssh dev -t '$SHELL -lic \"claude\"'"
        );
        // Trims both sides before joining.
        assert_eq!(
            remote_command_with_agent("  ssh dev  ", Some("  claude  ")),
            "ssh dev -t '$SHELL -lic \"claude\"'"
        );
    }

    #[test]
    fn remote_command_with_agent_keeps_agent_args_in_one_c_string() {
        // Double-quoting the agent inside keeps a multi-token launch
        // (`claude --resume x`) as a single `-c` argument on the far
        // end rather than splitting into positional params.
        assert_eq!(
            remote_command_with_agent("ssh dev", Some("claude --resume x")),
            "ssh dev -t '$SHELL -lic \"claude --resume x\"'"
        );
    }

    #[test]
    fn remote_command_with_agent_no_agent_is_verbatim_trimmed() {
        assert_eq!(remote_command_with_agent("ssh dev", None), "ssh dev");
        assert_eq!(remote_command_with_agent("ssh dev", Some("   ")), "ssh dev");
        assert_eq!(remote_command_with_agent("  ssh dev ", None), "ssh dev");
    }

    #[test]
    fn new_remote_shell_stores_and_blank_agent_becomes_none() {
        let p = Project::new_remote_shell("dev".into(), "ssh dev".into(), Some("claude".into()));
        assert_eq!(p.remote_agent_id.as_deref(), Some("claude"));
        let p2 = Project::new_remote_shell("dev".into(), "ssh dev".into(), Some("  ".into()));
        assert_eq!(p2.remote_agent_id, None);
    }

    #[test]
    fn remote_shell_command_validation() {
        assert!(is_valid_remote_shell_command("ssh dev@box -t claude"));
        assert!(is_valid_remote_shell_command("  wsl.exe -d Ubuntu  "));
        assert!(!is_valid_remote_shell_command(""));
        assert!(!is_valid_remote_shell_command("   \t "));
        assert!(!is_valid_remote_shell_command("ssh box\nrm -rf /"));
        assert!(!is_valid_remote_shell_command("ssh box\r\nclaude"));
    }

    #[test]
    fn migrate_leaves_remote_shell_projects_without_a_primary() {
        // A remote-shell project has an empty path, so `migrate`
        // must not synthesise a primary worktree for it — that row
        // would point at "" and the pollers would shell out to git
        // against it every tick.
        let mut cfg = ProjectsConfig {
            version: CURRENT_VERSION,
            agents: Vec::new(),
            projects: vec![Project::new_remote_shell("box".into(), "ssh box".into(), None)],
        };
        cfg.migrate();
        assert!(cfg.projects[0].worktrees.is_empty());
        assert!(cfg.projects[0].is_remote_shell());
    }

    #[test]
    fn adopt_existing_worktrees_is_a_no_op_for_remote_shell() {
        let mut p = Project::new_remote_shell("box".into(), "ssh box".into(), None);
        p.adopt_existing_worktrees();
        assert!(p.worktrees.is_empty());
    }

    #[test]
    fn remote_shell_project_round_trips_with_camelcase_kind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        let cfg = ProjectsConfig {
            version: CURRENT_VERSION,
            agents: Vec::new(),
            projects: vec![Project::new_remote_shell("box".into(), "ssh box -t claude".into(), None)],
        };
        cfg.save_to(&path).unwrap();
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("\"kind\": \"remoteShell\""), "{written}");
        assert!(written.contains("\"command\": \"ssh box -t claude\""), "{written}");

        let loaded = ProjectsConfig::load_from(&path).unwrap();
        let p = &loaded.projects[0];
        assert_eq!(p.kind, ProjectKind::RemoteShell);
        assert_eq!(p.remote_shell_command(), Some("ssh box -t claude"));
        assert!(p.worktrees.is_empty(), "migrate must not add a primary");
    }

    #[test]
    fn project_without_kind_field_loads_as_local() {
        // Every projects.json written before #323 lacks `kind`; those
        // rows are all local checkouts and must keep behaving as such.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.json");
        std::fs::write(
            &path,
            r#"{"version":1,"projects":[{"id":"p1","name":"Repo","path":"C:/repo"}]}"#,
        )
        .unwrap();
        let loaded = ProjectsConfig::load_from(&path).unwrap();
        let p = &loaded.projects[0];
        assert_eq!(p.kind, ProjectKind::Local);
        assert_eq!(p.command, None);
        assert!(!p.is_remote_shell());
        assert!(p.worktrees.iter().any(|wt| wt.is_primary), "migrate still seeds the primary");
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
                kind: ProjectKind::Local,
                command: None,
                remote_agent_id: None,
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

    #[test]
    fn normalise_project_path_handles_trailing_separators_and_backslashes() {
        let canonical = normalise_project_path("C:\\repos\\repo");
        assert_eq!(normalise_project_path("C:\\repos\\repo\\"), canonical);
        assert_eq!(normalise_project_path("C:/repos/repo"), canonical);
        assert_eq!(normalise_project_path("  C:\\repos\\repo  "), canonical);
    }

    #[test]
    fn find_project_index_by_path_dedups_across_spellings() {
        let cfg = ProjectsConfig {
            version: CURRENT_VERSION,
            agents: Vec::new(),
            projects: vec![Project::new("C:\\repos\\repo".into())],
        };
        // Trailing slash and forward-slash variants resolve to the
        // same project; case-insensitive on Windows.
        assert_eq!(cfg.find_project_index_by_path("C:\\repos\\repo"), Some(0));
        assert_eq!(cfg.find_project_index_by_path("C:\\repos\\repo\\"), Some(0));
        assert_eq!(cfg.find_project_index_by_path("C:/repos/repo"), Some(0));
        if cfg!(windows) {
            assert_eq!(cfg.find_project_index_by_path("c:\\REPOS\\repo"), Some(0));
        }
        assert_eq!(cfg.find_project_index_by_path("C:\\repos\\other"), None);
        assert_eq!(cfg.find_project_index_by_path(""), None);
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

    #[test]
    fn rename_project_updates_name_and_reports_change() {
        let mut cfg = ProjectsConfig::default();
        let project = Project::new("C:\\repos\\foo".into());
        let id = project.id.clone();
        cfg.projects.push(project);

        let changed = rename_project(&mut cfg, &id, "  Renamed  ").unwrap();
        assert!(changed);
        assert_eq!(cfg.projects[0].name, "Renamed");
    }

    #[test]
    fn rename_project_is_noop_when_name_unchanged() {
        let mut cfg = ProjectsConfig::default();
        let mut project = Project::new("C:\\repos\\foo".into());
        project.name = "Stable".into();
        let id = project.id.clone();
        cfg.projects.push(project);

        let changed = rename_project(&mut cfg, &id, "Stable").unwrap();
        assert!(!changed);
        // Trim still applies to the comparison.
        let changed = rename_project(&mut cfg, &id, "  Stable  ").unwrap();
        assert!(!changed);
    }

    #[test]
    fn rename_project_rejects_empty_or_whitespace_name() {
        let mut cfg = ProjectsConfig::default();
        let project = Project::new("C:\\repos\\foo".into());
        let id = project.id.clone();
        cfg.projects.push(project);

        assert!(rename_project(&mut cfg, &id, "").is_err());
        assert!(rename_project(&mut cfg, &id, "   ").is_err());
    }

    #[test]
    fn rename_project_errors_on_unknown_id() {
        let mut cfg = ProjectsConfig::default();
        cfg.projects.push(Project::new("C:\\repos\\foo".into()));

        let err = rename_project(&mut cfg, "ghost", "New").unwrap_err();
        assert!(err.to_string().contains("ghost"));
    }
}
