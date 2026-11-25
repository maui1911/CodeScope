//! Dispatch (ADR-0027): after each board read, start a round for every item the user moved
//! into a trigger column, in a worktree of its own with an agent session on it.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codescope_core::AgentId;
use codescope_core::agents::claude::live_session::{self, LiveProcess};
use codescope_core::board::dispatch::{
    Facts, Hold, ProjectRemotes, Round, StatusChange, WorktreeDispatch, add_dispatch_worktree,
    branch_name, counts_towards_limit, decide, exclude_handover_files, find_skill, launch_with_task,
    mover_to_check, projects_for_repository, task_file, task_message, trigger_content_ids,
    write_task_file,
};
use codescope_core::board::github::{self, Viewer};
use codescope_core::board::{
    Board, BoardItem, BoardRef, ColumnMapping, effective_mapping, mapped_column,
};
use codescope_core::path_canon::paths_match;
use codescope_core::process_tree;
use codescope_core::projects::{ProjectKind, ProjectsConfig};
use codescope_core::worktree_create::record_dispatch;
use codescope_terminal::TerminalView;
use gpui::{AppContext as _, Context, SharedString, Window};

use super::{AppShell, ToastKind};

const REMOTES_MAX_AGE: Duration = Duration::from_secs(300);

#[derive(Default)]
pub(crate) struct DispatchState {
    viewer: Option<Viewer>,
    viewer_error: Option<String>,
    /// Write access by `(repository, login)`, both lower case.
    access: HashMap<(String, String), bool>,
    remotes: Vec<ProjectRemotes>,
    remotes_read_at: Option<Instant>,
    /// Item id → the round being started.
    starting: HashMap<String, Round>,
    /// Item id → the move that failed to start, and why.
    failed: HashMap<String, (String, String)>,
    holds: HashMap<String, Hold>,
}

/// What a board read needs from the app, taken on the foreground.
pub(crate) struct ReadInput {
    boards: Vec<(String, ColumnMapping)>,
    viewer: Option<Viewer>,
    known_access: HashSet<(String, String)>,
    /// Local projects as `(id, path)`, when their remotes are due for a read.
    projects: Option<Vec<(String, String)>>,
}

#[derive(Default)]
pub(crate) struct ReadOutput {
    pub boards: Vec<BoardFetch>,
    viewer: Option<Result<Viewer, String>>,
    access: Vec<((String, String), bool)>,
    remotes: Option<Vec<ProjectRemotes>>,
}

pub(crate) struct BoardFetch {
    pub url: String,
    pub board: Result<Board, String>,
    /// Last moves of the items in trigger columns by content id; `None` when they couldn't be read.
    pub changes: Option<HashMap<String, StatusChange>>,
}

/// A card's line about dispatch.
pub(crate) struct CardNote {
    pub text: SharedString,
    pub tone: NoteTone,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoteTone {
    Working,
    Waiting,
    Failed,
}

struct Start {
    board: Board,
    board_url: String,
    item: BoardItem,
    round: Round,
    trigger_at: String,
}

struct Job {
    repo: PathBuf,
    path: PathBuf,
    branch: String,
    remote: String,
    create: bool,
    record: WorktreeDispatch,
    assign_to: Option<String>,
    board: Board,
    progress_column_id: String,
}

struct Launch {
    project_id: String,
    title: codescope_core::TabTitle,
    agent_id: String,
    command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TabAtHandover {
    Shell,
    IdleAgent,
    Busy,
}

fn tab_at_handover(pids: &[u32], claude: Option<&LiveProcess>, typed: bool) -> TabAtHandover {
    if pids.is_empty() {
        TabAtHandover::Shell
    } else if claude.is_some_and(LiveProcess::is_idle) && !typed {
        TabAtHandover::IdleAgent
    } else {
        TabAtHandover::Busy
    }
}

fn access_key(repository: &str, login: &str) -> (String, String) {
    (repository.to_ascii_lowercase(), login.to_ascii_lowercase())
}

/// Read every board, and for items in trigger columns their last moves, who is signed in to
/// `gh`, the write access of teammates who moved them, and the projects' remotes.
pub(crate) fn read_boards(input: ReadInput) -> ReadOutput {
    let mut output = ReadOutput::default();
    let mut viewer = input.viewer;
    let mut any_trigger = false;
    for (url, stored) in input.boards {
        let board = match BoardRef::parse_url(&url) {
            Some(board) => github::fetch_board(&board).map_err(|err| format!("{err:#}")),
            None => Err("this is not a GitHub Projects board URL".to_string()),
        };
        let changes = board.as_ref().ok().and_then(|board| {
            let ids = trigger_content_ids(board, &effective_mapping(board, &stored));
            if ids.is_empty() {
                return Some(HashMap::new());
            }
            any_trigger = true;
            if viewer.is_none() {
                let fetched = github::fetch_viewer().map_err(|err| format!("{err:#}"));
                viewer = fetched.as_ref().ok().cloned();
                output.viewer = Some(fetched);
            }
            github::fetch_status_changes(&board.id, &ids)
                .inspect_err(|err| eprintln!("warning: couldn't read moves on {url}: {err:#}"))
                .ok()
        });
        if let (Ok(board), Some(found), Some(viewer)) = (&board, &changes, &viewer) {
            check_access(board, found, viewer, &input.known_access, &mut output.access);
        }
        output.boards.push(BoardFetch {
            url,
            board,
            changes,
        });
    }
    if any_trigger && let Some(projects) = input.projects {
        output.remotes = Some(
            projects
                .into_iter()
                .map(|(project_id, path)| ProjectRemotes {
                    remotes: codescope_core::git::remote_urls(Path::new(&path))
                        .unwrap_or_default(),
                    project_id,
                    path,
                })
                .collect(),
        );
    }
    output
}

fn check_access(
    board: &Board,
    changes: &HashMap<String, StatusChange>,
    viewer: &Viewer,
    known: &HashSet<(String, String)>,
    found: &mut Vec<((String, String), bool)>,
) {
    for item in &board.items {
        let (Some(id), Some(repository)) = (&item.content_id, &item.repository) else {
            continue;
        };
        let Some(login) = changes
            .get(id)
            .and_then(|change| mover_to_check(item, change, &viewer.login))
        else {
            continue;
        };
        let key = access_key(repository, login);
        if known.contains(&key) || found.iter().any(|(k, _)| *k == key) {
            continue;
        }
        match github::can_write(repository, login) {
            Ok(can_write) => found.push((key, can_write)),
            Err(err) => {
                eprintln!("warning: couldn't check {login}'s access to {repository}: {err:#}")
            }
        }
    }
}

fn prepare_worktree(job: &Job) -> anyhow::Result<()> {
    if job.create {
        add_dispatch_worktree(&job.repo, &job.path, &job.branch, &job.remote)?;
    }
    exclude_handover_files(&job.path)?;
    let home = codescope_core::paths::home_dir();
    let skill = find_skill(&job.path, home.as_deref(), job.record.round.skill()).map(|skill| {
        skill
            .strip_prefix(&job.path)
            .map(|relative| relative.to_string_lossy().replace('\\', "/"))
            .unwrap_or_else(|_| skill.to_string_lossy().into_owned())
    });
    write_task_file(&job.path, &task_file(&job.record, skill.as_deref()))
}

fn claim_on_board(job: &Job, viewer_id: &str) -> Vec<String> {
    let mut errors = Vec::new();
    if let Some(content_id) = &job.assign_to
        && let Err(err) = github::assign(content_id, viewer_id)
    {
        errors.push(format!("couldn't assign it to you: {err:#}"));
    }
    if let Err(err) = github::set_status(&job.board, &job.record.item_id, &job.progress_column_id) {
        errors.push(format!("couldn't move it: {err:#}"));
    }
    errors
}

impl AppShell {
    /// The read the board poll is about to make.
    pub(crate) fn board_read_input(&mut self) -> ReadInput {
        let state = &self.board.dispatch;
        let remotes_due = state
            .remotes_read_at
            .is_none_or(|at| at.elapsed() >= REMOTES_MAX_AGE)
            || state.remotes.len() != self.local_projects().len();
        ReadInput {
            boards: self
                .settings
                .boards
                .iter()
                .map(|b| (b.url.clone(), b.columns.clone()))
                .collect(),
            viewer: state.viewer.clone(),
            known_access: state.access.keys().cloned().collect(),
            projects: remotes_due.then(|| self.local_projects()),
        }
    }

    fn local_projects(&self) -> Vec<(String, String)> {
        self.projects
            .projects
            .iter()
            .filter(|p| p.kind == ProjectKind::Local && !p.path.is_empty())
            .map(|p| (p.id.clone(), p.path.clone()))
            .collect()
    }

    /// Take in what a board read found besides the boards themselves.
    pub(crate) fn accept_dispatch_read(&mut self, output: &mut ReadOutput) {
        let state = &mut self.board.dispatch;
        match output.viewer.take() {
            Some(Ok(viewer)) => {
                state.viewer = Some(viewer);
                state.viewer_error = None;
            }
            Some(Err(err)) => state.viewer_error = Some(err),
            None => {}
        }
        state.access.extend(output.access.drain(..));
        if let Some(remotes) = output.remotes.take() {
            state.remotes = remotes;
            state.remotes_read_at = Some(Instant::now());
        }
    }

    pub(crate) fn dispatch_error(&self) -> Option<&str> {
        self.board.dispatch.viewer_error.as_deref()
    }

    fn dispatch_records(&self) -> Vec<(String, PathBuf, Option<String>, WorktreeDispatch)> {
        self.projects
            .projects
            .iter()
            .flat_map(|p| {
                p.worktrees.iter().filter_map(|wt| {
                    let dispatch = wt.dispatch.clone()?;
                    Some((p.id.clone(), PathBuf::from(&wt.path), wt.branch.clone(), dispatch))
                })
            })
            .collect()
    }

    fn running_dispatches(&self) -> usize {
        let on_board = self
            .dispatch_records()
            .iter()
            .filter(|(_, _, _, record)| {
                let Some(read) = self.board.reads_by_url.get(&record.board_url) else {
                    return false;
                };
                let settings = self.settings.boards.iter().find(|b| b.url == record.board_url);
                let (Some(board), Some(settings)) = (&read.last_good, settings) else {
                    return false;
                };
                let change = read.changes.as_ref().and_then(|c| c.get(&record.content_id));
                board
                    .items
                    .iter()
                    .find(|item| item.id == record.item_id)
                    .is_some_and(|item| {
                        let mapping = effective_mapping(board, &settings.columns);
                        counts_towards_limit(board, &mapping, item, record, change)
                    })
            })
            .count();
        on_board + self.board.dispatch.starting.len()
    }

    /// Decide every item in a trigger column and start the rounds that are due.
    pub(crate) fn run_dispatch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(viewer) = self.board.dispatch.viewer.clone() else {
            return;
        };
        let records = self.dispatch_records();
        let mut running = self.running_dispatches();
        let limit = self.settings.dispatch_limit;
        let mut holds = HashMap::new();
        let mut decided = HashSet::new();
        let mut starts = Vec::new();
        let state = &self.board.dispatch;
        for settings in &self.settings.boards {
            let Some(read) = self.board.reads_by_url.get(&settings.url) else {
                continue;
            };
            let (Some(board), Some(changes)) = (&read.last_good, &read.changes) else {
                continue;
            };
            let mapping = effective_mapping(board, &settings.columns);
            for item in &board.items {
                if state.starting.contains_key(&item.id) {
                    continue;
                }
                let record = records
                    .iter()
                    .find(|(_, _, _, r)| r.board_url == settings.url && r.item_id == item.id)
                    .map(|(_, _, _, r)| r);
                let change = item.content_id.as_ref().and_then(|id| changes.get(id));
                let mover_can_write = change
                    .and_then(|c| mover_to_check(item, c, &viewer.login))
                    .zip(item.repository.as_deref())
                    .and_then(|(login, repository)| {
                        state.access.get(&access_key(repository, login)).copied()
                    });
                let facts = Facts {
                    viewer: &viewer.login,
                    change,
                    mover_can_write,
                    board_added_at: &settings.added_at,
                    projects: item
                        .repository
                        .as_deref()
                        .map(|r| projects_for_repository(&state.remotes, r).len())
                        .unwrap_or(0),
                    dispatch: record,
                    running,
                    limit,
                };
                let decision = decide(board, &mapping, item, &facts);
                if decision.is_some() {
                    decided.insert(item.id.clone());
                }
                match decision {
                    None => {}
                    Some(Err(hold)) => {
                        holds.insert(item.id.clone(), hold);
                    }
                    Some(Ok(round)) => {
                        let Some(change) = change else { continue };
                        let failed_before = state
                            .failed
                            .get(&item.id)
                            .is_some_and(|(at, _)| *at == change.created_at);
                        if failed_before {
                            continue;
                        }
                        running += 1;
                        starts.push(Start {
                            board: board.clone(),
                            board_url: settings.url.clone(),
                            item: item.clone(),
                            round,
                            trigger_at: change.created_at.clone(),
                        });
                    }
                }
            }
        }
        self.board.dispatch.holds = holds;
        self.board.dispatch.failed.retain(|id, _| decided.contains(id));
        for start in starts {
            self.start_dispatch(start, &viewer, window, cx);
        }
    }

    fn fail_dispatch(
        &mut self,
        item_id: &str,
        number: u32,
        trigger_at: &str,
        message: String,
        cx: &mut Context<Self>,
    ) {
        let state = &mut self.board.dispatch;
        state.starting.remove(item_id);
        state
            .failed
            .insert(item_id.to_string(), (trigger_at.to_string(), message.clone()));
        self.push_toast(
            ToastKind::Err,
            format!("Couldn't start #{number}"),
            Some(message.into()),
            cx,
        );
    }

    fn start_dispatch(
        &mut self,
        start: Start,
        viewer: &Viewer,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Start {
            board,
            board_url,
            item,
            round,
            trigger_at,
        } = start;
        let (Some(number), Some(repository), Some(content_id), Some(url)) = (
            item.number,
            item.repository.clone(),
            item.content_id.clone(),
            item.url.clone(),
        ) else {
            return;
        };
        let matched = projects_for_repository(&self.board.dispatch.remotes, &repository)
            .first()
            .map(|(project, remote)| (project.project_id.clone(), remote.to_string()));
        let Some((project_id, remote)) = matched else {
            return;
        };
        let Some(project) = self.projects.project(&project_id).cloned() else {
            return;
        };
        let mapping = self
            .settings
            .boards
            .iter()
            .find(|b| b.url == board_url)
            .map(|b| effective_mapping(&board, &b.columns))
            .unwrap_or_default();
        let Some(progress_column_id) =
            mapped_column(&board, &mapping, round.progress_role()).map(|c| c.id.clone())
        else {
            return;
        };

        let profile = project
            .default_agent_id
            .as_deref()
            .and_then(|id| self.agent_registry.get_by_id(id))
            .or_else(|| self.agent_registry.get_default())
            .cloned();
        let command = profile.as_ref().and_then(|profile| {
            codescope_core::build_new_session_auto_type(profile)
                .and_then(|launch| launch_with_task(&launch, &task_message(number)))
        });
        let (Some(profile), Some(command)) = (profile, command) else {
            let message = "The project's agent can't be handed a task yet. Claude Code, Codex, \
                           Gemini CLI, Copilot and Pi can."
                .to_string();
            self.fail_dispatch(&item.id, number, &trigger_at, message, cx);
            return;
        };

        let existing = self
            .dispatch_records()
            .into_iter()
            .find(|(_, _, _, r)| r.board_url == board_url && r.item_id == item.id);
        let (create, path, branch) = match &existing {
            Some((_, path, branch, _)) => (
                false,
                path.clone(),
                branch.clone().unwrap_or_else(|| branch_name(number, &item.title)),
            ),
            None => {
                let branch = branch_name(number, &item.title);
                (true, PathBuf::from(project.worktree_root_path()).join(&branch), branch)
            }
        };
        let mut title = codescope_core::TabTitle::project(project.name.clone());
        title.set_branch(&branch);
        let launch = Launch {
            project_id: project_id.clone(),
            title,
            agent_id: profile.id.clone(),
            command,
        };
        let job = Job {
            repo: PathBuf::from(&project.path),
            path,
            branch,
            remote,
            create,
            record: WorktreeDispatch {
                board_url,
                item_id: item.id.clone(),
                content_id: content_id.clone(),
                repository,
                number,
                title: item.title.clone(),
                url,
                round,
                trigger_at: trigger_at.clone(),
            },
            assign_to: item.assignees.is_empty().then_some(content_id),
            board,
            progress_column_id,
        };
        self.board.dispatch.starting.insert(item.id.clone(), round);
        self.board.dispatch.failed.remove(&item.id);
        cx.notify();

        let viewer_id = viewer.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (job, result) = cx
                .background_spawn(async move {
                    let result = prepare_worktree(&job)
                        .map(|()| claim_on_board(&job, &viewer_id))
                        .map_err(|err| format!("{err:#}"));
                    (job, result)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.finish_dispatch(job, launch, result, window, cx);
            });
        })
        .detach();
    }

    fn finish_dispatch(
        &mut self,
        job: Job,
        launch: Launch,
        result: Result<Vec<String>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let record = job.record;
        let board_errors = match result {
            Ok(errors) => errors,
            Err(message) => {
                self.fail_dispatch(&record.item_id, record.number, &record.trigger_at, message, cx);
                return;
            }
        };
        let path = job.path.to_string_lossy().into_owned();
        let saved = ProjectsConfig::load(&self.paths)
            .map_err(|err| format!("{err:#}"))
            .and_then(|mut next| {
                let project_id = &launch.project_id;
                if !record_dispatch(&mut next, project_id, &path, &job.branch, record.clone()) {
                    return Err("the project no longer tracks the worktree".to_string());
                }
                next.save(&self.paths).map_err(|err| format!("{err:#}"))?;
                Ok(next)
            });
        let next = match saved {
            Ok(next) => next,
            Err(message) => {
                self.fail_dispatch(&record.item_id, record.number, &record.trigger_at, message, cx);
                return;
            }
        };
        self.projects = next.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.replace_projects(next);
            cx.notify();
        });
        self.board.dispatch.starting.remove(&record.item_id);
        self.hand_over(&job.path, launch, record.number, window, cx);

        let started = format!("{} started on #{}", record.round.label(), record.number);
        if board_errors.is_empty() {
            self.push_toast(ToastKind::Ok, started, Some(record.title.into()), cx);
        } else {
            let detail = format!("The board isn't up to date: {}", board_errors.join("; "));
            self.push_toast(ToastKind::Err, started, Some(detail.into()), cx);
        }
        cx.notify();
    }

    /// Give the session on `path` its task: typed into a tab whose Claude Code waits for a
    /// prompt, else in a new tab, unless something else runs in a tab there. Focus stays where it
    /// was.
    fn hand_over(
        &mut self,
        path: &Path,
        launch: Launch,
        number: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = task_message(number);
        let tabs = self.tabs_at_handover(path, cx);
        let idle = tabs.iter().find(|(_, at)| *at == TabAtHandover::IdleAgent);
        if let Some((terminal, _)) = idle {
            terminal.update(cx, |term, _| {
                let mut bytes = task.into_bytes();
                bytes.push(b'\r');
                term.write_input(bytes);
            });
            return;
        }
        if tabs.iter().any(|(_, at)| *at == TabAtHandover::Busy) {
            self.push_toast(
                ToastKind::Info,
                format!("#{number} waits for its tab"),
                Some(format!("Something runs there now. When it's done, send: {task}").into()),
                cx,
            );
            return;
        }
        let before = self
            .groups
            .get(self.focused_group)
            .map(|g| (self.focused_group, g.active_tab));
        let focused = window.focused(cx);
        self.spawn_tab_in(
            Some(path.to_path_buf()),
            Some(launch.title),
            Some(launch.command.into()),
            Some(launch.agent_id.into()),
            None,
            None,
            window,
            cx,
        );
        if let Some((group, tab)) = before {
            self.activate_tab(group, tab, window, cx);
        }
        if let Some(focused) = focused {
            focused.focus(window);
        }
    }

    fn tabs_at_handover(
        &self,
        path: &Path,
        cx: &Context<Self>,
    ) -> Vec<(gpui::Entity<TerminalView>, TabAtHandover)> {
        let wanted = path.to_string_lossy();
        let on_path: Vec<_> = self
            .groups
            .iter()
            .flat_map(|g| g.tabs.iter())
            .filter(|t| t.remote_project_id.is_none())
            .filter(|t| {
                t.working_directory
                    .as_deref()
                    .is_some_and(|wd| paths_match(&wd.to_string_lossy(), &wanted))
            })
            .collect();
        if on_path.is_empty() {
            return Vec::new();
        }
        let table = process_tree::process_table();
        let sessions = live_session::default_sessions_dir();
        on_path
            .into_iter()
            .map(|tab| {
                let terminal = tab.terminal.read(cx);
                let pids = terminal
                    .child_pid()
                    .map(|shell| process_tree::descendants(shell, &table))
                    .unwrap_or_default();
                let claude = (tab.agent_id == Some(AgentId::Claude))
                    .then_some(sessions.as_deref())
                    .flatten()
                    .and_then(|dir| live_session::process_for_pids(dir, &pids, &wanted));
                let at = tab_at_handover(&pids, claude.as_ref(), terminal.typed_since_submit());
                (tab.terminal.clone(), at)
            })
            .collect()
    }

    /// What each card on the board at `board_url` says about dispatch, by item id.
    pub(crate) fn card_notes(&self, board_url: &str) -> HashMap<String, CardNote> {
        let state = &self.board.dispatch;
        let mut notes: HashMap<String, CardNote> = self
            .dispatch_records()
            .into_iter()
            .filter(|(_, _, _, r)| r.board_url == board_url)
            .map(|(_, _, branch, r)| {
                let text = match branch {
                    Some(branch) => format!("{} · {branch}", r.round.label()),
                    None => r.round.label().to_string(),
                };
                (
                    r.item_id,
                    CardNote {
                        text: text.into(),
                        tone: NoteTone::Working,
                    },
                )
            })
            .collect();
        for (item_id, hold) in &state.holds {
            if !hold.is_quiet() {
                notes.insert(
                    item_id.clone(),
                    CardNote {
                        text: hold.label().into(),
                        tone: NoteTone::Waiting,
                    },
                );
            }
        }
        for (item_id, (_, message)) in &state.failed {
            notes.insert(
                item_id.clone(),
                CardNote {
                    text: format!("Couldn't start: {message}").into(),
                    tone: NoteTone::Failed,
                },
            );
        }
        for (item_id, round) in &state.starting {
            notes.insert(
                item_id.clone(),
                CardNote {
                    text: format!("Starting {}…", round.label().to_lowercase()).into(),
                    tone: NoteTone::Working,
                },
            );
        }
        notes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude(status: &str) -> LiveProcess {
        LiveProcess {
            pid: 7,
            session_id: "s".into(),
            cwd: "D:/wt".into(),
            version: None,
            status: Some(status.into()),
        }
    }

    #[test]
    fn only_an_idle_claude_gets_the_task_typed_and_a_bare_shell_is_no_session() {
        let idle = claude("idle");
        let busy = claude("busy");
        assert_eq!(tab_at_handover(&[], None, false), TabAtHandover::Shell);
        assert_eq!(tab_at_handover(&[7], Some(&idle), false), TabAtHandover::IdleAgent);
        assert_eq!(tab_at_handover(&[7], Some(&idle), true), TabAtHandover::Busy);
        assert_eq!(tab_at_handover(&[7], Some(&busy), false), TabAtHandover::Busy);
        assert_eq!(tab_at_handover(&[9], None, false), TabAtHandover::Busy);
    }
}
