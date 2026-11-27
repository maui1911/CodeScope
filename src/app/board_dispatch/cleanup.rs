//! Cleanup (ADR-0027): once the work of a dispatch ended and its session is done, its tabs close
//! and its worktree and branch go, unless something there is its own; then the row says why.

use std::path::Path;
use std::time::{Duration, Instant};

use codescope_core::board::ItemKind;
use codescope_core::board::cleanup::{
    Dispatched, Ended, Kept, clean_up, ended, kept, remove_linked_branch,
};
use codescope_core::board::dispatch::{Hold, WorktreeDispatch, projects_for_repository};
use codescope_core::path_canon::paths_match;
use codescope_core::process_tree;
use codescope_core::projects::ProjectsConfig;
use gpui::{App, AppContext as _, AsyncApp, Context, Window};

use super::{
    AppShell, CardNote, NoteTone, RoundKey, TabAtHandover, ToastKind, Turn, round_key, tab_free,
};

const CHECK_EVERY: Duration = Duration::from_secs(60);
const CHECK_OWN_COMMITS_EVERY: Duration = Duration::from_secs(600);
const EXIT_WAIT: Duration = Duration::from_secs(20);
const EXIT_POLL: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct Cleanup {
    running: bool,
    removable: bool,
    checked_at: Option<Instant>,
    failed: Option<(Instant, String)>,
    refused: bool,
    told: Option<String>,
    waits_on: Option<String>,
}

enum InTheWay {
    Nothing,
    Working,
    Unreadable(String),
}

struct Target {
    project_id: String,
    dispatched: Dispatched,
}

struct Swept {
    target: Target,
    found: anyhow::Result<Option<Kept>>,
    on_remote: anyhow::Result<()>,
}

impl AppShell {
    pub(crate) fn run_cleanups(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (project_id, path, branch, record) in self.dispatch_records(cx) {
            let target = branch
                .and_then(|branch| self.cleanup_target(project_id, &path, branch, &record, cx))
                .filter(|target| target.dispatched.path.exists())
                .filter(|_| !self.round_to_come(&record.item_id));
            let Some((target, ended)) = target.zip(self.dispatch_ended(&record)) else {
                self.board.dispatch.cleanups.remove(&record.item_id);
                continue;
            };
            self.clean_up_dispatch(target, &record, ended, window, cx);
        }
    }

    fn cleanup_target(
        &self,
        project_id: String,
        path: &Path,
        branch: String,
        record: &WorktreeDispatch,
        cx: &App,
    ) -> Option<Target> {
        let project = self.current_projects(cx).project(&project_id)?;
        if paths_match(&project.path, &path.to_string_lossy()) {
            return None;
        }
        let remote = projects_for_repository(&self.board.dispatch.remotes, &record.repository)
            .into_iter()
            .find(|(remotes, _)| remotes.project_id == project_id)
            .map(|(_, remote)| remote.to_string())?;
        let dispatched = Dispatched {
            repo: project.path.clone().into(),
            path: path.to_path_buf(),
            branch,
            remote,
            linked_at: record.linked_at.clone(),
        };
        Some(Target {
            project_id,
            dispatched,
        })
    }

    fn round_to_come(&self, item_id: &str) -> bool {
        let state = &self.board.dispatch;
        let held = state.holds.get(item_id);
        state.starting.contains_key(item_id) || held.is_some_and(Hold::starts_later)
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
        let path = target.dispatched.path.clone();
        let tabs_open = !self.tabs_on(&path).is_empty();
        let in_the_way = self.tabs_in_the_way(&path, cx);
        let every = match record.kept {
            Some(Kept::OwnCommits) => CHECK_OWN_COMMITS_EVERY,
            _ => CHECK_EVERY,
        };
        let state = self.board.dispatch.cleanups.entry(record.item_id.clone()).or_default();
        let waits_on = match in_the_way {
            InTheWay::Unreadable(ref tab) => Some(tab.clone()),
            InTheWay::Nothing | InTheWay::Working => None,
        };
        if state.waits_on != waits_on {
            state.waits_on = waits_on;
            cx.notify();
        }
        if !matches!(in_the_way, InTheWay::Nothing) {
            state.removable = false;
            return;
        }
        let lately =
            |at: Option<Instant>, every: Duration| at.is_some_and(|at| at.elapsed() < every);
        let failed_at = state.failed.as_ref().map(|(at, _)| *at);
        let removable = state.removable;
        if state.running
            || lately(failed_at, CHECK_EVERY)
            || (!removable && lately(state.checked_at, every))
            || (removable && state.refused && tabs_open)
        {
            return;
        }
        state.running = true;
        let processes = if removable {
            let processes = self.tab_processes(&path, cx);
            self.close_tabs_on(&path, window, cx);
            processes
        } else {
            Vec::new()
        };
        cx.notify();
        let item_id = record.item_id.clone();
        let round = round_key(record);
        cx.spawn(async move |this, cx| {
            wait_for_exit(processes, cx).await;
            let found = cx
                .background_spawn(async move {
                    let found = if removable {
                        clean_up(&target.dispatched, &ended)
                    } else {
                        kept(&target.dispatched, &ended)
                    };
                    let on_remote = match found {
                        Ok(None) if removable => remove_linked_branch(&target.dispatched),
                        _ => Ok(()),
                    };
                    Swept { target, found, on_remote }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_cleanup(&item_id, &round, removable, found, cx);
            });
        })
        .detach();
    }

    fn tabs_in_the_way(&self, path: &Path, cx: &App) -> InTheWay {
        let tabs = self.tabs_on(path);
        let states = self.tab_states(&tabs, path, cx);
        let probed: Vec<(TabAtHandover, Turn, bool)> = tabs
            .iter()
            .zip(states)
            .map(|(tab, state)| {
                (state, self.turn(tab), tab.terminal.read(cx).typed_since_submit())
            })
            .collect();
        if tabs_free(&probed) {
            return InTheWay::Nothing;
        }
        match unreadable_tab(&probed) {
            Some(index) => {
                let tab = tabs[index];
                let name = tab.title_parts.label().map(str::to_owned);
                InTheWay::Unreadable(name.unwrap_or_else(|| tab.title.to_string()))
            }
            None => InTheWay::Working,
        }
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
        round: &RoundKey,
        removed: bool,
        Swept { target, found: result, on_remote }: Swept,
        cx: &mut Context<Self>,
    ) {
        let current = self.dispatch_record(item_id, cx).map(|(_, _, _, record)| record);
        let Some(record) = current.filter(|record| round_key(record) == *round) else {
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
                state.refused |= removed;
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
        if !state.refused {
            state.failed = None;
        }
        match found {
            None if !removed => state.removable = true,
            None => {
                self.board.dispatch.cleanups.remove(item_id);
                self.forget_worktree(&target, cx);
                let title = format!("Cleaned up #{number}");
                let (branch, remote) = (&target.dispatched.branch, &target.dispatched.remote);
                let done = format!("Removed its worktree and the branch {branch}");
                let (kind, detail) = match on_remote {
                    Ok(()) => (ToastKind::Ok, format!("{done}.")),
                    Err(err) => (
                        ToastKind::Info,
                        format!("{done} here; it stays on {remote}: {err:#}"),
                    ),
                };
                self.push_toast(kind, title, Some(detail.into()), cx);
            }
            Some(kept) => {
                state.removable = false;
                state.refused = false;
                state.failed = None;
                if record.kept == Some(kept) {
                    return;
                }
                let d = &target.dispatched;
                let marked = WorktreeDispatch {
                    kept: Some(kept),
                    ..record
                };
                let saved = self.save_record(&target.project_id, &d.path, &d.branch, marked, cx);
                if let Err(err) = saved {
                    eprintln!("warning: couldn't mark the worktree of #{number} as kept: {err}");
                }
                let title = format!("Kept the worktree of #{number}");
                self.push_toast(ToastKind::Info, title, Some(kept_reason(kept).into()), cx);
            }
        }
    }

    fn forget_worktree(&mut self, target: &Target, cx: &mut Context<Self>) {
        let path = target.dispatched.path.to_string_lossy().to_string();
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

fn tabs_free(tabs: &[(TabAtHandover, Turn, bool)]) -> bool {
    tabs.iter().all(|&(state, turn, typed)| tab_free(state, turn, typed))
}

fn unreadable_tab(tabs: &[(TabAtHandover, Turn, bool)]) -> Option<usize> {
    tabs.iter().position(|&(state, turn, typed)| {
        !typed && state == TabAtHandover::Other && turn == Turn::Unknown
    })
}

async fn wait_for_exit(processes: Vec<u32>, cx: &mut AsyncApp) {
    let started = Instant::now();
    while !processes.is_empty() && started.elapsed() < EXIT_WAIT {
        let pids = processes.clone();
        let alive = cx
            .background_spawn(async move {
                let table = process_tree::process_table();
                pids.iter().any(|pid| table.iter().any(|&(alive, _)| alive == *pid))
            })
            .await;
        if !alive {
            return;
        }
        cx.background_executor().timer(EXIT_POLL).await;
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
    let text = match kept {
        Some(Kept::Changes) => "Kept its worktree · uncommitted changes".to_string(),
        Some(Kept::OwnCommits) => "Kept its worktree · commits of its own".to_string(),
        None => format!("Close {} to clean up", state?.waits_on.as_deref()?),
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

    #[test]
    fn cleanup_closes_only_tabs_where_nothing_works_or_waits_for_enter() {
        use TabAtHandover::{Busy, Draft, IdleClaude, Other, Shell, Stopped};
        assert!(tabs_free(&[]));
        assert!(tabs_free(&[(Shell, Turn::Unknown, false), (IdleClaude, Turn::Ended, false)]));
        assert!(tabs_free(&[(Other, Turn::Ended, false)]), "an agent whose turn ended");
        assert!(!tabs_free(&[(Shell, Turn::Unknown, true)]), "a command typed in a shell");
        assert!(!tabs_free(&[(Draft, Turn::Ended, true)]), "a prompt not sent yet");
        assert!(!tabs_free(&[(Other, Turn::Ended, true)]));
        assert!(!tabs_free(&[(Other, Turn::Unknown, false)]), "a program it can't read");
        assert!(!tabs_free(&[(Other, Turn::Quiet, false)]));
        assert!(!tabs_free(&[(Busy, Turn::Running, false)]));
        assert!(!tabs_free(&[(Stopped, Turn::Ended, false)]));
        assert!(!tabs_free(&[(IdleClaude, Turn::Ended, false), (Busy, Turn::Running, false)]));
    }

    #[test]
    fn only_a_program_it_can_t_read_waits_for_the_user_to_close_it() {
        use TabAtHandover::{Busy, IdleClaude, Other};
        assert_eq!(unreadable_tab(&[(Busy, Turn::Running, false)]), None, "ends by itself");
        assert_eq!(unreadable_tab(&[(Other, Turn::Unknown, true)]), None, "the user is there");
        assert_eq!(unreadable_tab(&[(Other, Turn::Quiet, false)]), None);
        let tabs = [(IdleClaude, Turn::Ended, false), (Other, Turn::Unknown, false)];
        assert_eq!(unreadable_tab(&tabs), Some(1));
        let waiting = Cleanup {
            waits_on: Some("Copilot CLI".into()),
            ..Cleanup::default()
        };
        let note = cleanup_note(None, Some(&waiting)).unwrap();
        assert_eq!(note.text.as_ref(), "Close Copilot CLI to clean up");
        assert!(note.tone == NoteTone::Waiting);
        let kept = cleanup_note(Some(Kept::Changes), Some(&waiting)).unwrap();
        assert_eq!(kept.text.as_ref(), "Kept its worktree · uncommitted changes");
    }
}
