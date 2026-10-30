//! In-flight "New worktree" creations (#342).
//!
//! `git worktree add` on a big repository checks out the whole tree
//! and can take a long time, so the sidebar runs it on the background
//! executor and shows a "creating…" row while it does. This module
//! holds the pure bookkeeping behind that row, away from gpui so it
//! can be unit-tested:
//!
//! - [`PendingWorktrees`] tracks what is being created right now. It
//!   refuses a second creation of the same folder, or of the same
//!   branch in the same project, while the first is still running.
//! - [`record_created_worktree`] adds the finished worktree's row to a
//!   freshly reloaded [`ProjectsConfig`], matching the project by id
//!   (indices can shift while git runs) and never adding a second row
//!   for a path some project already tracks.
//!
//! Nothing here is persisted: a creation cut short by closing the app
//! leaves no trace in `projects.json`, and the worktree discovery poll
//! adopts whatever git finished on disk at the next launch.

use crate::git::WorktreeInfo;
use crate::projects::{ProjectsConfig, Worktree, normalise_project_path};

/// Handle for one in-flight creation, unique for the life of a
/// [`PendingWorktrees`].
pub type PendingId = u64;

/// One worktree being created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingWorktree {
    pub id: PendingId,
    /// Stable id of the owning project (not its index).
    pub project_id: String,
    /// Folder `git worktree add` is creating, as the user entered it.
    pub path: String,
    /// Branch being created with `-b`.
    pub branch: String,
}

/// Why [`PendingWorktrees::begin`] refused a creation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeginError {
    /// The same folder is already being created.
    PathInFlight { path: String },
    /// The same branch is already being created in this project.
    BranchInFlight { branch: String },
}

impl std::fmt::Display for BeginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PathInFlight { path } => {
                write!(f, "a worktree is already being created at {path}")
            }
            Self::BranchInFlight { branch } => {
                write!(f, "a worktree for branch '{branch}' is already being created")
            }
        }
    }
}

/// The set of creations currently running.
#[derive(Debug, Default)]
pub struct PendingWorktrees {
    next_id: PendingId,
    items: Vec<PendingWorktree>,
}

impl PendingWorktrees {
    /// Register a creation about to start. Fails when the same folder
    /// (compared normalised, so `C:\a\b` and `c:/a/b/` match on
    /// Windows) or the same branch in the same project is already in
    /// flight: the second `git worktree add` could only fail, and two
    /// "creating…" rows for one folder would be confusing.
    pub fn begin(
        &mut self,
        project_id: &str,
        path: &str,
        branch: &str,
    ) -> Result<PendingId, BeginError> {
        let key = normalise_project_path(path);
        for item in &self.items {
            if normalise_project_path(&item.path) == key {
                return Err(BeginError::PathInFlight { path: path.to_string() });
            }
            if item.project_id == project_id && item.branch == branch {
                return Err(BeginError::BranchInFlight { branch: branch.to_string() });
            }
        }
        self.next_id += 1;
        let id = self.next_id;
        self.items.push(PendingWorktree {
            id,
            project_id: project_id.to_string(),
            path: path.to_string(),
            branch: branch.to_string(),
        });
        Ok(id)
    }

    /// Forget creation `id` once it has finished, either way. Returns
    /// what was registered, or `None` for an unknown or already
    /// finished id.
    pub fn finish(&mut self, id: PendingId) -> Option<PendingWorktree> {
        let pos = self.items.iter().position(|p| p.id == id)?;
        Some(self.items.remove(pos))
    }

    /// Look up an in-flight creation.
    pub fn get(&self, id: PendingId) -> Option<&PendingWorktree> {
        self.items.iter().find(|p| p.id == id)
    }

    /// `true` when nothing is being created.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// In-flight creations for one project, oldest first.
    pub fn for_project<'a>(
        &'a self,
        project_id: &'a str,
    ) -> impl Iterator<Item = &'a PendingWorktree> + 'a {
        self.items.iter().filter(move |p| p.project_id == project_id)
    }

    /// What of one discovery scan (`(project id, git worktree list)`
    /// per project) may be synced while creations are in flight. Git
    /// lists a worktree as soon as `git worktree add` registers it,
    /// long before its checkout is done, so adopting from such a list
    /// would race the in-app create for the row. A project with a
    /// create in flight sits the scan out entirely; every other
    /// project is synced, minus any listed path being created (another
    /// checkout of the same repository lists it too).
    pub fn discovery_view(
        &self,
        listed: &[(String, Vec<WorktreeInfo>)],
    ) -> Vec<(String, Vec<WorktreeInfo>)> {
        let in_flight: Vec<String> =
            self.items.iter().map(|p| normalise_project_path(&p.path)).collect();
        listed
            .iter()
            .filter(|(project_id, _)| self.for_project(project_id).next().is_none())
            .map(|(project_id, wts)| {
                let wts = wts
                    .iter()
                    .filter(|w| !in_flight.contains(&normalise_project_path(&w.path)))
                    .cloned()
                    .collect();
                (project_id.clone(), wts)
            })
            .collect()
    }
}

/// What [`record_created_worktree`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    /// A new row was appended; the caller should save.
    Added,
    /// Some project already tracks this path, as its root or as a
    /// worktree (for example the discovery poll adopted it first).
    /// Nothing changed.
    AlreadyTracked,
    /// The project was removed while git ran. Nothing changed.
    ProjectGone,
}

/// Append a row for a worktree `git worktree add` just created at
/// `path` on `branch` to project `project_id`. Meant to run on a
/// config reloaded from disk right before saving it (the
/// reload-then-mutate rule every `projects.json` writer follows).
pub fn record_created_worktree(
    cfg: &mut ProjectsConfig,
    project_id: &str,
    path: &str,
    branch: &str,
) -> RecordOutcome {
    let key = normalise_project_path(path);
    if cfg.stored_paths().iter().any(|p| normalise_project_path(p) == key) {
        return RecordOutcome::AlreadyTracked;
    }
    let Some(project) = cfg.projects.iter_mut().find(|p| p.id == project_id) else {
        return RecordOutcome::ProjectGone;
    };
    project.worktrees.push(Worktree {
        id: uuid::Uuid::new_v4().to_string(),
        path: path.to_string(),
        branch: Some(branch.to_string()),
        is_primary: false,
    });
    RecordOutcome::Added
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::Project;

    #[test]
    fn begin_hands_out_distinct_ids_and_lists_per_project() {
        let mut pending = PendingWorktrees::default();
        assert!(pending.is_empty());
        let a = pending.begin("p1", "/repo.worktrees/a", "a").unwrap();
        let b = pending.begin("p1", "/repo.worktrees/b", "b").unwrap();
        let c = pending.begin("p2", "/other.worktrees/c", "c").unwrap();
        assert!(a != b && b != c && a != c);
        assert!(!pending.is_empty());
        let p1: Vec<&str> = pending.for_project("p1").map(|p| p.branch.as_str()).collect();
        assert_eq!(p1, vec!["a", "b"]);
        assert_eq!(pending.for_project("p2").count(), 1);
        assert_eq!(pending.for_project("nope").count(), 0);
        assert_eq!(pending.get(c).map(|p| p.project_id.as_str()), Some("p2"));
    }

    #[test]
    fn begin_refuses_a_folder_already_in_flight() {
        let mut pending = PendingWorktrees::default();
        pending.begin("p1", "/repo.worktrees/feat", "feat").unwrap();
        let err = pending.begin("p2", "/repo.worktrees/feat/", "other").unwrap_err();
        assert_eq!(err, BeginError::PathInFlight { path: "/repo.worktrees/feat/".into() });
    }

    #[cfg(windows)]
    #[test]
    fn begin_compares_folders_like_windows_does() {
        let mut pending = PendingWorktrees::default();
        pending.begin("p1", r"C:\repo.worktrees\Feat", "feat").unwrap();
        assert!(pending.begin("p1", "c:/repo.worktrees/feat", "x").is_err());
    }

    #[test]
    fn begin_refuses_the_same_branch_twice_in_one_project_only() {
        let mut pending = PendingWorktrees::default();
        pending.begin("p1", "/repo.worktrees/one", "feat").unwrap();
        let err = pending.begin("p1", "/repo.worktrees/two", "feat").unwrap_err();
        assert_eq!(err, BeginError::BranchInFlight { branch: "feat".into() });
        // Another project is another repository: same name is fine.
        assert!(pending.begin("p2", "/other.worktrees/one", "feat").is_ok());
    }

    fn listed(path: &str) -> WorktreeInfo {
        WorktreeInfo {
            path: path.into(),
            head: "abc".into(),
            branch: None,
            is_primary: false,
            locked: false,
            prunable: false,
        }
    }

    #[test]
    fn discovery_holds_back_only_projects_with_a_create_in_flight() {
        let mut pending = PendingWorktrees::default();
        pending.begin("a", "/a.worktrees/feat", "feat").unwrap();
        let scan = vec![
            ("a".to_string(), vec![listed("/a"), listed("/a.worktrees/feat")]),
            ("b".to_string(), vec![listed("/b"), listed("/b.worktrees/x")]),
        ];
        let kept = pending.discovery_view(&scan);
        let ids: Vec<&str> = kept.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, vec!["b"], "project a sits this scan out");
        assert_eq!(kept[0].1.len(), 2, "project b is synced in full");
    }

    #[test]
    fn discovery_never_offers_a_half_made_worktree_to_another_project() {
        // Project `c` is another checkout of the same repository (a
        // worktree added as a project of its own), so its listing
        // names the tree `a` is still creating.
        let mut pending = PendingWorktrees::default();
        pending.begin("a", "/a.worktrees/feat", "feat").unwrap();
        let scan = vec![(
            "c".to_string(),
            vec![listed("/a"), listed("/a.worktrees/feat/"), listed("/a.worktrees/old")],
        )];
        let kept = pending.discovery_view(&scan);
        let paths: Vec<&str> = kept[0].1.iter().map(|w| w.path.as_str()).collect();
        assert_eq!(paths, vec!["/a", "/a.worktrees/old"]);
    }

    #[test]
    fn discovery_passes_everything_through_when_nothing_is_pending() {
        let pending = PendingWorktrees::default();
        let scan = vec![("a".to_string(), vec![listed("/a"), listed("/a.worktrees/x")])];
        assert_eq!(pending.discovery_view(&scan), scan);
    }

    #[test]
    fn finish_frees_the_slot_once() {
        let mut pending = PendingWorktrees::default();
        let id = pending.begin("p1", "/repo.worktrees/feat", "feat").unwrap();
        let done = pending.finish(id).expect("registered");
        assert_eq!(done.path, "/repo.worktrees/feat");
        assert!(pending.is_empty());
        assert!(pending.finish(id).is_none(), "second finish is a no-op");
        assert!(pending.get(id).is_none());
        // The folder can be retried once the first attempt is over,
        // and the retry gets a fresh id.
        let retry = pending.begin("p1", "/repo.worktrees/feat", "feat").unwrap();
        assert_ne!(retry, id);
    }

    fn config_with_project(path: &str) -> (ProjectsConfig, String) {
        let mut cfg = ProjectsConfig::default();
        let project = Project::new(path.to_string());
        let id = project.id.clone();
        cfg.projects.push(project);
        (cfg, id)
    }

    #[test]
    fn record_appends_a_secondary_row_with_the_branch() {
        let (mut cfg, id) = config_with_project("/repo");
        let outcome = record_created_worktree(&mut cfg, &id, "/repo.worktrees/feat", "feat");
        assert_eq!(outcome, RecordOutcome::Added);
        let wts = &cfg.projects[0].worktrees;
        assert_eq!(wts.len(), 2, "primary + new row");
        let row = &wts[1];
        assert_eq!(row.path, "/repo.worktrees/feat");
        assert_eq!(row.branch.as_deref(), Some("feat"));
        assert!(!row.is_primary);
        assert!(!row.id.is_empty() && row.id != "primary");
    }

    #[test]
    fn record_finds_the_project_by_id_after_a_reorder() {
        let (mut cfg, id) = config_with_project("/repo");
        cfg.projects.insert(0, Project::new("/first".to_string()));
        assert_eq!(
            record_created_worktree(&mut cfg, &id, "/repo.worktrees/feat", "feat"),
            RecordOutcome::Added
        );
        assert_eq!(cfg.projects[0].worktrees.len(), 1, "other project untouched");
        assert_eq!(cfg.projects[1].worktrees.len(), 2);
    }

    #[test]
    fn record_skips_a_path_already_tracked_anywhere() {
        let (mut cfg, id) = config_with_project("/repo");
        // Adopted by the discovery poll under another spelling.
        cfg.projects[0].worktrees.push(Worktree {
            id: "adopted".into(),
            path: "/repo.worktrees/feat/".into(),
            branch: Some("feat".into()),
            is_primary: false,
        });
        assert_eq!(
            record_created_worktree(&mut cfg, &id, "/repo.worktrees/feat", "feat"),
            RecordOutcome::AlreadyTracked
        );
        assert_eq!(cfg.projects[0].worktrees.len(), 2);
        // A folder another project has as its root counts too.
        cfg.projects.push(Project::new("/repo.worktrees/solo".to_string()));
        assert_eq!(
            record_created_worktree(&mut cfg, &id, "/repo.worktrees/solo", "solo"),
            RecordOutcome::AlreadyTracked
        );
    }

    #[test]
    fn record_reports_a_project_removed_meanwhile() {
        let (mut cfg, _) = config_with_project("/repo");
        assert_eq!(
            record_created_worktree(&mut cfg, "gone", "/repo.worktrees/feat", "feat"),
            RecordOutcome::ProjectGone
        );
        assert_eq!(cfg.projects[0].worktrees.len(), 1);
    }
}
