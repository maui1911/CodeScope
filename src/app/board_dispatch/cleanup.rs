//! Cleanup (ADR-0027): once the work of a dispatch ended and its session is done, its tabs close
//! and its worktree and branch go, unless something there is its own; then the row says why.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codescope_core::board::ItemKind;
use codescope_core::board::cleanup::{Ended, Kept, clean_up, ended, kept};
use codescope_core::board::dispatch::WorktreeDispatch;
use codescope_core::path_canon::paths_match;
use codescope_core::process_tree;
use codescope_core::projects::ProjectsConfig;
use gpui::{App, AppContext as _, Context, Window};

use super::{AppShell, CardNote, NoteTone, SessionAt, ToastKind};

const CHECK_EVERY: Duration = Duration::from_secs(60);
const EXIT_WAIT: Duration = Duration::from_secs(20);
const EXIT_POLL: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct Cleanup {
    running: bool,
    removable: bool,
    checked_at: Option<Instant>,
    failed: Option<(Instant, String)>,
    told: Option<String>,
}

struct Target {
    project_id: String,
    repo: PathBuf,
    path: PathBuf,
    branch: String,
    default_branch: String,
}

impl AppShell {
    pub(crate) fn run_cleanups(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (project_id, path, branch, record) in self.dispatch_records(cx) {
            let target = branch.and_then(|branch| {
                let project = self.current_projects(cx).project(&project_id)?;
                let primary = paths_match(&project.path, &path.to_string_lossy());
                (!primary).then(|| Target {
                    project_id: project_id.clone(),
                    repo: PathBuf::from(&project.path),
                    path: path.clone(),
                    branch,
                    default_branch: project.default_branch.clone(),
                })
            });
            let target = target.filter(|target| target.path.exists());
            let Some((target, ended)) = target.zip(self.dispatch_ended(&record)) else {
                self.board.dispatch.cleanups.remove(&record.item_id);
                continue;
            };
            self.clean_up_dispatch(target, &record, ended, window, cx);
        }
    }

    fn dispatch_ended(&self, record: &WorktreeDispatch) -> Option<Ended> {
        let (board, _) = self.board_and_mapping(&record.board_url)?;
        let item = board.items.iter().find(|item| item.id == record.item_id)?;
        let watch = self.board.dispatch.watches.get(&record.item_id);
        let own = match record.kind {
            ItemKind::PullRequest => Some(record.number),
            ItemKind::Issue | ItemKind::Draft => record
                .pull_request
                .as_ref()
                .or(watch.and_then(|watch| watch.reported.as_ref()))
                .map(|posted| posted.number),
        };
        let read = watch
            .filter(|watch| watch.read_ok_at.is_some())
            .map(|watch| watch.pull_request.as_ref());
        ended(item.state, own, read)
    }

    fn clean_up_dispatch(
        &mut self,
        target: Target,
        record: &WorktreeDispatch,
        ended: Ended,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let item_id = record.item_id.clone();
        let state = self.board.dispatch.cleanups.entry(item_id.clone()).or_default();
        let lately = |at: Option<Instant>| at.is_some_and(|at| at.elapsed() < CHECK_EVERY);
        let failed_lately = lately(state.failed.as_ref().map(|(at, _)| *at));
        let removable = state.removable;
        if state.running || failed_lately || (!removable && lately(state.checked_at)) {
            return;
        }
        let in_use = !self.tabs_on(&target.path).is_empty()
            && !matches!(self.session_at(&target.path, cx), SessionAt::Done(_));
        if in_use {
            return;
        }
        let processes = if removable {
            let processes = self.tab_processes(&target.path, cx);
            self.close_tabs_on(&target.path, window, cx);
            processes
        } else {
            Vec::new()
        };
        if let Some(state) = self.board.dispatch.cleanups.get_mut(&item_id) {
            state.running = true;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = if removable {
                cx.background_spawn(async move {
                    wait_for_exit(&processes);
                    let found = clean_up(
                        &target.repo,
                        &target.path,
                        &target.branch,
                        &ended,
                        &target.default_branch,
                    );
                    (target, found)
                })
                .await
            } else {
                cx.background_spawn(async move {
                    let found = kept(&target.path, &ended, &target.default_branch);
                    (target, found)
                })
                .await
            };
            let _ = this.update(cx, |this, cx| {
                this.finish_cleanup(&item_id, removable, result, cx);
            });
        })
        .detach();
    }

    fn tab_processes(&self, path: &Path, cx: &App) -> Vec<u32> {
        let table = process_tree::process_table();
        self.tabs_on(path)
            .iter()
            .filter_map(|tab| tab.terminal.read(cx).child_pid())
            .flat_map(|shell| {
                let mut tree = process_tree::descendants(shell, &table);
                tree.push(shell);
                tree
            })
            .collect()
    }

    fn close_tabs_on(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = path.to_string_lossy();
        let mut open = Vec::new();
        for (group_idx, group) in self.groups.iter().enumerate() {
            for (tab_idx, tab) in group.tabs.iter().enumerate() {
                let here = tab
                    .working_directory
                    .as_deref()
                    .is_some_and(|wd| paths_match(&wd.to_string_lossy(), &wanted));
                if here && tab.remote_project_id.is_none() {
                    open.push((group_idx, tab_idx));
                }
            }
        }
        for (group_idx, tab_idx) in open.into_iter().rev() {
            self.close_tab(group_idx, tab_idx, window, cx);
        }
    }

    fn finish_cleanup(
        &mut self,
        item_id: &str,
        removed: bool,
        (target, result): (Target, anyhow::Result<Option<Kept>>),
        cx: &mut Context<Self>,
    ) {
        let Some((_, _, _, record)) = self.dispatch_record(item_id, cx) else {
            self.board.dispatch.cleanups.remove(item_id);
            return;
        };
        let number = record.number;
        let state = self.board.dispatch.cleanups.entry(item_id.to_string()).or_default();
        state.running = false;
        state.checked_at = Some(Instant::now());
        cx.notify();
        let found = match result {
            Ok(found) => found,
            Err(err) => {
                let message = format!("{err:#}");
                state.failed = Some((Instant::now(), message.clone()));
                state.removable = false;
                if state.told.as_ref() == Some(&message) {
                    return;
                }
                state.told = Some(message.clone());
                let title = format!("Couldn't clean up #{number}");
                self.push_toast(ToastKind::Err, title, Some(message.into()), cx);
                return;
            }
        };
        state.failed = None;
        match found {
            None if !removed => state.removable = true,
            None => {
                self.board.dispatch.cleanups.remove(item_id);
                self.forget_worktree(&target, cx);
                let title = format!("Cleaned up #{number}");
                let detail = format!("Removed its worktree and the branch {}.", target.branch);
                self.push_toast(ToastKind::Ok, title, Some(detail.into()), cx);
            }
            Some(kept) => {
                state.removable = false;
                if record.kept == Some(kept) {
                    return;
                }
                let place = (target.project_id.as_str(), target.path.as_path());
                let marked = WorktreeDispatch {
                    kept: Some(kept),
                    ..record
                };
                if let Err(err) = self.save_record(place.0, place.1, &target.branch, marked, cx) {
                    eprintln!("warning: couldn't mark the worktree of #{number} as kept: {err}");
                }
                let title = format!("Kept the worktree of #{number}");
                self.push_toast(ToastKind::Info, title, Some(kept_reason(kept).into()), cx);
            }
        }
    }

    fn forget_worktree(&mut self, target: &Target, cx: &mut Context<Self>) {
        let path = target.path.to_string_lossy().to_string();
        let result = ProjectsConfig::reload_and_edit(self.paths.as_ref(), |config| {
            let Some(project) = config.projects.iter_mut().find(|p| p.id == target.project_id)
            else {
                return Ok(false);
            };
            let before = project.worktrees.len();
            project.worktrees.retain(|worktree| !paths_match(&worktree.path, &path));
            Ok(project.worktrees.len() != before)
        });
        match result {
            Ok((next, _)) => {
                self.projects = next.clone();
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.note_worktree_edit();
                    sidebar.replace_projects(next);
                    cx.notify();
                });
            }
            Err(err) => eprintln!("warning: couldn't drop the row of {path}: {err:#}"),
        }
    }

    pub(super) fn cleanup_card(&self, record: &WorktreeDispatch) -> Option<CardNote> {
        cleanup_note(record.kept, self.board.dispatch.cleanups.get(&record.item_id))
    }
}

fn wait_for_exit(processes: &[u32]) {
    let started = Instant::now();
    while started.elapsed() < EXIT_WAIT {
        let table = process_tree::process_table();
        if processes.iter().all(|pid| table.iter().all(|&(alive, _)| alive != *pid)) {
            return;
        }
        std::thread::sleep(EXIT_POLL);
    }
}

fn kept_reason(kept: Kept) -> &'static str {
    match kept {
        Kept::Changes => "It has uncommitted changes.",
        Kept::OwnCommits => {
            "Its branch has commits that neither the merge nor the default branch holds."
        }
    }
}

fn cleanup_note(kept: Option<Kept>, state: Option<&Cleanup>) -> Option<CardNote> {
    if let Some((_, message)) = state.and_then(|state| state.failed.as_ref()) {
        return Some(CardNote::new(format!("Couldn't clean up: {message}"), NoteTone::Failed));
    }
    if state.is_some_and(|state| state.removable) {
        return Some(CardNote::new("Cleaning up…", NoteTone::Working));
    }
    let text = match kept? {
        Kept::Changes => "Kept its worktree · uncommitted changes",
        Kept::OwnCommits => "Kept its worktree · commits of its own",
    };
    Some(CardNote::new(text, NoteTone::Waiting))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cleanup_s_card_says_what_it_does_and_why_it_kept_the_worktree() {
        assert!(cleanup_note(None, None).is_none());
        assert!(cleanup_note(None, Some(&Cleanup::default())).is_none(), "still checking");
        let kept = cleanup_note(Some(Kept::Changes), None).unwrap();
        assert_eq!(kept.text.as_ref(), "Kept its worktree · uncommitted changes");
        assert!(kept.tone == NoteTone::Waiting);
        let own = cleanup_note(Some(Kept::OwnCommits), None).unwrap();
        assert_eq!(own.text.as_ref(), "Kept its worktree · commits of its own");
        let removing = Cleanup {
            removable: true,
            ..Cleanup::default()
        };
        let note = cleanup_note(Some(Kept::Changes), Some(&removing)).unwrap();
        assert_eq!(note.text.as_ref(), "Cleaning up…", "the changes are gone since");
        let failed = Cleanup {
            failed: Some((Instant::now(), "the folder is in use".into())),
            ..Cleanup::default()
        };
        let note = cleanup_note(None, Some(&failed)).unwrap();
        assert_eq!(note.text.as_ref(), "Couldn't clean up: the folder is in use");
        assert!(note.tone == NoteTone::Failed);
    }
}
