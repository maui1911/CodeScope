//! Dispatch (ADR-0027): after each board read, start a round for every item the user moved
//! into a trigger column, in a worktree of its own with an agent session on it. When a research
//! round's session is idle with its findings written, post them on the issue, then read the
//! answers to its decision points and hand them to the implementation round. Watch that round's
//! pull request, report it on the issue and move the item to In review when it is ready.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use codescope_core::agents::claude::live_session;
use codescope_core::board::dispatch::{
    FINDINGS_FILE, Facts, Hold, PostedFindings, ProjectRemotes, Round, StatusChange,
    WorktreeDispatch, add_dispatch_worktree, branch_name, clear_findings, counts_towards_limit,
    decide, exclude_handover_files, find_skill, launch_with_task, mover_to_check,
    moves_to_needs_decision, projects_for_repository, task_file, task_message,
    trigger_content_ids, write_task_file,
};
use codescope_core::board::answers::{
    Answer, Comment, after_findings, answer_body, answers_section, choices_text, chosen,
    find_answers,
};
use codescope_core::board::findings::{
    DecisionPoint, FileStamp, Outcome, Problem, comment_body, posted_findings, problems_list,
    problems_message, read_findings,
};
use codescope_core::board::github::{self, Viewer};
use codescope_core::board::pull_request::{
    Checks, PostedPullRequest, PullRequest, PullRequestState, moves_to_in_review,
    pull_request_comment, round_pull_request,
};
use codescope_core::board::{
    Board, BoardItem, BoardRef, ColumnMapping, ColumnRole, effective_mapping, mapped_column,
};
use codescope_core::path_canon::paths_match;
use codescope_core::process_tree;
use codescope_core::{SessionState, TelemetrySnapshot};
use codescope_core::projects::{ProjectKind, ProjectsConfig};
use codescope_core::worktree_create::{PendingId, record_dispatch, record_posted_findings};
use codescope_terminal::TerminalView;
use gpui::{App, AppContext as _, Context, Entity, SharedString, Window};

use super::{AppShell, ToastKind};
use crate::notifications::NotificationKind;

const REMOTES_MAX_AGE: Duration = Duration::from_secs(300);
const VIEWER_MAX_AGE: Duration = Duration::from_secs(300);
const ACCESS_RECHECK_AFTER: Duration = Duration::from_secs(300);
const ACCESS_EXPIRES_AFTER: Duration = Duration::from_secs(600);
/// Text and Enter in one write can reach an agent's prompt as a paste, whose Enter is a newline.
const SUBMIT_AFTER_TYPING: Duration = Duration::from_millis(200);
const LAUNCH_GRACE: Duration = Duration::from_secs(30);
const MAX_REMINDERS: u32 = 3;
const POST_RETRY: Duration = Duration::from_secs(120);
const PULL_REQUEST_READ_EVERY: Duration = Duration::from_secs(30);

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
    /// Rounds whose findings wait for the user to free a tab on their worktree, and what holds it.
    findings_held: HashMap<RoundKey, HeldBy>,
    findings_announced: HashSet<RoundKey>,
    /// Content id → the decision points posted on it and the answers so far.
    discussions: HashMap<String, Discussion>,
    /// Items the Decide dialog moved to Ready, until their implementation round starts.
    readied: HashSet<String>,
    /// Item id → the pull request and session of its implementation round.
    watches: HashMap<String, PullRequestWatch>,
}

/// What the board poll knows about an implementation round's pull request and session.
#[derive(Default)]
struct PullRequestWatch {
    reading: bool,
    /// When the last read started, whether it worked or not.
    read_at: Option<Instant>,
    /// When the last read that worked started.
    read_ok_at: Option<Instant>,
    /// What the last read that worked found.
    pull_request: Option<PullRequest>,
    /// Whether the session worked since CodeScope started watching it.
    busy_seen: bool,
    /// How many prompts the session had at the last look.
    turns: Option<u32>,
    /// When the session last stopped; `None` while it works or has no tab.
    idle_since: Option<Instant>,
    /// The stop the bell rang for.
    rang_for: Option<Instant>,
    reporting: bool,
    /// Reported on the issue but not saved to `projects.json`.
    reported: Option<PostedPullRequest>,
    report_error: Option<(Instant, String)>,
    moving: bool,
    move_error: Option<(Instant, String)>,
    marking_ready: bool,
}

impl PullRequestWatch {
    fn track(&mut self, session: Option<(SessionState, u32)>) {
        let Some((state, turns)) = session else {
            self.idle_since = None;
            return;
        };
        let new_turn = self.turns.is_some_and(|seen| turns > seen);
        self.turns = Some(turns);
        match state {
            SessionState::Busy | SessionState::PendingToolUse => {
                self.busy_seen = true;
                self.idle_since = None;
            }
            SessionState::Idle if new_turn => {
                self.busy_seen = true;
                self.idle_since = Some(Instant::now());
            }
            SessionState::Idle => {
                self.idle_since.get_or_insert_with(Instant::now);
            }
            SessionState::Unknown => self.idle_since = None,
        }
    }

    /// Every 30 s while the item is in In progress or In review or its pull request is open, and
    /// right after the session stops.
    fn read_due(&self, role: Option<ColumnRole>) -> bool {
        let open = self
            .pull_request
            .as_ref()
            .is_some_and(|pr| pr.state == PullRequestState::Open);
        let watched = open || matches!(role, Some(ColumnRole::InProgress | ColumnRole::InReview));
        if self.reading || !watched {
            return false;
        }
        let Some(read_at) = self.read_at else {
            return true;
        };
        read_at.elapsed() >= PULL_REQUEST_READ_EVERY
            || self.idle_since.is_some_and(|idle| read_at < idle)
    }

    /// The session worked and stopped, and a read since then found no pull request.
    fn waits_for_you(&self) -> bool {
        self.busy_seen
            && self.pull_request.is_none()
            && self
                .idle_since
                .zip(self.read_ok_at)
                .is_some_and(|(idle, read)| read > idle)
    }
}

fn retry_due(error: &Option<(Instant, String)>) -> bool {
    error.as_ref().is_none_or(|(at, _)| at.elapsed() >= POST_RETRY)
}

/// A round: the board item's id and when the move that started the round was made.
type RoundKey = (String, String);

fn round_key(record: &WorktreeDispatch) -> RoundKey {
    (record.item_id.clone(), record.trigger_at.clone())
}

/// Decision points posted on an item, and the answers on its issue so far.
#[derive(Clone, Default)]
pub(crate) struct Discussion {
    points: Vec<DecisionPoint>,
    answers: Vec<Answer>,
}

/// An item whose decision points wait for answers, as a board read looks them up.
pub(crate) struct DiscussionRead {
    content_id: String,
    repository: String,
    findings_url: String,
    /// When the research round started; older comments never answer it.
    since: String,
}

/// Where an implementation round finds the answers to the research before it.
struct AnswersFrom {
    findings_url: String,
    since: String,
}

/// What the Decide dialog asks about an item.
pub(crate) struct Decision {
    pub item_id: String,
    pub board_url: String,
    pub content_id: String,
    pub number: u32,
    pub repository: String,
    pub title: String,
    pub findings_url: String,
    pub points: Vec<DecisionPoint>,
    /// Per point, the option the latest answer chose, else the advice.
    pub choices: Vec<char>,
}

/// An answer the Decide dialog sends.
pub(crate) struct AnswerPost {
    pub item_id: String,
    pub board_url: String,
    pub content_id: String,
    pub number: u32,
    pub choices: Vec<(String, char)>,
    pub note: String,
    pub move_to_ready: bool,
}

struct Reminder {
    problems: String,
    /// The version of the file the last reminder was about.
    file: Option<FileStamp>,
    sent: u32,
}

enum SessionAt {
    Working,
    Held(HeldBy),
    Done(Remind),
}

enum Remind {
    Typing(Entity<TerminalView>),
    Notice,
    Nothing,
}

struct Posting {
    record: WorktreeDispatch,
    path: PathBuf,
    outcome: Outcome,
    points: Vec<DecisionPoint>,
}

struct Posted {
    url: String,
    move_error: Option<String>,
}

/// What a board read needs from the app, taken on the foreground.
pub(crate) struct ReadInput {
    boards: Vec<(String, ColumnMapping)>,
    viewer: Option<Viewer>,
    known_access: HashMap<(String, String), bool>,
    /// Local projects as `(id, path)`, when their remotes are due for a read.
    projects: Option<Vec<(String, String)>>,
    discussions: Vec<DiscussionRead>,
}

#[derive(Default)]
pub(crate) struct ReadOutput {
    pub boards: Vec<BoardFetch>,
    viewer: Option<Result<Viewer, String>>,
    /// Write access by `(repository, login)`; `None` where the check failed.
    access: Vec<((String, String), Option<bool>)>,
    remotes: Option<Vec<ProjectRemotes>>,
    /// Discussions by content id; `None` when they weren't read.
    discussions: Option<HashMap<String, Discussion>>,
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
    pub action: Option<CardAction>,
}

impl CardNote {
    fn new(text: impl Into<SharedString>, tone: NoteTone) -> Self {
        Self {
            text: text.into(),
            tone,
            action: None,
        }
    }
}

/// A button on a card.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum CardAction {
    Decide,
    ReadyForReview,
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
    answers_from: Option<AnswersFrom>,
    known_access: HashMap<(String, String), bool>,
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
    Stopped,
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
    stopped: bool,
    typed: bool,
}

fn tab_at_handover(tab: &TabProbe) -> TabAtHandover {
    if tab.restarting {
        return TabAtHandover::Busy;
    }
    let state = match (tab.running, tab.claude_idle) {
        (Some([]), _) => TabAtHandover::Shell,
        (Some(_), _) if tab.shell_gone => TabAtHandover::Other,
        (Some(_), _) if tab.stopped => TabAtHandover::Stopped,
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
    } else if tabs.contains(&TabAtHandover::Stopped) {
        Handover::Wait(Hold::SessionStopped)
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
    Settled(RemindBy),
    Wait,
    Held(HeldBy),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HeldBy {
    Program,
    QuietAgent,
    StoppedSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RemindBy {
    Typing(usize),
    Notice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Turn {
    Ended,
    Running,
    Quiet,
    Unknown,
}

fn turn_of(state: codescope_core::SessionState, quiet_timeout: bool) -> Turn {
    use codescope_core::SessionState;
    match state {
        _ if quiet_timeout => Turn::Quiet,
        SessionState::Idle => Turn::Ended,
        SessionState::Busy | SessionState::PendingToolUse => Turn::Running,
        SessionState::Unknown => Turn::Unknown,
    }
}

fn findings_gate(tabs: &[(TabAtHandover, Turn)]) -> FindingsGate {
    let other = |turn: Turn| tabs.contains(&(TabAtHandover::Other, turn));
    if tabs.iter().any(|(state, _)| *state == TabAtHandover::Busy) || other(Turn::Running) {
        return FindingsGate::Wait;
    }
    if other(Turn::Unknown) {
        return FindingsGate::Held(HeldBy::Program);
    }
    if other(Turn::Quiet) {
        return FindingsGate::Held(HeldBy::QuietAgent);
    }
    if tabs.iter().any(|(state, _)| *state == TabAtHandover::Stopped) {
        return FindingsGate::Held(HeldBy::StoppedSession);
    }
    let idle_claude = tabs.iter().position(|(state, _)| *state == TabAtHandover::IdleClaude);
    FindingsGate::Settled(idle_claude.map_or(RemindBy::Notice, RemindBy::Typing))
}

struct Place {
    create: bool,
    path: PathBuf,
    branch: String,
}

fn fresh_access(
    access: &HashMap<(String, String), (bool, Instant)>,
) -> HashMap<(String, String), bool> {
    access
        .iter()
        .filter(|(_, (_, at))| at.elapsed() < ACCESS_RECHECK_AFTER)
        .map(|(key, (can_write, _))| (key.clone(), *can_write))
        .collect()
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
    if !input.discussions.is_empty() {
        if viewer.is_none() {
            output.viewer = Some(github::fetch_viewer().map_err(|err| format!("{err:#}")));
        }
        output.discussions =
            read_discussions(&input.discussions, &input.known_access, &mut output.access);
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
    known: &HashMap<(String, String), bool>,
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
        check_write_access(repository, login, known, found);
    }
}

fn check_write_access(
    repository: &str,
    login: &str,
    known: &HashMap<(String, String), bool>,
    found: &mut Vec<((String, String), Option<bool>)>,
) {
    let key = access_key(repository, login);
    if known.contains_key(&key) || found.iter().any(|(k, _)| *k == key) {
        return;
    }
    let answer = github::can_write(repository, login)
        .inspect_err(|err| {
            eprintln!("warning: couldn't check {login}'s access to {repository}: {err:#}")
        })
        .ok();
    found.push((key, answer));
}

fn read_discussions(
    reads: &[DiscussionRead],
    known: &HashMap<(String, String), bool>,
    found: &mut Vec<((String, String), Option<bool>)>,
) -> Option<HashMap<String, Discussion>> {
    let ids: Vec<String> = reads.iter().map(|r| r.content_id.clone()).collect();
    let comments = github::fetch_comments(&ids)
        .inspect_err(|err| eprintln!("warning: couldn't read the answers: {err:#}"))
        .ok()?;
    let mut discussions = HashMap::new();
    for read in reads {
        let Some(list) = comments.get(&read.content_id) else {
            continue;
        };
        let found_here = discussion(
            list,
            &read.repository,
            &read.findings_url,
            &read.since,
            known,
            found,
        );
        discussions.insert(read.content_id.clone(), found_here);
    }
    Some(discussions)
}

/// The decision points of the findings comment among `comments`, and the answers after it.
/// Write access that `known` doesn't cover is checked and added to `found`.
fn discussion(
    comments: &[Comment],
    repository: &str,
    findings_url: &str,
    since: &str,
    known: &HashMap<(String, String), bool>,
    found: &mut Vec<((String, String), Option<bool>)>,
) -> Discussion {
    let points = comments
        .iter()
        .find(|c| c.url == findings_url)
        .and_then(|c| posted_findings(&c.body))
        .map(|findings| findings.decision_points)
        .unwrap_or_default();
    for comment in after_findings(comments, findings_url, since) {
        if let Some(login) = comment.author.as_deref() {
            check_write_access(repository, login, known, found);
        }
    }
    let found: &[((String, String), Option<bool>)] = found;
    let can_write = |login: &str| {
        let key = access_key(repository, login);
        known
            .get(&key)
            .copied()
            .or_else(|| found.iter().find(|(k, _)| *k == key).and_then(|(_, can)| *can))
            .unwrap_or(false)
    };
    let answers = find_answers(comments, findings_url, since, &points, can_write);
    Discussion { points, answers }
}

/// The `## Answers` section of an implementation round's task file, read fresh from the issue.
fn answers_for_task(job: &Job, from: &AnswersFrom) -> String {
    let record = &job.record;
    let comments = github::fetch_comments(std::slice::from_ref(&record.content_id))
        .inspect_err(|err| {
            eprintln!("warning: couldn't read #{}'s answers: {err:#}", record.number)
        })
        .ok()
        .and_then(|mut found| found.remove(&record.content_id));
    let mut checked = Vec::new();
    let discussion = comments.map(|comments| {
        discussion(
            &comments,
            &record.repository,
            &from.findings_url,
            &from.since,
            &job.known_access,
            &mut checked,
        )
    });
    answers_section(discussion.as_ref().map(|d| d.answers.as_slice()), &from.findings_url)
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
    let answers = job.answers_from.as_ref().map(|from| answers_for_task(job, from));
    write_task_file(&job.path, &task_file(&job.record, skill.as_deref(), answers.as_deref()))
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
        let discussions = self
            .dispatch_records(cx)
            .into_iter()
            .filter_map(|(_, _, _, record)| {
                let posted = record.findings.clone()?;
                let waiting = record.round == Round::Research
                    && posted.outcome == Outcome::DecisionPoints;
                waiting.then_some(DiscussionRead {
                    content_id: record.content_id,
                    repository: record.repository,
                    findings_url: posted.comment_url,
                    since: record.trigger_at,
                })
            })
            .collect();
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
            known_access: fresh_access(&state.access),
            projects: remotes_due.then_some(local),
            discussions,
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
        if let Some(discussions) = output.discussions.take() {
            state.discussions.extend(discussions);
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
        let previous = self
            .dispatch_records(cx)
            .into_iter()
            .map(|(_, _, _, record)| record)
            .find(|r| r.board_url == board_url && r.item_id == item.id);
        let answers_from = previous.as_ref().and_then(|previous| {
            let posted = previous.findings.clone()?;
            let after_research = previous.round == Round::Research && round != Round::Research;
            after_research.then(|| AnswersFrom {
                findings_url: posted.comment_url,
                since: previous.trigger_at.clone(),
            })
        });
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
                pull_request: None,
            },
            assign_to: item.assignees.is_empty().then_some(content_id),
            board,
            progress_column_id,
            answers_from,
            known_access: fresh_access(&self.board.dispatch.access),
        };
        self.board.dispatch.readied.remove(&item.id);
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
        let saved =
            self.save_record(&launch.project_id, &job.path, &job.branch, record.clone(), cx);
        if let Err(message) = saved {
            self.fail_dispatch(&record.item_id, record.number, &record.trigger_at, message, cx);
            return None;
        }
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
                let claude = running.as_deref().and_then(|running| {
                    live_session::process_for_pids(sessions.as_deref()?, running, &wanted)
                });
                let relaunched = self.claude_updates.relaunched_at(tab.id);
                let launched_lately = tab.spawned_instant.elapsed() < LAUNCH_GRACE
                    || relaunched.is_some_and(|at| at.elapsed() < LAUNCH_GRACE);
                tab_at_handover(&TabProbe {
                    restarting: self.claude_updates.is_restarting(tab.id),
                    launched_lately,
                    shell_gone: shell_gone(shell, &table),
                    running: running.as_deref(),
                    claude_idle: claude.as_ref().and_then(|claude| claude.reports_idle()),
                    stopped: claude.is_some_and(|claude| process_tree::is_stopped(claude.pid)),
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
        state.findings_announced.retain(|key| awaiting.contains(key));
        let held_before = std::mem::take(&mut state.findings_held);

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
            let remind = match self.session_at(&path, cx) {
                SessionAt::Working => continue,
                SessionAt::Held(by) => {
                    self.board.dispatch.findings_held.insert(key, by);
                    continue;
                }
                SessionAt::Done(_) if !researching => Remind::Nothing,
                SessionAt::Done(remind) => remind,
            };
            match file.check() {
                Ok((text, findings)) => {
                    let body = comment_body(text, &findings);
                    let posting = Posting {
                        points: findings.decision_points.clone(),
                        record,
                        path,
                        outcome: findings.outcome(),
                    };
                    self.post_findings(posting, body, board, mapping, cx);
                }
                Err(problems) => {
                    self.remind(&record, &path, file.stamp, remind, &problems, cx);
                }
            }
        }
        if self.board.dispatch.findings_held != held_before {
            cx.notify();
        }
    }

    fn session_at(&self, path: &Path, cx: &App) -> SessionAt {
        let tabs = self.tabs_on(path);
        let states = self.tab_states(&tabs, path, cx);
        let probed: Vec<(TabAtHandover, Turn)> = tabs
            .iter()
            .zip(states)
            .map(|(tab, state)| (state, self.turn(tab)))
            .collect();
        match findings_gate(&probed) {
            FindingsGate::Wait => SessionAt::Working,
            FindingsGate::Held(by) => SessionAt::Held(by),
            FindingsGate::Settled(RemindBy::Typing(index)) => {
                SessionAt::Done(Remind::Typing(tabs[index].terminal.clone()))
            }
            FindingsGate::Settled(RemindBy::Notice) => SessionAt::Done(Remind::Notice),
        }
    }

    fn turn(&self, tab: &super::Tab) -> Turn {
        let snapshot = tab.adopted_session_id.as_deref().and_then(|sid| self.telemetry_for(sid));
        snapshot.map_or(Turn::Unknown, |snap| turn_of(snap.state, snap.quiet_timeout))
    }

    fn telemetry_at(&self, path: &Path) -> Option<TelemetrySnapshot> {
        let mut sessions = self.tabs_on(path).into_iter().filter_map(|tab| {
            tab.adopted_session_id.as_deref().and_then(|sid| self.telemetry_for(sid))
        });
        sessions.next()
    }

    fn tab_title_at(&self, path: &Path) -> Option<SharedString> {
        self.tabs_on(path).first().map(|tab| tab.title.clone())
    }

    fn remind(
        &mut self,
        record: &WorktreeDispatch,
        path: &Path,
        file: FileStamp,
        remind: Remind,
        problems: &[Problem],
        cx: &mut Context<Self>,
    ) {
        let reminders = &mut self.board.dispatch.reminders;
        let reminder = reminders.entry(round_key(record)).or_insert(Reminder {
            problems: String::new(),
            file: None,
            sent: 0,
        });
        reminder.problems = problems_list(problems);
        cx.notify();
        let due = reminder.file != Some(file) && reminder.sent < MAX_REMINDERS;
        if !due || matches!(remind, Remind::Nothing) {
            return;
        }
        reminder.file = Some(file);
        reminder.sent += 1;
        let listed = reminder.problems.clone();
        match remind {
            Remind::Typing(terminal) => {
                Self::submit_line(&terminal, problems_message(problems), cx);
            }
            Remind::Notice => {
                let title = format!("Findings on #{} can't be posted yet", record.number);
                let detail = format!("{listed}. Fix {FINDINGS_FILE}, or have its session fix it.");
                self.push_toast(ToastKind::Err, title.clone(), Some(detail.clone().into()), cx);
                self.push_notification(
                    crate::notifications::NotificationKind::SessionWaiting,
                    title,
                    detail,
                    self.tab_title_at(path),
                    cx,
                );
            }
            Remind::Nothing => {}
        }
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
                    let move_error = if !moves_to_needs_decision(role) {
                        None
                    } else {
                        match mapped_column(&board, &mapping, ColumnRole::NeedsDecision) {
                            Some(column) => github::set_status(&board, &item_id, &column.id)
                                .map_err(failed)
                                .err(),
                            None => Some("the board has no Needs decision column".to_string()),
                        }
                    };
                    Ok::<_, String>(Posted { url, move_error })
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_posting(posting, result, cx));
        })
        .detach();
    }

    fn finish_posting(
        &mut self,
        posting: Posting,
        result: Result<Posted, String>,
        cx: &mut Context<Self>,
    ) {
        let Posting {
            record,
            path,
            outcome,
            points,
        } = posting;
        let key = round_key(&record);
        self.board.dispatch.posting.remove(&key);
        cx.notify();
        let Posted { url, move_error } = match result {
            Ok(posted) => posted,
            Err(message) => {
                let note = format!("Couldn't post the findings: {message}");
                let errors = &mut self.board.dispatch.post_errors;
                let first = errors.insert(key, (Instant::now(), note)).is_none();
                if first {
                    self.push_toast(
                        ToastKind::Err,
                        format!("Couldn't post the findings of #{}", record.number),
                        Some(message.into()),
                        cx,
                    );
                }
                return;
            }
        };

        let first_news = self.board.dispatch.findings_announced.insert(key.clone());
        let posted = PostedFindings {
            outcome,
            comment_url: url,
        };
        if outcome == Outcome::DecisionPoints {
            let discussion = Discussion {
                points,
                answers: Vec::new(),
            };
            self.board.dispatch.discussions.insert(record.content_id.clone(), discussion);
        }
        let saved = ProjectsConfig::load(&self.paths)
            .map_err(|err| format!("{err:#}"))
            .and_then(|mut next| {
                if record_posted_findings(&mut next, &record.item_id, &record.trigger_at, posted) {
                    next.save(&self.paths).map_err(|err| format!("{err:#}"))?;
                }
                Ok(next)
            });
        match saved {
            Ok(next) => {
                self.projects = next.clone();
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.replace_projects(next);
                    cx.notify();
                });
            }
            Err(err) => {
                let note = format!("Findings posted, not saved: {err}");
                self.board.dispatch.post_errors.insert(key, (Instant::now(), note));
            }
        }
        if !first_news && move_error.is_none() {
            return;
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
        let tab_title = self.tab_title_at(&path);
        self.push_notification(NotificationKind::SessionReady, title, detail, tab_title, cx);
        cx.notify();
    }

    /// Keep `record` on its worktree row in `projects.json`, read fresh so changes made since
    /// stay.
    fn save_record(
        &mut self,
        project_id: &str,
        path: &Path,
        branch: &str,
        record: WorktreeDispatch,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let path = path.to_string_lossy();
        let mut next = ProjectsConfig::load(&self.paths).map_err(|err| format!("{err:#}"))?;
        if !record_dispatch(&mut next, project_id, &path, branch, record) {
            return Err("the project no longer tracks the worktree".to_string());
        }
        next.save(&self.paths).map_err(|err| format!("{err:#}"))?;
        self.projects = next.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.replace_projects(next);
            cx.notify();
        });
        Ok(())
    }

    fn dispatch_record(
        &self,
        item_id: &str,
        cx: &App,
    ) -> Option<(String, PathBuf, Option<String>, WorktreeDispatch)> {
        self.dispatch_records(cx)
            .into_iter()
            .find(|(_, _, _, r)| r.item_id == item_id)
    }

    fn board_and_mapping(&self, board_url: &str) -> Option<(&Board, ColumnMapping)> {
        let board = self.board.reads_by_url.get(board_url)?.last_good.as_ref()?;
        let settings = self.settings.boards.iter().find(|b| b.url == board_url)?;
        Some((board, effective_mapping(board, &settings.columns)))
    }

    /// The role of the column `item_id` is in, on the last good read of `board_url`.
    fn item_role(&self, board_url: &str, item_id: &str) -> Option<ColumnRole> {
        let (board, mapping) = self.board_and_mapping(board_url)?;
        let item = board.items.iter().find(|item| item.id == item_id)?;
        board.role_of(item, &mapping)
    }

    /// The board at `board_url` and the id of its column with `role`, to move an item into.
    fn column_target(&self, board_url: &str, role: ColumnRole) -> Option<(Board, String)> {
        let (board, mapping) = self.board_and_mapping(board_url)?;
        let column = mapped_column(board, &mapping, role)?;
        Some((board.clone(), column.id.clone()))
    }

    /// Watch the pull request of every implementation round: read it, report it on the issue
    /// once, move the item to In review once it is ready, and ring the bell when the session
    /// stopped without one.
    pub(crate) fn watch_pull_requests(&mut self, cx: &mut Context<Self>) {
        let records: Vec<_> = self
            .dispatch_records(cx)
            .into_iter()
            .filter(|(_, _, _, r)| r.round == Round::Implementation)
            .collect();
        self.board
            .dispatch
            .watches
            .retain(|id, _| records.iter().any(|(_, _, _, r)| r.item_id == *id));
        for (_, path, branch, record) in records {
            let Some(branch) = branch else {
                continue;
            };
            let role = self.item_role(&record.board_url, &record.item_id);
            let session = self
                .telemetry_at(&path)
                .map(|snapshot| (super::notified_state(&snapshot), snapshot.turn_count));
            let watch = self
                .board
                .dispatch
                .watches
                .entry(record.item_id.clone())
                .or_default();
            watch.track(session);
            let read = watch.read_due(role);
            watch.reading |= read;
            let posted = record.pull_request.as_ref().or(watch.reported.as_ref());
            let found = watch
                .pull_request
                .clone()
                .filter(|pr| pr.state == PullRequestState::Open);
            let report = found
                .clone()
                .filter(|_| posted.is_none() && !watch.reporting && retry_due(&watch.report_error));
            let move_it = posted.is_some_and(|p| !p.in_review)
                && found.is_some_and(|pr| moves_to_in_review(&pr, role))
                && !watch.moving
                && retry_due(&watch.move_error);
            watch.moving |= move_it;
            let ring = watch.waits_for_you() && watch.rang_for != watch.idle_since;
            if ring {
                watch.rang_for = watch.idle_since;
            }
            if read {
                self.read_pull_request(&record, branch, cx);
            }
            if let Some(pr) = report {
                self.report_pull_request(&record, pr, cx);
            }
            if move_it {
                self.move_to_in_review(&record, cx);
            }
            if ring {
                let tab_title = self.tab_title_at(&path);
                self.push_notification(
                    NotificationKind::SessionWaiting,
                    format!("#{} waits for you", record.number),
                    "Implementation stopped without a pull request.",
                    tab_title,
                    cx,
                );
            }
        }
    }

    fn read_pull_request(
        &mut self,
        record: &WorktreeDispatch,
        branch: String,
        cx: &mut Context<Self>,
    ) {
        let item_id = record.item_id.clone();
        let repository = record.repository.clone();
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let (branch, result) = cx
                .background_spawn(async move {
                    let found = github::fetch_pull_requests(&repository, &branch);
                    (branch, found.map(round_pull_request))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(watch) = this.board.dispatch.watches.get_mut(&item_id) else {
                    return;
                };
                watch.reading = false;
                watch.read_at = Some(started);
                match result {
                    Ok(found) => {
                        watch.pull_request = found;
                        watch.read_ok_at = Some(started);
                    }
                    Err(err) => {
                        eprintln!("warning: couldn't read the pull request on {branch}: {err:#}")
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn report_pull_request(
        &mut self,
        record: &WorktreeDispatch,
        pr: PullRequest,
        cx: &mut Context<Self>,
    ) {
        let item_id = record.item_id.clone();
        let content_id = record.content_id.clone();
        let body = pull_request_comment(&pr);
        if let Some(watch) = self.board.dispatch.watches.get_mut(&item_id) {
            watch.reporting = true;
            watch.report_error = None;
        }
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    github::add_comment(&content_id, &body).map_err(|err| format!("{err:#}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_report(&item_id, pr, result, cx));
        })
        .detach();
    }

    fn finish_report(
        &mut self,
        item_id: &str,
        pr: PullRequest,
        result: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(watch) = self.board.dispatch.watches.get_mut(item_id) else {
            return;
        };
        watch.reporting = false;
        let comment_url = match result {
            Ok(url) => url,
            Err(message) => {
                watch.report_error = Some((Instant::now(), message.clone()));
                let title = format!("Couldn't report PR #{} on the issue", pr.number);
                self.push_toast(ToastKind::Err, title, Some(message.into()), cx);
                return;
            }
        };
        let posted = PostedPullRequest {
            number: pr.number,
            url: pr.url.clone(),
            comment_url,
            in_review: false,
        };
        self.keep_posted(item_id, posted, cx);
        let Some((_, path, _, record)) = self.dispatch_record(item_id, cx) else {
            return;
        };
        let (kind, toast, title, detail) = if pr.is_draft {
            (
                NotificationKind::SessionWaiting,
                ToastKind::Info,
                format!("Draft PR #{} for #{}", pr.number, record.number),
                "Test it by hand, then mark it ready for review.".to_string(),
            )
        } else {
            (
                NotificationKind::SessionReady,
                ToastKind::Ok,
                format!("PR #{} for #{}", pr.number, record.number),
                record.title.clone(),
            )
        };
        self.push_toast(toast, title.clone(), Some(detail.clone().into()), cx);
        let tab_title = self.tab_title_at(&path);
        self.push_notification(kind, title, detail, tab_title, cx);
        cx.notify();
    }

    /// Save the reported pull request on its record; it stays in memory when that fails.
    fn keep_posted(&mut self, item_id: &str, posted: PostedPullRequest, cx: &mut Context<Self>) {
        let saved = match self.dispatch_record(item_id, cx) {
            Some((project_id, path, branch, mut record)) => {
                record.pull_request = Some(posted.clone());
                let branch = branch.unwrap_or_default();
                self.save_record(&project_id, &path, &branch, record, cx)
            }
            None => Err("the worktree is gone".to_string()),
        };
        if let Err(err) = &saved {
            eprintln!("warning: couldn't record the pull request: {err}");
        }
        if let Some(watch) = self.board.dispatch.watches.get_mut(item_id) {
            watch.reported = saved.is_err().then_some(posted);
        }
    }

    fn move_to_in_review(&mut self, record: &WorktreeDispatch, cx: &mut Context<Self>) {
        let item_id = record.item_id.clone();
        let Some((board, column)) = self.column_target(&record.board_url, ColumnRole::InReview)
        else {
            if let Some(watch) = self.board.dispatch.watches.get_mut(&item_id) {
                watch.moving = false;
            }
            return;
        };
        let number = record.number;
        cx.spawn(async move |this, cx| {
            let moved_id = item_id.clone();
            let result = cx
                .background_spawn(async move {
                    github::set_status(&board, &moved_id, &column).map_err(|err| format!("{err:#}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_move(&item_id, number, result, cx));
        })
        .detach();
    }

    fn finish_move(
        &mut self,
        item_id: &str,
        number: u32,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let Some(watch) = self.board.dispatch.watches.get_mut(item_id) else {
            return;
        };
        watch.moving = false;
        if let Err(message) = result {
            watch.move_error = Some((Instant::now(), message.clone()));
            let title = format!("Couldn't move #{number} to In review");
            self.push_toast(ToastKind::Err, title, Some(message.into()), cx);
            return;
        }
        watch.move_error = None;
        let reported = watch.reported.clone();
        let posted = self
            .dispatch_record(item_id, cx)
            .and_then(|(_, _, _, record)| record.pull_request)
            .or(reported);
        if let Some(posted) = posted {
            self.keep_posted(item_id, PostedPullRequest { in_review: true, ..posted }, cx);
        }
        self.refresh_boards(cx);
        cx.notify();
    }

    /// Mark the draft pull request of `item_id` ready for review; the next watch moves the item
    /// to In review.
    pub(crate) fn ready_for_review(&mut self, item_id: &str, cx: &mut Context<Self>) {
        let Some(watch) = self.board.dispatch.watches.get_mut(item_id) else {
            return;
        };
        let draft = watch
            .pull_request
            .clone()
            .filter(|pr| pr.is_draft && pr.state == PullRequestState::Open);
        let Some(pr) = draft.filter(|_| !watch.marking_ready) else {
            return;
        };
        watch.marking_ready = true;
        cx.notify();
        let item_id = item_id.to_string();
        cx.spawn(async move |this, cx| {
            let id = pr.id.clone();
            let result = cx
                .background_spawn(async move {
                    github::mark_ready_for_review(&id).map_err(|err| format!("{err:#}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_ready_for_review(&item_id, pr, result, cx);
            });
        })
        .detach();
    }

    fn finish_ready_for_review(
        &mut self,
        item_id: &str,
        pr: PullRequest,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        let Some(watch) = self.board.dispatch.watches.get_mut(item_id) else {
            return;
        };
        watch.marking_ready = false;
        if let Err(message) = result {
            let title = format!("Couldn't mark PR #{} ready for review", pr.number);
            self.push_toast(ToastKind::Err, title, Some(message.into()), cx);
            return;
        }
        let ready = PullRequest {
            is_draft: false,
            ..pr
        };
        watch.pull_request = Some(ready.clone());
        let role = self
            .dispatch_record(item_id, cx)
            .and_then(|(_, _, _, r)| self.item_role(&r.board_url, item_id));
        let detail = moves_to_in_review(&ready, role)
            .then(|| SharedString::from("It moves to In review."));
        let title = format!("PR #{} is ready for review", ready.number);
        self.push_toast(ToastKind::Ok, title, detail, cx);
        self.watch_pull_requests(cx);
        cx.notify();
    }

    /// What each card on the board at `board_url` says about dispatch, by item id.
    pub(crate) fn card_notes(&self, board_url: &str, cx: &App) -> HashMap<String, CardNote> {
        let state = &self.board.dispatch;
        let viewer = state.viewer.as_ref().map(|v| v.login.as_str());
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
                    (Some(Outcome::DecisionPoints), _) => decision_note(
                        state.discussions.get(&r.content_id),
                        viewer,
                        state.readied.contains(&r.item_id),
                    ),
                    (Some(Outcome::Findings), _) => {
                        CardNote::new("Findings posted · move it to Ready", NoteTone::NeedsYou)
                    }
                    (None, Some(branch)) => CardNote::new(
                        format!("{} · {branch}", r.round.label()),
                        NoteTone::Working,
                    ),
                    (None, None) => CardNote::new(r.round.label(), NoteTone::Working),
                };
                let watched = state
                    .watches
                    .get(&r.item_id)
                    .filter(|_| r.round == Round::Implementation)
                    .and_then(|watch| {
                        let posted = r.pull_request.as_ref().or(watch.reported.as_ref());
                        pull_request_note(watch, posted)
                    });
                (r.item_id, watched.unwrap_or(note))
            })
            .collect();
        for key in state.posting.iter().filter(|key| rounds.contains(*key)) {
            notes.insert(key.0.clone(), CardNote::new("Posting the findings…", NoteTone::Working));
        }
        for (key, reminder) in state.reminders.iter().filter(|(key, _)| rounds.contains(*key)) {
            let text = format!("Findings incomplete: {}", reminder.problems);
            notes.insert(key.0.clone(), CardNote::new(text, NoteTone::Failed));
        }
        for (key, (_, note)) in state.post_errors.iter().filter(|(key, _)| rounds.contains(*key)) {
            notes.insert(key.0.clone(), CardNote::new(note.clone(), NoteTone::Failed));
        }
        for (key, by) in state.findings_held.iter().filter(|(key, _)| rounds.contains(*key)) {
            let text = match by {
                HeldBy::Program => {
                    "Findings written · to post them, quit what runs in its worktree's tab, or \
                     close the tab"
                }
                HeldBy::QuietAgent => {
                    "Findings written · its agent may not be done: to post them, quit what runs \
                     in its tab, or close the tab"
                }
                HeldBy::StoppedSession => {
                    "Findings written · to post them, type fg in its session's tab to resume \
                     it, or close the tab"
                }
            };
            notes.insert(key.0.clone(), CardNote::new(text, NoteTone::Waiting));
        }
        for (item_id, hold) in &state.holds {
            if !hold.is_quiet() {
                notes.insert(item_id.clone(), CardNote::new(hold.label(), NoteTone::Waiting));
            }
        }
        for (item_id, (_, message)) in &state.failed {
            let text = format!("Couldn't start: {message}");
            notes.insert(item_id.clone(), CardNote::new(text, NoteTone::Failed));
        }
        for (item_id, round) in &state.starting {
            let text = format!("Starting {}…", round.label().to_lowercase());
            notes.insert(item_id.clone(), CardNote::new(text, NoteTone::Working));
        }
        notes
    }

    /// What the Decide dialog asks about `item_id`, while its decision points are known.
    pub(crate) fn decision(&self, item_id: &str, cx: &App) -> Option<Decision> {
        let (_, _, _, record) = self
            .dispatch_records(cx)
            .into_iter()
            .find(|(_, _, _, r)| r.item_id == item_id && r.round == Round::Research)?;
        let posted = record.findings.clone()?;
        let discussion = self.board.dispatch.discussions.get(&record.content_id)?;
        if posted.outcome != Outcome::DecisionPoints || discussion.points.is_empty() {
            return None;
        }
        let answered = chosen(&discussion.answers);
        let choices = discussion
            .points
            .iter()
            .map(|point| {
                answered
                    .iter()
                    .find(|(id, _)| *id == point.id)
                    .map_or(point.advice, |(_, letter)| *letter)
            })
            .collect();
        Some(Decision {
            item_id: record.item_id,
            board_url: record.board_url,
            content_id: record.content_id,
            number: record.number,
            repository: record.repository,
            title: record.title,
            findings_url: posted.comment_url,
            points: discussion.points.clone(),
            choices,
        })
    }

    /// Post the answer on the issue, then move the item to Ready when asked and it is still in
    /// Needs decision, where CodeScope put it.
    pub(crate) fn send_answer(&mut self, post: AnswerPost, cx: &mut Context<Self>) {
        let ready = if post.move_to_ready {
            self.ready_target(&post.board_url, &post.item_id)
        } else {
            None
        };
        let stayed = post.move_to_ready && ready.is_none();
        let body = answer_body(&post.choices, &post.note);
        let content_id = post.content_id.clone();
        let item_id = post.item_id.clone();
        let posted_body = body.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let url = github::add_comment(&content_id, &posted_body)
                        .map_err(|err| format!("{err:#}"))?;
                    let moved = ready.and_then(|(board, column)| {
                        github::set_status(&board, &item_id, &column)
                            .map_err(|err| format!("{err:#}"))
                            .err()
                    });
                    Ok::<_, String>((url, moved))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_answer(post, body, stayed, result, cx);
            });
        })
        .detach();
    }

    /// The board and Ready column to move `item_id` to, while it is in Needs decision.
    fn ready_target(&self, board_url: &str, item_id: &str) -> Option<(Board, String)> {
        if self.item_role(board_url, item_id) != Some(ColumnRole::NeedsDecision) {
            return None;
        }
        self.column_target(board_url, ColumnRole::Ready)
    }

    fn finish_answer(
        &mut self,
        post: AnswerPost,
        body: String,
        stayed: bool,
        result: Result<(String, Option<String>), String>,
        cx: &mut Context<Self>,
    ) {
        let (url, move_error) = match result {
            Ok(done) => done,
            Err(message) => {
                if let Some(dialog) = self.decide_dialog.as_mut() {
                    dialog.sending = false;
                    dialog.error = Some(format!("Couldn't post the answer: {message}"));
                }
                cx.notify();
                return;
            }
        };
        self.decide_dialog = None;
        let state = &mut self.board.dispatch;
        if let (Some(viewer), Some(discussion)) =
            (&state.viewer, state.discussions.get_mut(&post.content_id))
        {
            discussion.answers.push(Answer {
                author: viewer.login.clone(),
                created_at: codescope_core::time::now_iso8601(),
                url,
                choices: post.choices.clone(),
                body,
            });
        }
        let moved = post.move_to_ready && !stayed && move_error.is_none();
        if moved {
            state.readied.insert(post.item_id.clone());
        }
        let title = format!("Answer posted on #{}", post.number);
        let (kind, detail) = match (&move_error, stayed, post.move_to_ready) {
            (Some(err), _, _) => (ToastKind::Err, format!("Couldn't move it to Ready: {err}")),
            (None, true, _) => (
                ToastKind::Info,
                "It isn't in Needs decision anymore, so it stays where it is.".to_string(),
            ),
            (None, false, true) => (
                ToastKind::Ok,
                "It moved to Ready; implementation starts at the next board read.".to_string(),
            ),
            (None, false, false) => (
                ToastKind::Ok,
                "Move it to Ready to start implementation.".to_string(),
            ),
        };
        self.push_toast(kind, title, Some(detail.into()), cx);
        if moved {
            self.refresh_boards(cx);
        }
        cx.notify();
    }
}

/// The card line of research that posted decision points: what to decide, or who answered.
fn decision_note(discussion: Option<&Discussion>, viewer: Option<&str>, readied: bool) -> CardNote {
    let Some(discussion) = discussion else {
        return CardNote::new("Decision points posted · answer on the issue", NoteTone::NeedsYou);
    };
    let Some(last) = discussion.answers.last() else {
        let count = discussion.points.len();
        if count == 0 {
            return CardNote::new(
                "Decision points posted · answer on the issue",
                NoteTone::NeedsYou,
            );
        }
        let noun = if count == 1 { "decision point" } else { "decision points" };
        return CardNote {
            text: format!("{count} {noun} · decide here or on the issue").into(),
            tone: NoteTone::NeedsYou,
            action: Some(CardAction::Decide),
        };
    };
    let who = if viewer.is_some_and(|v| v.eq_ignore_ascii_case(&last.author)) {
        "you".to_string()
    } else {
        format!("@{}", last.author)
    };
    let picked = chosen(&discussion.answers);
    let answered = if picked.is_empty() {
        format!("Answered by {who}")
    } else {
        format!("Answered by {who}: {}", choices_text(&picked))
    };
    if readied {
        CardNote::new(format!("{answered} · implementation starts"), NoteTone::Working)
    } else {
        CardNote::new(format!("{answered} · move it to Ready"), NoteTone::NeedsYou)
    }
}

/// The card line of an implementation round when its pull request or session has something to
/// say; `None` leaves the round's own line.
fn pull_request_note(
    watch: &PullRequestWatch,
    posted: Option<&PostedPullRequest>,
) -> Option<CardNote> {
    if watch.reporting {
        return Some(CardNote::new("Reporting the pull request…", NoteTone::Working));
    }
    if let Some((_, message)) = &watch.report_error {
        let text = format!("Couldn't report the pull request: {message}");
        return Some(CardNote::new(text, NoteTone::Failed));
    }
    if let Some((_, message)) = &watch.move_error {
        let text = format!("Couldn't move it to In review: {message}");
        return Some(CardNote::new(text, NoteTone::Failed));
    }
    let Some(pr) = &watch.pull_request else {
        if watch.waits_for_you() {
            return Some(CardNote::new("Waiting for you in the tab", NoteTone::NeedsYou));
        }
        let unread = watch.read_ok_at.is_none();
        return posted
            .filter(|_| unread)
            .map(|p| CardNote::new(format!("PR #{}", p.number), NoteTone::Waiting));
    };
    let number = pr.number;
    Some(match (pr.state, pr.is_draft) {
        (PullRequestState::Merged, _) => {
            CardNote::new(format!("PR #{number} merged"), NoteTone::Waiting)
        }
        (PullRequestState::Open, true) if watch.marking_ready => CardNote::new(
            format!("Marking PR #{number} ready for review…"),
            NoteTone::Working,
        ),
        (PullRequestState::Open, true) => CardNote {
            text: format!("Draft PR #{number} · test by hand").into(),
            tone: NoteTone::NeedsYou,
            action: Some(CardAction::ReadyForReview),
        },
        (PullRequestState::Open, false) => {
            let (checks, tone) = match pr.checks {
                Checks::None => ("no checks", NoteTone::Waiting),
                Checks::Running => ("checks running", NoteTone::Waiting),
                Checks::Passing => ("checks passing", NoteTone::Waiting),
                Checks::Failing => ("checks failing", NoteTone::Failed),
            };
            CardNote::new(format!("PR #{number} · {checks}"), tone)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codescope_core::board::findings::DecisionOption;

    fn tab(running: Option<&[u32]>, claude_idle: Option<bool>, typed: bool) -> TabAtHandover {
        tab_at_handover(&TabProbe {
            restarting: false,
            launched_lately: false,
            shell_gone: false,
            running,
            claude_idle,
            stopped: false,
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
        let stopped = TabProbe {
            restarting: false,
            launched_lately: false,
            shell_gone: false,
            running: Some(&[7]),
            claude_idle: Some(true),
            stopped: true,
            typed: true,
        };
        assert_eq!(tab_at_handover(&stopped), Stopped, "Ctrl+Z hands its keys to the shell");
        for claude_idle in [Some(false), None] {
            let other_status = TabProbe {
                claude_idle,
                ..stopped
            };
            assert_eq!(tab_at_handover(&other_status), Stopped, "mid-turn or without a status");
        }
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
                stopped: false,
                typed: false,
            };
            assert_eq!(tab_at_handover(&probe), TabAtHandover::Busy);
        }
    }

    fn point(id: &str) -> DecisionPoint {
        DecisionPoint {
            id: id.into(),
            question: "Q?".into(),
            context: String::new(),
            options: ['A', 'B']
                .map(|letter| DecisionOption {
                    letter,
                    text: "x".into(),
                })
                .to_vec(),
            advice: 'A',
        }
    }

    fn answer(author: &str, choices: &[(&str, char)]) -> Answer {
        Answer {
            author: author.into(),
            created_at: "t".into(),
            url: "u".into(),
            choices: choices.iter().map(|(id, l)| (id.to_string(), *l)).collect(),
            body: String::new(),
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
                stopped: false,
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
                stopped: false,
                typed: false,
            })
        };
        assert_eq!(gone(Some(&[41]), Some(true)), Other, "its terminal no longer takes input");
        assert_eq!(gone(Some(&[41]), Some(false)), Other);
        assert_eq!(gone(Some(&[]), None), Shell);
    }

    #[test]
    fn a_turn_counts_as_running_only_while_its_transcript_says_so() {
        use codescope_core::SessionState::*;
        assert_eq!(turn_of(Idle, false), Turn::Ended);
        assert_eq!(turn_of(Busy, false), Turn::Running);
        assert_eq!(turn_of(PendingToolUse, false), Turn::Running);
        assert_eq!(turn_of(Unknown, false), Turn::Unknown);
        assert_eq!(turn_of(Idle, true), Turn::Quiet, "a guess at its end, or a killed agent");
    }

    #[test]
    fn findings_wait_for_every_session_on_the_worktree_and_only_claude_code_is_reminded() {
        use FindingsGate::{Held, Settled, Wait};
        use RemindBy::{Notice, Typing};
        use TabAtHandover::*;
        use Turn::{Ended, Quiet, Running, Unknown};
        assert_eq!(findings_gate(&[]), Settled(Notice), "with no session left, the user is told");
        assert_eq!(findings_gate(&[(Shell, Unknown), (IdleClaude, Ended)]), Settled(Typing(1)));
        assert_eq!(
            findings_gate(&[(IdleClaude, Ended), (Busy, Running)]),
            Wait,
            "another session there still works"
        );
        assert_eq!(
            findings_gate(&[(Draft, Ended)]),
            Settled(Notice),
            "the findings can be posted; problems go to the user, not into the tab"
        );
        assert_eq!(
            findings_gate(&[(Stopped, Ended), (IdleClaude, Ended)]),
            Held(HeldBy::StoppedSession),
            "a stopped session may be mid-turn, and only the user can resume it"
        );
        assert_eq!(findings_gate(&[(Draft, Ended), (IdleClaude, Ended)]), Settled(Typing(1)));
        assert_eq!(
            findings_gate(&[(Other, Running), (Shell, Unknown)]),
            Wait,
            "another agent still in its turn"
        );
        assert_eq!(
            findings_gate(&[(Other, Unknown), (Shell, Unknown)]),
            Held(HeldBy::Program),
            "a program that is no agent only the user can stop"
        );
        assert_eq!(
            findings_gate(&[(Other, Quiet), (Shell, Unknown)]),
            Held(HeldBy::QuietAgent),
            "an agent whose transcript went quiet may or may not be done"
        );
        assert_eq!(findings_gate(&[(Other, Unknown), (Busy, Running)]), Wait);
        assert_eq!(
            findings_gate(&[(Other, Ended)]),
            Settled(Notice),
            "another agent whose turn ended is done; the user is told, nothing is typed"
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
        assert_eq!(handover_to(&[Stopped, Shell]), Handover::Wait(Hold::SessionStopped));
        assert_eq!(handover_to(&[Stopped, Draft]), Handover::Wait(Hold::SessionStopped));
        assert_eq!(handover_to(&[Stopped, IdleClaude]), Handover::Type(1));
        assert_eq!(handover_to(&[Stopped, Busy]), Handover::Wait(Hold::SessionBusy));
    }

    #[test]
    fn unanswered_points_offer_the_decide_button() {
        let discussion = Discussion {
            points: vec![point("B1"), point("B2")],
            answers: Vec::new(),
        };
        let note = decision_note(Some(&discussion), Some("maui1911"), false);
        assert_eq!(note.text.as_ref(), "2 decision points · decide here or on the issue");
        assert!(note.action == Some(CardAction::Decide));
        let unknown = decision_note(None, Some("maui1911"), false);
        assert_eq!(unknown.text.as_ref(), "Decision points posted · answer on the issue");
        assert!(unknown.action.is_none());
    }

    #[test]
    fn an_answer_names_who_gave_it_and_what_was_chosen() {
        let mut discussion = Discussion {
            points: vec![point("B1"), point("B2")],
            answers: vec![answer("Maui1911", &[("B1", 'B'), ("B2", 'A')])],
        };
        let mine = decision_note(Some(&discussion), Some("maui1911"), false);
        assert_eq!(mine.text.as_ref(), "Answered by you: B1 B, B2 A · move it to Ready");
        assert!(mine.action.is_none());
        let readied = decision_note(Some(&discussion), Some("maui1911"), true);
        assert_eq!(readied.text.as_ref(), "Answered by you: B1 B, B2 A · implementation starts");
        discussion.answers.push(answer("sanne", &[]));
        let theirs = decision_note(Some(&discussion), Some("maui1911"), false);
        assert_eq!(theirs.text.as_ref(), "Answered by @sanne: B1 B, B2 A · move it to Ready");
    }

    fn open_pull_request(number: u32, is_draft: bool, checks: Checks) -> PullRequest {
        PullRequest {
            id: format!("PR_{number}"),
            number,
            url: format!("https://github.com/maui1911/CodeScope/pull/{number}"),
            state: PullRequestState::Open,
            is_draft,
            checks,
        }
    }

    fn posted(number: u32) -> PostedPullRequest {
        PostedPullRequest {
            number,
            url: "u".into(),
            comment_url: "c".into(),
            in_review: false,
        }
    }

    #[test]
    fn a_draft_needs_testing_by_hand_and_offers_ready_for_review() {
        let watch = PullRequestWatch {
            pull_request: Some(open_pull_request(415, true, Checks::Running)),
            read_ok_at: Some(Instant::now()),
            ..PullRequestWatch::default()
        };
        let note = pull_request_note(&watch, Some(&posted(415))).unwrap();
        assert_eq!(note.text.as_ref(), "Draft PR #415 · test by hand");
        assert!(note.tone == NoteTone::NeedsYou);
        assert!(note.action == Some(CardAction::ReadyForReview));
    }

    #[test]
    fn a_ready_pull_request_shows_its_checks_and_red_ones_as_failed() {
        let mut watch = PullRequestWatch {
            pull_request: Some(open_pull_request(415, false, Checks::Passing)),
            read_ok_at: Some(Instant::now()),
            ..PullRequestWatch::default()
        };
        let note = pull_request_note(&watch, Some(&posted(415))).unwrap();
        assert_eq!(note.text.as_ref(), "PR #415 · checks passing");
        assert!(note.tone == NoteTone::Waiting && note.action.is_none());
        watch.pull_request = Some(open_pull_request(415, false, Checks::Failing));
        let note = pull_request_note(&watch, Some(&posted(415))).unwrap();
        assert_eq!(note.text.as_ref(), "PR #415 · checks failing");
        assert!(note.tone == NoteTone::Failed);
    }

    #[test]
    fn a_session_that_worked_and_stopped_without_a_pull_request_waits_for_you() {
        let mut watch = PullRequestWatch::default();
        watch.track(Some((SessionState::Idle, 3)));
        watch.read_ok_at = Some(Instant::now());
        assert!(!watch.waits_for_you(), "it never worked while watched");
        watch.track(Some((SessionState::Busy, 4)));
        watch.track(Some((SessionState::Idle, 4)));
        let stopped = watch.idle_since.unwrap();
        watch.read_ok_at = Some(stopped - Duration::from_millis(1));
        assert!(!watch.waits_for_you(), "no read since it stopped");
        watch.read_ok_at = Some(stopped + Duration::from_millis(1));
        assert!(watch.waits_for_you());
        let note = pull_request_note(&watch, None).unwrap();
        assert_eq!(note.text.as_ref(), "Waiting for you in the tab");
        assert!(note.tone == NoteTone::NeedsYou);
        watch.track(None);
        assert!(!watch.waits_for_you(), "the tab is closed");
    }

    #[test]
    fn a_stop_reads_the_pull_request_again_at_once() {
        let mut watch = PullRequestWatch::default();
        assert!(watch.read_due(Some(ColumnRole::InProgress)));
        assert!(!watch.read_due(Some(ColumnRole::Done)), "no open pull request to follow");
        watch.read_at = Some(Instant::now());
        assert!(!watch.read_due(Some(ColumnRole::InProgress)));
        watch.track(Some((SessionState::Busy, 4)));
        watch.track(Some((SessionState::Idle, 4)));
        assert!(watch.read_due(Some(ColumnRole::InProgress)));
        watch.reading = true;
        assert!(!watch.read_due(Some(ColumnRole::InProgress)));
    }

    #[test]
    fn a_turn_between_two_looks_counts_as_work_and_as_a_new_stop() {
        let mut watch = PullRequestWatch::default();
        watch.track(Some((SessionState::Idle, 63)));
        let first = watch.idle_since;
        watch.track(Some((SessionState::Idle, 63)));
        assert!(!watch.busy_seen);
        assert_eq!(watch.idle_since, first);
        std::thread::sleep(Duration::from_millis(2));
        watch.track(Some((SessionState::Idle, 64)));
        assert!(watch.busy_seen);
        assert!(watch.idle_since > first);
    }
}
