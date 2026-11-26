//! Dispatch (ADR-0027): after each board read, start a round for every item the user moved
//! into a trigger column, in a worktree of its own with an agent session on it. When a research
//! round's session is idle with its findings written, post them on the issue.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codescope_core::agents::claude::live_session;
use codescope_core::board::dispatch::{
    Facts, Hold, PostedFindings, ProjectRemotes, Round, StatusChange, WorktreeDispatch,
    add_dispatch_worktree, branch_name, clear_findings, counts_towards_limit, decide,
    exclude_handover_files, find_skill, launch_with_task, mover_to_check, moves_to_needs_decision,
    projects_for_repository, task_file, task_message, trigger_content_ids, write_task_file,
};
use codescope_core::board::findings::{
    FileStamp, Outcome, Problem, comment_body, problems_list, problems_message, read_findings,
};
use codescope_core::board::github::{self, Viewer};
use codescope_core::board::{
    Board, BoardItem, BoardRef, ColumnMapping, ColumnRole, effective_mapping, mapped_column,
};
use codescope_core::path_canon::paths_match;
use codescope_core::process_tree;
use codescope_core::projects::{ProjectKind, ProjectsConfig};
use codescope_core::worktree_create::{PendingId, record_dispatch, record_posted_findings};
use codescope_terminal::TerminalView;
use gpui::{App, AppContext as _, Context, Entity, SharedString, Window};

use super::{AppShell, ToastKind};

const REMOTES_MAX_AGE: Duration = Duration::from_secs(300);
const VIEWER_MAX_AGE: Duration = Duration::from_secs(300);
const ACCESS_RECHECK_AFTER: Duration = Duration::from_secs(300);
const ACCESS_EXPIRES_AFTER: Duration = Duration::from_secs(600);
/// Text and Enter in one write can reach an agent's prompt as a paste, whose Enter is a newline.
const SUBMIT_AFTER_TYPING: Duration = Duration::from_millis(200);
const LAUNCH_GRACE: Duration = Duration::from_secs(30);
const MAX_REMINDERS: u32 = 3;
const POST_RETRY: Duration = Duration::from_secs(120);

#[derive(Default)]
pub(crate) struct DispatchState {
    viewer: Option<Viewer>,
    viewer_read_at: Option<Instant>,
    viewer_error: Option<String>,
    /// Write access by `(repository, login)`, both lower case, and when it was read.
    access: HashMap<(String, String), (bool, Instant)>,
    remotes: Vec<ProjectRemotes>,
    remotes_read_at: Option<Instant>,
    /// Item id → the round being started.
    starting: HashMap<String, Round>,
    /// Item id → the move that failed to start, and why.
    failed: HashMap<String, (String, String)>,
    holds: HashMap<String, Hold>,
    /// Rounds whose findings are being posted.
    posting: HashSet<RoundKey>,
    /// Round → when posting or saving its findings failed, and the card's line about it.
    post_errors: HashMap<RoundKey, (Instant, String)>,
    /// Round → what is wrong with its findings and what the session was told.
    reminders: HashMap<RoundKey, Reminder>,
}

/// A round: the board item's id and when the move that started the round was made.
type RoundKey = (String, String);

fn round_key(record: &WorktreeDispatch) -> RoundKey {
    (record.item_id.clone(), record.trigger_at.clone())
}

struct Reminder {
    problems: String,
    /// The version of the file the last reminder was about.
    file: Option<FileStamp>,
    sent: u32,
}

enum SessionAt {
    Working,
    Done(Option<Entity<TerminalView>>),
}

struct Posting {
    record: WorktreeDispatch,
    outcome: Outcome,
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
    /// Write access by `(repository, login)`; `None` where the check failed.
    access: Vec<((String, String), Option<bool>)>,
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
    NeedsYou,
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
    /// The create the sidebar shows as in flight, while `create` runs.
    pending: Option<PendingId>,
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
    IdleClaude,
    Draft,
    Busy,
    Other,
}

fn running_under(shell: Option<u32>, table: &[(u32, u32)]) -> Option<Vec<u32>> {
    if table.is_empty() {
        return None;
    }
    Some(process_tree::descendants(shell?, table))
}

fn shell_gone(shell: Option<u32>, table: &[(u32, u32)]) -> bool {
    !table.is_empty() && shell.is_some_and(|shell| table.iter().all(|&(pid, _)| pid != shell))
}

struct TabProbe<'a> {
    restarting: bool,
    launched_lately: bool,
    shell_gone: bool,
    running: Option<&'a [u32]>,
    claude_idle: Option<bool>,
    typed: bool,
}

fn tab_at_handover(tab: &TabProbe) -> TabAtHandover {
    if tab.restarting {
        return TabAtHandover::Busy;
    }
    let state = match (tab.running, tab.claude_idle) {
        (Some([]), _) => TabAtHandover::Shell,
        (Some(_), _) if tab.shell_gone => TabAtHandover::Other,
        (Some(_), Some(true)) if tab.typed => TabAtHandover::Draft,
        (Some(_), Some(true)) => TabAtHandover::IdleClaude,
        (Some(_), Some(false)) => TabAtHandover::Busy,
        _ => TabAtHandover::Other,
    };
    match state {
        TabAtHandover::Shell | TabAtHandover::Other if tab.launched_lately => TabAtHandover::Busy,
        state => state,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Handover {
    Type(usize),
    NewTab,
    Wait(Hold),
}

fn handover_to(tabs: &[TabAtHandover]) -> Handover {
    if let Some(index) = tabs.iter().position(|t| *t == TabAtHandover::IdleClaude) {
        Handover::Type(index)
    } else if tabs.contains(&TabAtHandover::Busy) {
        Handover::Wait(Hold::SessionBusy)
    } else if tabs.contains(&TabAtHandover::Draft) {
        Handover::Wait(Hold::UnsentInput)
    } else if tabs.contains(&TabAtHandover::Other) {
        Handover::Wait(Hold::OtherProgram)
    } else {
        Handover::NewTab
    }
}

enum HandoverTarget {
    Type(Entity<TerminalView>),
    NewTab,
    Wait(Hold),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FindingsGate {
    Settled(Option<usize>),
    Wait,
}

fn findings_gate(tabs: &[(TabAtHandover, bool)]) -> FindingsGate {
    let settled = tabs.iter().all(|(state, turn_ended)| match state {
        TabAtHandover::Shell | TabAtHandover::IdleClaude => true,
        TabAtHandover::Other => *turn_ended,
        TabAtHandover::Busy | TabAtHandover::Draft => false,
    });
    if !settled {
        return FindingsGate::Wait;
    }
    FindingsGate::Settled(tabs.iter().position(|(state, _)| *state == TabAtHandover::IdleClaude))
}

struct Place {
    create: bool,
    path: PathBuf,
    branch: String,
}

fn usable_access(answer: Option<&(bool, Instant)>, now: Instant) -> Option<bool> {
    answer
        .filter(|(_, at)| now.saturating_duration_since(*at) < ACCESS_EXPIRES_AFTER)
        .map(|(can_write, _)| *can_write)
}

fn access_key(repository: &str, login: &str) -> (String, String) {
    (repository.to_ascii_lowercase(), login.to_ascii_lowercase())
}

/// Read every board, and for items in trigger columns their last moves, who is signed in to
/// `gh`, the write access of teammates who moved them, and the projects' remotes.
pub(crate) fn read_boards(input: ReadInput) -> ReadOutput {
    let mut output = ReadOutput::default();
    let mut viewer = input.viewer;
    if viewer.is_none() {
        let fetched = github::fetch_viewer().map_err(|err| format!("{err:#}"));
        viewer = fetched.as_ref().ok().cloned();
        output.viewer = Some(fetched);
    }
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
    found: &mut Vec<((String, String), Option<bool>)>,
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
        let answer = github::can_write(repository, login)
            .inspect_err(|err| {
                eprintln!("warning: couldn't check {login}'s access to {repository}: {err:#}")
            })
            .ok();
        found.push((key, answer));
    }
}

fn prepare_worktree(job: &Job) -> anyhow::Result<()> {
    if job.create {
        add_dispatch_worktree(&job.repo, &job.path, &job.branch, &job.remote)?;
    }
    exclude_handover_files(&job.path)?;
    if job.record.round == Round::Research {
        clear_findings(&job.path)?;
    }
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
    pub(crate) fn board_read_input(&mut self, cx: &App) -> ReadInput {
        let state = &self.board.dispatch;
        let local = self.local_projects(cx);
        let read_projects: Vec<(&str, &str)> = state
            .remotes
            .iter()
            .map(|r| (r.project_id.as_str(), r.path.as_str()))
            .collect();
        let same_projects = read_projects.len() == local.len()
            && local.iter().all(|(id, path)| read_projects.contains(&(id.as_str(), path.as_str())));
        let remotes_due = !same_projects
            || state.remotes_read_at.is_none_or(|at| at.elapsed() >= REMOTES_MAX_AGE);
        ReadInput {
            boards: self
                .settings
                .boards
                .iter()
                .map(|b| (b.url.clone(), b.columns.clone()))
                .collect(),
            viewer: state
                .viewer_read_at
                .is_some_and(|at| at.elapsed() < VIEWER_MAX_AGE)
                .then(|| state.viewer.clone())
                .flatten(),
            known_access: state
                .access
                .iter()
                .filter(|(_, (_, at))| at.elapsed() < ACCESS_RECHECK_AFTER)
                .map(|(key, _)| key.clone())
                .collect(),
            projects: remotes_due.then_some(local),
        }
    }

    /// The projects as the sidebar has them, which follows its own edits as well as this shell's.
    fn current_projects<'a>(&self, cx: &'a App) -> &'a ProjectsConfig {
        self.sidebar.read(cx).projects()
    }

    fn local_projects(&self, cx: &App) -> Vec<(String, String)> {
        self.current_projects(cx)
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
                let switched = state
                    .viewer
                    .as_ref()
                    .is_some_and(|before| !before.login.eq_ignore_ascii_case(&viewer.login));
                if switched {
                    state.access.clear();
                    state.holds.clear();
                    state.failed.clear();
                }
                state.viewer = Some(viewer);
                state.viewer_read_at = Some(Instant::now());
                state.viewer_error = None;
            }
            Some(Err(err)) => state.viewer_error = Some(err),
            None => {}
        }
        let now = Instant::now();
        for (key, answer) in output.access.drain(..) {
            match answer {
                Some(can_write) => state.access.insert(key, (can_write, now)),
                None => state.access.remove(&key),
            };
        }
        if let Some(remotes) = output.remotes.take() {
            state.remotes = remotes;
            state.remotes_read_at = Some(Instant::now());
        }
    }

    pub(crate) fn dispatch_error(&self) -> Option<&str> {
        self.board.dispatch.viewer_error.as_deref()
    }

    fn dispatch_records(
        &self,
        cx: &App,
    ) -> Vec<(String, PathBuf, Option<String>, WorktreeDispatch)> {
        self.current_projects(cx)
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

    fn place_of(&self, board_url: &str, item: &BoardItem, cx: &App) -> Option<Place> {
        let number = item.number?;
        let existing = self
            .dispatch_records(cx)
            .into_iter()
            .find(|(_, _, _, r)| r.board_url == board_url && r.item_id == item.id);
        if let Some((_, path, branch, _)) = existing {
            let branch = branch.unwrap_or_else(|| branch_name(number, &item.title));
            return Some(Place {
                create: false,
                path,
                branch,
            });
        }
        let remotes = &self.board.dispatch.remotes;
        let (matched, _) = *projects_for_repository(remotes, item.repository.as_deref()?).first()?;
        let project = self.current_projects(cx).project(&matched.project_id)?;
        let branch = branch_name(number, &item.title);
        Some(Place {
            create: true,
            path: PathBuf::from(project.worktree_root_path()).join(&branch),
            branch,
        })
    }

    fn running_dispatches(&self, cx: &App) -> usize {
        let on_board = self
            .dispatch_records(cx)
            .iter()
            .filter(|(_, _, _, record)| record.findings.is_none())
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
        let records = self.dispatch_records(cx);
        let mut running = self.running_dispatches(cx);
        let limit = self.settings.dispatch_limit;
        let mut holds = HashMap::new();
        let mut seen = HashSet::new();
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
                seen.insert(item.id.clone());
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
                        let known = state.access.get(&access_key(repository, login));
                        usable_access(known, Instant::now())
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
                        let target = self
                            .place_of(&settings.url, item, cx)
                            .map(|place| self.handover_target(&place.path, cx));
                        if let Some(HandoverTarget::Wait(hold)) = target {
                            holds.insert(item.id.clone(), hold);
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
        let state = &mut self.board.dispatch;
        state.holds.retain(|id, _| !seen.contains(id));
        state.holds.extend(holds);
        state.failed.retain(|id, _| decided.contains(id) || !seen.contains(id));
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
        let Some(project) = self.current_projects(cx).project(&project_id).cloned() else {
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

        let Some(Place { create, path, branch }) = self.place_of(&board_url, &item, cx) else {
            return;
        };
        let pending = if create {
            let path = path.to_string_lossy().into_owned();
            let begun = self.sidebar.update(cx, |sidebar, cx| {
                cx.notify();
                sidebar.pending_worktrees_mut().begin(&project_id, &path, &branch)
            });
            match begun {
                Ok(id) => Some(id),
                Err(err) => {
                    self.fail_dispatch(&item.id, number, &trigger_at, err.to_string(), cx);
                    return;
                }
            }
        } else {
            None
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
            pending,
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
                findings: None,
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
            let (job, prepared) = cx
                .background_spawn(async move {
                    let prepared = prepare_worktree(&job).map_err(|err| format!("{err:#}"));
                    (job, prepared)
                })
                .await;
            let Ok(Some(job)) = this.update_in(cx, |this, window, cx| {
                this.record_dispatch_start(job, launch, prepared, window, cx)
            }) else {
                return;
            };
            let (job, board_errors) = cx
                .background_spawn(async move {
                    let errors = claim_on_board(&job, &viewer_id);
                    (job, errors)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.report_dispatch_start(&job.record, board_errors, cx);
            });
        })
        .detach();
    }

    /// Keep the started round on the worktree's row and hand the session its task; the job comes
    /// back to be claimed on the board only once that is saved.
    fn record_dispatch_start(
        &mut self,
        job: Job,
        launch: Launch,
        prepared: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Job> {
        if let Some(id) = job.pending {
            self.sidebar.update(cx, |sidebar, cx| {
                sidebar.pending_worktrees_mut().finish(id);
                sidebar.note_worktree_edit();
                cx.notify();
            });
        }
        let record = &job.record;
        if let Err(message) = prepared {
            self.fail_dispatch(&record.item_id, record.number, &record.trigger_at, message, cx);
            return None;
        }
        let terminal = match self.handover_target(&job.path, cx) {
            HandoverTarget::Type(terminal) => Some(terminal),
            HandoverTarget::NewTab => None,
            HandoverTarget::Wait(hold) => {
                let state = &mut self.board.dispatch;
                state.starting.remove(&record.item_id);
                state.holds.insert(record.item_id.clone(), hold);
                cx.notify();
                return None;
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
                return None;
            }
        };
        self.projects = next.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.replace_projects(next);
            cx.notify();
        });
        self.board.dispatch.starting.remove(&record.item_id);
        self.hand_over(&job.path, terminal, launch, record.number, window, cx);
        cx.notify();
        Some(job)
    }

    fn report_dispatch_start(
        &mut self,
        record: &WorktreeDispatch,
        board_errors: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        let started = format!("{} started on #{}", record.round.label(), record.number);
        if board_errors.is_empty() {
            self.push_toast(ToastKind::Ok, started, Some(record.title.clone().into()), cx);
        } else {
            let detail = format!("The board isn't up to date: {}", board_errors.join("; "));
            self.push_toast(ToastKind::Err, started, Some(detail.into()), cx);
        }
        cx.notify();
    }

    /// Give the session on `path` its task: typed into `terminal`, where Claude Code waits for a
    /// prompt, or else in a new tab. Focus stays where it was.
    fn hand_over(
        &mut self,
        path: &Path,
        terminal: Option<Entity<TerminalView>>,
        launch: Launch,
        number: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = terminal {
            Self::submit_line(&terminal, task_message(number), cx);
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

    fn tabs_on(&self, path: &Path) -> Vec<&super::Tab> {
        let wanted = path.to_string_lossy();
        self.groups
            .iter()
            .flat_map(|g| g.tabs.iter())
            .filter(|t| t.remote_project_id.is_none())
            .filter(|t| {
                t.working_directory
                    .as_deref()
                    .is_some_and(|wd| paths_match(&wd.to_string_lossy(), &wanted))
            })
            .collect()
    }

    fn tab_states(&self, tabs: &[&super::Tab], path: &Path, cx: &App) -> Vec<TabAtHandover> {
        if tabs.is_empty() {
            return Vec::new();
        }
        let wanted = path.to_string_lossy();
        let table = process_tree::process_table();
        let sessions = live_session::default_sessions_dir();
        tabs.iter()
            .map(|tab| {
                let terminal = tab.terminal.read(cx);
                let shell = terminal.child_pid();
                let running = running_under(shell, &table);
                let claude_idle = running.as_deref().and_then(|running| {
                    let dir = sessions.as_deref()?;
                    let claude = live_session::process_for_pids(dir, running, &wanted)?;
                    Some(claude.reports_idle()? && !process_tree::is_stopped(claude.pid))
                });
                let relaunched = self.claude_updates.relaunched_at(tab.id);
                let launched_lately = tab.spawned_instant.elapsed() < LAUNCH_GRACE
                    || relaunched.is_some_and(|at| at.elapsed() < LAUNCH_GRACE);
                tab_at_handover(&TabProbe {
                    restarting: self.claude_updates.is_restarting(tab.id),
                    launched_lately,
                    shell_gone: shell_gone(shell, &table),
                    running: running.as_deref(),
                    claude_idle,
                    typed: terminal.typed_since_submit(),
                })
            })
            .collect()
    }

    /// Where a task for the worktree at `path` goes: into a tab where Claude Code waits for a
    /// prompt, into a new tab when nothing runs there, or nowhere yet, with the reason.
    fn handover_target(&self, path: &Path, cx: &App) -> HandoverTarget {
        let on_path = self.tabs_on(path);
        if on_path.is_empty() {
            return HandoverTarget::NewTab;
        }
        match handover_to(&self.tab_states(&on_path, path, cx)) {
            Handover::Type(index) => HandoverTarget::Type(on_path[index].terminal.clone()),
            Handover::NewTab => HandoverTarget::NewTab,
            Handover::Wait(hold) => HandoverTarget::Wait(hold),
        }
    }

    fn submit_line(terminal: &Entity<TerminalView>, line: String, cx: &mut Context<Self>) {
        let terminal = terminal.downgrade();
        cx.spawn(async move |_, cx| {
            let _ = terminal.update(cx, |term, _| term.write_input(line.into_bytes()));
            cx.background_executor().timer(SUBMIT_AFTER_TYPING).await;
            let _ = terminal.update(cx, |term, _| term.write_input(&b"\r"[..]));
        })
        .detach();
    }

    fn read_board(&self, board_url: &str) -> Option<(&Board, ColumnMapping)> {
        let board = self.board.reads_by_url.get(board_url)?.last_good.as_ref()?;
        let settings = self.settings.boards.iter().find(|b| b.url == board_url)?;
        Some((board, effective_mapping(board, &settings.columns)))
    }

    /// Post the findings of every research round whose sessions are done, or tell the session
    /// what to fix first while the item is still in Researching.
    pub(crate) fn collect_findings(&mut self, cx: &mut Context<Self>) {
        let records = self.dispatch_records(cx);
        let awaiting: HashSet<RoundKey> = records
            .iter()
            .filter(|(_, _, _, record)| record.awaits_findings())
            .map(|(_, _, _, record)| round_key(record))
            .collect();
        let state = &mut self.board.dispatch;
        state.reminders.retain(|key, _| awaiting.contains(key));
        state.post_errors.retain(|key, _| awaiting.contains(key));

        for (_, path, _, record) in records {
            let key = round_key(&record);
            let state = &self.board.dispatch;
            let waiting_to_retry = state
                .post_errors
                .get(&key)
                .is_some_and(|(at, _)| at.elapsed() < POST_RETRY);
            if !awaiting.contains(&key) || state.posting.contains(&key) || waiting_to_retry {
                continue;
            }
            let Some((board, mapping)) = self.read_board(&record.board_url) else {
                continue;
            };
            let researching = board
                .items
                .iter()
                .find(|item| item.id == record.item_id)
                .and_then(|item| board.role_of(item, &mapping))
                == Some(ColumnRole::Researching);
            let board = board.clone();
            let Some(file) = read_findings(&path) else {
                continue;
            };
            let SessionAt::Done(waiting_tab) = self.session_at(&path, cx) else {
                continue;
            };
            match file.check() {
                Ok((text, findings)) => {
                    let body = comment_body(text, &findings);
                    let posting = Posting {
                        record,
                        outcome: findings.outcome(),
                    };
                    self.post_findings(posting, body, board, mapping, cx);
                }
                Err(problems) => {
                    let terminal = waiting_tab.filter(|_| researching);
                    self.remind(key, file.stamp, terminal, &problems, cx);
                }
            }
        }
    }

    fn session_at(&self, path: &Path, cx: &App) -> SessionAt {
        let tabs = self.tabs_on(path);
        let states = self.tab_states(&tabs, path, cx);
        let probed: Vec<(TabAtHandover, bool)> = tabs
            .iter()
            .zip(states)
            .map(|(tab, state)| (state, self.turn_ended(tab)))
            .collect();
        match findings_gate(&probed) {
            FindingsGate::Wait => SessionAt::Working,
            FindingsGate::Settled(index) => {
                SessionAt::Done(index.map(|index| tabs[index].terminal.clone()))
            }
        }
    }

    fn turn_ended(&self, tab: &super::Tab) -> bool {
        tab.adopted_session_id
            .as_deref()
            .and_then(|sid| self.telemetry_for(sid))
            .is_some_and(|snap| super::notified_state(&snap) == codescope_core::SessionState::Idle)
    }

    fn remind(
        &mut self,
        key: RoundKey,
        file: FileStamp,
        terminal: Option<Entity<TerminalView>>,
        problems: &[Problem],
        cx: &mut Context<Self>,
    ) {
        let reminder = self.board.dispatch.reminders.entry(key).or_insert(Reminder {
            problems: String::new(),
            file: None,
            sent: 0,
        });
        reminder.problems = problems_list(problems);
        cx.notify();
        if reminder.file == Some(file) || reminder.sent >= MAX_REMINDERS {
            return;
        }
        let Some(terminal) = terminal else {
            return;
        };
        reminder.file = Some(file);
        reminder.sent += 1;
        Self::submit_line(&terminal, problems_message(problems), cx);
    }

    fn post_findings(
        &mut self,
        posting: Posting,
        body: String,
        board: Board,
        mapping: ColumnMapping,
        cx: &mut Context<Self>,
    ) {
        let record = &posting.record;
        let item_id = record.item_id.clone();
        let content_id = record.content_id.clone();
        let since = record.trigger_at.clone();
        self.board.dispatch.posting.insert(round_key(record));
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let failed = |err: anyhow::Error| format!("{err:#}");
                    let before = github::before_posting(&item_id, &content_id, &since, &body)
                        .map_err(failed)?;
                    let url = match before.posted {
                        Some(url) => url,
                        None => github::add_comment(&content_id, &body).map_err(failed)?,
                    };
                    let role = before
                        .column_id
                        .as_deref()
                        .and_then(|id| board.role_of_column(id, &mapping));
                    let moved = if !moves_to_needs_decision(role) {
                        None
                    } else {
                        match mapped_column(&board, &mapping, ColumnRole::NeedsDecision) {
                            Some(column) => github::set_status(&board, &item_id, &column.id)
                                .map_err(failed)
                                .err(),
                            None => Some("the board has no Needs decision column".to_string()),
                        }
                    };
                    Ok::<_, String>((url, moved))
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_posting(posting, result, cx));
        })
        .detach();
    }

    fn finish_posting(
        &mut self,
        posting: Posting,
        result: Result<(String, Option<String>), String>,
        cx: &mut Context<Self>,
    ) {
        let Posting { record, outcome } = posting;
        let key = round_key(&record);
        self.board.dispatch.posting.remove(&key);
        let (comment_url, move_error) = match result {
            Ok(posted) => posted,
            Err(message) => {
                let note = format!("Couldn't post the findings: {message}");
                self.board.dispatch.post_errors.insert(key, (Instant::now(), note));
                self.push_toast(
                    ToastKind::Err,
                    format!("Couldn't post the findings of #{}", record.number),
                    Some(message.into()),
                    cx,
                );
                return;
            }
        };

        let posted = PostedFindings {
            outcome,
            comment_url,
        };
        let saved = ProjectsConfig::load(&self.paths)
            .map_err(|err| format!("{err:#}"))
            .and_then(|mut next| {
                if !record_posted_findings(&mut next, &record.item_id, &record.trigger_at, posted) {
                    return Ok(None);
                }
                next.save(&self.paths).map_err(|err| format!("{err:#}"))?;
                Ok(Some(next))
            });
        match saved {
            Ok(Some(next)) => {
                self.projects = next.clone();
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.replace_projects(next);
                    cx.notify();
                });
            }
            Ok(None) => {}
            Err(err) => {
                let note = format!("Findings posted, not saved: {err}");
                self.board.dispatch.post_errors.insert(key, (Instant::now(), note));
            }
        }

        let title = format!("Findings on #{}", record.number);
        let next_step = match outcome {
            Outcome::DecisionPoints => "Answer the decision points on the issue.",
            Outcome::Findings => "Move the item to Ready to implement it.",
        };
        let detail = match &move_error {
            Some(err) => format!("{next_step} Couldn't move it to Needs decision: {err}"),
            None => next_step.to_string(),
        };
        let kind = if move_error.is_some() {
            ToastKind::Err
        } else {
            ToastKind::Ok
        };
        self.push_toast(kind, title.clone(), Some(detail.clone().into()), cx);
        self.push_notification(
            crate::notifications::NotificationKind::SessionReady,
            title,
            detail,
            Some(record.title.clone().into()),
            cx,
        );
        cx.notify();
    }

    /// What each card on the board at `board_url` says about dispatch, by item id.
    pub(crate) fn card_notes(&self, board_url: &str, cx: &App) -> HashMap<String, CardNote> {
        let state = &self.board.dispatch;
        let records: Vec<_> = self
            .dispatch_records(cx)
            .into_iter()
            .filter(|(_, _, _, r)| r.board_url == board_url)
            .collect();
        let rounds: HashSet<RoundKey> = records.iter().map(|(_, _, _, r)| round_key(r)).collect();
        let mut notes: HashMap<String, CardNote> = records
            .into_iter()
            .map(|(_, _, branch, r)| {
                let posted = r.findings.as_ref().filter(|_| r.round == Round::Research);
                let note = match (posted.map(|p| p.outcome), branch) {
                    (Some(Outcome::DecisionPoints), _) => CardNote {
                        text: "Decision points posted · answer on the issue".into(),
                        tone: NoteTone::NeedsYou,
                    },
                    (Some(Outcome::Findings), _) => CardNote {
                        text: "Findings posted · move it to Ready".into(),
                        tone: NoteTone::NeedsYou,
                    },
                    (None, Some(branch)) => CardNote {
                        text: format!("{} · {branch}", r.round.label()).into(),
                        tone: NoteTone::Working,
                    },
                    (None, None) => CardNote {
                        text: r.round.label().into(),
                        tone: NoteTone::Working,
                    },
                };
                (r.item_id, note)
            })
            .collect();
        for key in state.posting.iter().filter(|key| rounds.contains(*key)) {
            notes.insert(
                key.0.clone(),
                CardNote {
                    text: "Posting the findings…".into(),
                    tone: NoteTone::Working,
                },
            );
        }
        for (key, reminder) in state.reminders.iter().filter(|(key, _)| rounds.contains(*key)) {
            notes.insert(
                key.0.clone(),
                CardNote {
                    text: format!("Findings incomplete: {}", reminder.problems).into(),
                    tone: NoteTone::Failed,
                },
            );
        }
        for (key, (_, note)) in state.post_errors.iter().filter(|(key, _)| rounds.contains(*key)) {
            notes.insert(
                key.0.clone(),
                CardNote {
                    text: note.clone().into(),
                    tone: NoteTone::Failed,
                },
            );
        }
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

    fn tab(running: Option<&[u32]>, claude_idle: Option<bool>, typed: bool) -> TabAtHandover {
        tab_at_handover(&TabProbe {
            restarting: false,
            launched_lately: false,
            shell_gone: false,
            running,
            claude_idle,
            typed,
        })
    }

    #[test]
    fn a_bare_shell_is_no_session_and_only_an_idle_claude_code_gets_the_task_typed() {
        use TabAtHandover::*;
        assert_eq!(tab(Some(&[]), None, false), Shell);
        assert_eq!(tab(Some(&[7]), Some(true), false), IdleClaude);
        assert_eq!(tab(Some(&[7]), Some(true), true), Draft);
        assert_eq!(tab(Some(&[7]), Some(false), false), Busy);
        assert_eq!(tab(Some(&[7]), None, false), Other, "another agent, or any other program");
        assert_eq!(tab(None, None, false), Other, "nothing known about its processes");
    }

    #[test]
    fn a_tab_a_claude_update_restarts_is_busy_whatever_runs_in_it() {
        for running in [None, Some(&[][..]), Some(&[7][..])] {
            let probe = TabProbe {
                restarting: true,
                launched_lately: false,
                shell_gone: false,
                running,
                claude_idle: running.map(|_| true),
                typed: false,
            };
            assert_eq!(tab_at_handover(&probe), TabAtHandover::Busy);
        }
    }

    #[test]
    fn a_write_access_answer_no_read_could_recheck_stops_counting() {
        let at = Instant::now();
        let answer = (true, at);
        assert_eq!(usable_access(Some(&answer), at + ACCESS_RECHECK_AFTER), Some(true));
        assert_eq!(usable_access(Some(&answer), at + ACCESS_EXPIRES_AFTER), None);
        assert_eq!(usable_access(None, at), None);
    }

    #[test]
    fn a_tab_that_launched_lately_is_busy_until_its_program_shows() {
        use TabAtHandover::*;
        let lately = |running: Option<&[u32]>, claude_idle: Option<bool>| {
            tab_at_handover(&TabProbe {
                restarting: false,
                launched_lately: true,
                shell_gone: false,
                running,
                claude_idle,
                typed: false,
            })
        };
        assert_eq!(lately(Some(&[]), None), Busy, "its shell hasn't started the agent yet");
        assert_eq!(lately(Some(&[7]), None), Busy, "the agent hasn't written its session yet");
        assert_eq!(lately(None, None), Busy);
        assert_eq!(lately(Some(&[7]), Some(true)), IdleClaude);
    }

    #[test]
    fn a_shell_that_exited_runs_only_its_orphans_and_an_unread_table_tells_nothing() {
        let table = [(1, 0), (10, 1), (11, 10), (12, 11)];
        assert_eq!(running_under(Some(10), &table), Some(vec![11, 12]));
        assert_eq!(running_under(Some(12), &table), Some(vec![]));
        assert_eq!(running_under(Some(40), &table), Some(vec![]), "its shell exited");
        let orphaned = [(1, 0), (41, 40), (42, 41)];
        assert_eq!(running_under(Some(40), &orphaned), Some(vec![41, 42]), "Windows keeps ppid");
        assert_eq!(running_under(None, &table), None);
        assert_eq!(running_under(Some(10), &[]), None);
        assert!(shell_gone(Some(40), &orphaned));
        assert!(!shell_gone(Some(10), &table));
        assert!(!shell_gone(None, &table));
        assert!(!shell_gone(Some(10), &[]), "an unread table tells nothing");
    }

    #[test]
    fn a_tab_whose_shell_is_gone_takes_no_typing() {
        use TabAtHandover::*;
        let gone = |running: Option<&[u32]>, claude_idle: Option<bool>| {
            tab_at_handover(&TabProbe {
                restarting: false,
                launched_lately: false,
                shell_gone: true,
                running,
                claude_idle,
                typed: false,
            })
        };
        assert_eq!(gone(Some(&[41]), Some(true)), Other, "its terminal no longer takes input");
        assert_eq!(gone(Some(&[41]), Some(false)), Other);
        assert_eq!(gone(Some(&[]), None), Shell);
    }

    #[test]
    fn findings_wait_for_every_session_on_the_worktree_and_only_claude_code_is_reminded() {
        use TabAtHandover::*;
        assert_eq!(findings_gate(&[]), FindingsGate::Settled(None));
        assert_eq!(
            findings_gate(&[(Shell, false), (IdleClaude, false)]),
            FindingsGate::Settled(Some(1))
        );
        assert_eq!(
            findings_gate(&[(IdleClaude, true), (Busy, false)]),
            FindingsGate::Wait,
            "another session there still works"
        );
        assert_eq!(findings_gate(&[(Draft, true)]), FindingsGate::Wait, "the user is typing");
        assert_eq!(findings_gate(&[(Other, false)]), FindingsGate::Wait);
        assert_eq!(
            findings_gate(&[(Other, true)]),
            FindingsGate::Settled(None),
            "another agent whose turn ended is done, but nothing is typed into it"
        );
    }

    #[test]
    fn the_task_waits_while_a_session_there_is_busy_or_unknown() {
        use TabAtHandover::*;
        assert_eq!(handover_to(&[]), Handover::NewTab);
        assert_eq!(handover_to(&[Shell, Shell]), Handover::NewTab);
        assert_eq!(handover_to(&[Busy, IdleClaude]), Handover::Type(1));
        assert_eq!(handover_to(&[Shell, Busy]), Handover::Wait(Hold::SessionBusy));
        assert_eq!(handover_to(&[Other, Shell]), Handover::Wait(Hold::OtherProgram));
        assert_eq!(handover_to(&[Other, Busy]), Handover::Wait(Hold::SessionBusy));
        assert_eq!(handover_to(&[Other, Draft]), Handover::Wait(Hold::UnsentInput));
        assert_eq!(handover_to(&[Draft, Busy]), Handover::Wait(Hold::SessionBusy));
    }
}
