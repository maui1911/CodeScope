//! The item drawer's contents (ADR-0027): one board item as CodeScope knows it from the last
//! board read and the worktree row of its dispatch, and what the drawer read when it opened.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, Instant};

use codescope_core::board::dispatch::Round;
use codescope_core::board::github;
use codescope_core::board::pull_request::{
    Checks, PullRequest, PullRequestState, moves_to_in_review,
};
use codescope_core::board::repair::{FaultKind, REPAIR_ATTEMPTS, Repair};
use codescope_core::board::{BoardItem, ColumnRole, ItemKind, ItemState, RoleKind};
use codescope_core::SessionState;
use gpui::{App, AppContext as _, Context, SharedString};

use super::details::{DetailsView, details_view};
use super::{AppShell, CardAction, CardNote, NoteTone, ToastKind};
use crate::board_panel::{BoardRead, MoveBack};

/// One board item, as the drawer shows it.
pub(crate) struct ItemDrawer {
    pub board_url: String,
    pub item_id: String,
    pub kind: ItemKind,
    pub state: ItemState,
    /// `#N · owner/name · issue`.
    pub reference: String,
    pub title: String,
    /// The column it is in; `None` without a status.
    pub column: Option<String>,
    /// The round its dispatch started last; `None` when nothing was dispatched.
    pub round: Option<&'static str>,
    pub url: Option<String>,
    /// What the card's button asks of the user, when it has one.
    pub needs: Option<Needs>,
    /// The card's line, when it has no button.
    pub note: Option<(SharedString, NoteTone)>,
    pub rows: Vec<Row>,
    /// Set when the item has a dispatch, whose tab *Open tab* opens.
    pub dispatched: bool,
    /// The dispatch's pull request on GitHub, once read.
    pub pull_request_url: Option<String>,
    /// Set while it can move back to Todo.
    pub back_to_todo: Option<BackToTodo>,
    /// What the drawer read when it opened.
    pub details: DetailsView,
}

/// What the card's button asks of the user, in the drawer's own words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Needs {
    pub action: CardAction,
    pub title: String,
    pub text: String,
    /// What the button does.
    pub hint: String,
    /// A second button that opens a page on GitHub: its label and URL.
    pub link: Option<(&'static str, String)>,
}

/// A line of the Dispatch section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Row {
    pub label: &'static str,
    pub value: String,
    /// A word after the value in a colour of its own, such as "linked".
    pub mark: Option<(String, Mark)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    Good,
    Bad,
    Busy,
    Plain,
}

pub(crate) struct BackToTodo {
    pub hint: &'static str,
    pub moving: bool,
}

impl Row {
    fn new(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            label,
            value: value.into(),
            mark: None,
        }
    }

    fn marked(mut self, mark: Option<(String, Mark)>) -> Self {
        self.mark = mark;
        self
    }
}

impl AppShell {
    /// The drawer for `item_id` on the board at `board_url`, while the last read has the item.
    pub(crate) fn item_drawer(
        &self,
        board_url: &str,
        item_id: &str,
        notes: &HashMap<String, CardNote>,
        cx: &App,
    ) -> Option<ItemDrawer> {
        let (board, mapping) = self.read_board(board_url)?;
        let item = board.items.iter().find(|item| item.id == item_id)?;
        let role = board.role_of(item, &mapping);
        let viewer = self.board.dispatch.viewer.as_ref().map(|v| v.login.as_str());
        let dispatch = self
            .dispatch_records(cx)
            .into_iter()
            .find(|(_, _, _, r)| r.board_url == board_url && r.item_id == item_id);
        let watch = self.board.dispatch.watches.get(item_id);
        let read_pr = watch.and_then(|watch| watch.pull_request.as_ref());
        let note = notes.get(item_id);

        let mut rows = vec![Row::new("Assigned", assigned(&item.assignees, viewer))];
        if let Some(turn) = self.turn_labels(board_url).remove(item_id) {
            let needs = turn.yours.then(|| ("needs you".to_string(), Mark::Busy));
            rows.push(Row::new("Turn", turn.text.to_string()).marked(needs));
        }
        let mut needs_link = None;
        if let Some((project_id, path, branch, record)) = &dispatch {
            rows.push(Row::new("Worktree", worktree_label(path)));
            if let Some(branch) = branch {
                let linked = record.linked_at.is_some().then(|| ("linked".into(), Mark::Good));
                rows.push(Row::new("Branch", branch.clone()).marked(linked));
            }
            let posted = record.pull_request.as_ref().or(watch.and_then(|w| w.reported.as_ref()));
            match (read_pr, posted) {
                (Some(pr), _) => {
                    let (value, mark) = pull_request_row(pr);
                    rows.push(Row::new("Pull request", value).marked(Some(mark)));
                    rows.push(Row::new("Repairs", repairs_row(record.repair.as_ref())));
                }
                (None, Some(posted)) => {
                    rows.push(Row::new("Pull request", format!("#{}", posted.number)));
                }
                (None, None) => {}
            }
            let agent = record
                .agent
                .as_deref()
                .map(|id| self.agent_registry.get_by_id(id).map_or(id, |p| &p.display_name))
                .map(str::to_string)
                .or_else(|| {
                    self.round_agent_of(project_id, record.round, cx).map(|p| p.display_name)
                });
            if let Some(agent) = agent {
                rows.push(Row::new("Agent", agent));
            }
            let agent = record.agent.as_deref();
            let tabs: Vec<_> = self
                .tabs_on(path)
                .into_iter()
                .filter(|tab| !super::runs_another_agent(tab.agent_profile_id.as_deref(), agent))
                .collect();
            let state = tabs.iter().find_map(|tab| {
                let sid = tab.adopted_session_id.as_deref()?;
                self.telemetry_for(sid).map(|snapshot| crate::app::notified_state(&snapshot))
            });
            let idle_for = watch.and_then(|w| w.idle_since).map(|since| since.elapsed());
            let session = session_row((!tabs.is_empty()).then_some(state), idle_for);
            rows.push(Row::new("Session", session));
            rows.push(Row::new("Started", started_row(record.round, &record.trigger_at, now())));
            needs_link = match note.and_then(|n| n.action) {
                Some(CardAction::Decide) => record
                    .findings
                    .as_ref()
                    .map(|f| ("Open findings", f.comment_url.clone())),
                _ => read_pr.map(|pr| ("Open PR", pr.url.clone())),
            };
        }

        let needs = note.and_then(|note| {
            let action = note.action?;
            let points = dispatch.as_ref().map_or(0, |(_, _, _, r)| {
                self.board.dispatch.discussions.get(&r.content_id).map_or(0, |d| d.points.len())
            });
            let asks = dispatch.as_ref().zip(read_pr).and_then(|((project_id, _, _, r), pr)| {
                self.review_request((project_id.as_str(), r), pr, cx).map(|request| request.names())
            });
            let open_item = match item.kind {
                ItemKind::PullRequest => "Open PR",
                ItemKind::Issue | ItemKind::Draft => "Open issue",
            };
            let link = needs_link.clone().or_else(|| item.url.clone().map(|url| (open_item, url)));
            let pr = read_pr.map(|pr| pr.number);
            let posted = dispatch.as_ref().and_then(|(_, _, _, r)| {
                r.pull_request.as_ref().or(watch.and_then(|w| w.reported.as_ref()))
            });
            let moves = posted.is_some_and(|posted| !posted.in_review)
                && read_pr.is_some_and(|pr| {
                    let ready = PullRequest {
                        is_draft: false,
                        ..pr.clone()
                    };
                    moves_to_in_review(&ready, role)
                });
            let facts = Facts {
                pr,
                moves,
                points,
                asks,
                link,
            };
            Some(needs(action, item.kind, facts, &note.text))
        });
        let note = note
            .filter(|_| needs.is_none())
            .map(|note| (note.text.clone(), note.tone));
        let open = item.state == ItemState::Open;
        let moving = self.board.moved_back.get(item_id).is_some_and(|back| back.pending);
        let back_to_todo = (open && (moving || can_move_back(role)))
            .then(|| self.column_target(board_url, ColumnRole::Todo))
            .flatten()
            .map(|_| BackToTodo {
                hint: if dispatch.is_some() {
                    "CodeScope sends nothing more into its session. The tab and the worktree stay."
                } else {
                    "Nothing starts from it until it is moved again."
                },
                moving,
            });

        Some(ItemDrawer {
            board_url: board_url.to_string(),
            item_id: item_id.to_string(),
            kind: item.kind,
            state: item.state,
            reference: reference(item.kind, item.number, item.repository.as_deref()),
            title: item.title.clone(),
            column: item
                .column_id
                .as_deref()
                .and_then(|id| board.column(id))
                .map(|column| column.name.clone()),
            round: dispatch.as_ref().map(|(_, _, _, r)| r.round.label()),
            url: item.url.clone(),
            needs,
            note,
            rows,
            dispatched: dispatch.is_some(),
            pull_request_url: dispatch.as_ref().and(read_pr).map(|pr| pr.url.clone()),
            back_to_todo,
            details: details_view(self.board.details.as_ref(), item_id, viewer, now()),
        })
    }
}

impl AppShell {
    /// Move `item_id` back to Todo for the user. It is their own move, so CodeScope's moves for
    /// the round end there; the panel shows it in Todo at once, and CodeScope acts on that.
    pub(crate) fn move_back_to_todo(
        &mut self,
        board_url: &str,
        item_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some((board, column)) = self.column_target(board_url, ColumnRole::Todo) else {
            return;
        };
        if self.board.moved_back.get(item_id).is_some_and(|back| back.pending) {
            return;
        }
        let Some(item) = read_item_mut(&mut self.board.reads_by_url, board_url, item_id) else {
            return;
        };
        let from = (item.column_id.replace(column.clone()), item.column_set_at.clone());
        let back = MoveBack {
            column: column.clone(),
            from,
            at: Instant::now(),
            pending: true,
        };
        self.board.moved_back.insert(item_id.to_string(), back);
        cx.notify();
        let (board_url, item_id) = (board_url.to_string(), item_id.to_string());
        cx.spawn(async move |this, cx| {
            let (moved_id, target) = (item_id.clone(), column.clone());
            let result = cx
                .background_spawn(async move {
                    github::set_status(&board, &moved_id, &target).map_err(|err| format!("{err:#}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_move_back(&board_url, &item_id, result, cx);
            });
        })
        .detach();
    }

    fn finish_move_back(
        &mut self,
        board_url: &str,
        item_id: &str,
        result: Result<(), String>,
        cx: &mut Context<Self>,
    ) {
        cx.notify();
        let item = read_item_mut(&mut self.board.reads_by_url, board_url, item_id);
        if let Err(message) = result {
            let back = self.board.moved_back.remove(item_id);
            let number = item.as_ref().and_then(|item| item.number);
            if let (Some(item), Some(back)) = (item, back)
                && item.column_id.as_ref() == Some(&back.column)
            {
                item.column_id = back.from.0;
            }
            let title = number.map_or_else(
                || "Couldn't move it back to Todo".to_string(),
                |n| format!("Couldn't move #{n} back to Todo"),
            );
            self.push_toast(ToastKind::Err, title, Some(message.into()), cx);
            return;
        }
        if let Some(back) = self.board.moved_back.get_mut(item_id) {
            back.pending = false;
            back.at = Instant::now();
            if let Some(item) = item {
                item.column_id = Some(back.column.clone());
                item.column_set_at = Some(codescope_core::time::now_iso8601());
            }
        }
        self.refresh_boards(cx);
    }
}

fn read_item_mut<'a>(
    reads: &'a mut HashMap<String, BoardRead>,
    board_url: &str,
    item_id: &str,
) -> Option<&'a mut BoardItem> {
    let board = reads.get_mut(board_url)?.last_good.as_mut()?;
    board.items.iter_mut().find(|item| item.id == item_id)
}

fn now() -> f64 {
    codescope_core::time::parse_iso8601_secs(&codescope_core::time::now_iso8601()).unwrap_or(0.0)
}

/// Whether an item in a column with `role` can move back to Todo: not from Todo itself, and not
/// from a column GitHub's workflows fill.
fn can_move_back(role: Option<ColumnRole>) -> bool {
    !matches!(role, Some(ColumnRole::Todo)) && role.map(ColumnRole::kind) != Some(RoleKind::Host)
}

/// What [`needs`] says about the item besides the button.
struct Facts {
    /// The dispatch's pull request, once read.
    pr: Option<u32>,
    /// Whether marking it ready moves the item on to In review, as it does from In progress.
    moves: bool,
    /// How many decision points the findings ask.
    points: usize,
    /// Whom the move to In review asks for a review.
    asks: Option<String>,
    link: Option<(&'static str, String)>,
}

fn needs(action: CardAction, kind: ItemKind, facts: Facts, note: &str) -> Needs {
    let pr = facts.pr.map_or_else(|| "the pull request".to_string(), |n| format!("PR #{n}"));
    let ready_hint = match (facts.moves, facts.asks) {
        (false, _) => "Ready for review marks the PR ready.".to_string(),
        (true, Some(names)) => format!(
            "Ready for review marks the PR ready, moves the item to In review and asks {names} \
             for a review."
        ),
        (true, None) => {
            "Ready for review marks the PR ready and moves the item to In review.".to_string()
        }
    };
    let then = if facts.moves {
        " The item moves to In review once it is marked ready."
    } else {
        ""
    };
    let (title, text, hint) = match (action, kind) {
        (CardAction::ReadyForReview, ItemKind::PullRequest) => (
            "Draft pull request".to_string(),
            format!("{pr} is a draft.{then}"),
            ready_hint,
        ),
        (CardAction::ReadyForReview, _) => (
            "Test this by hand".to_string(),
            format!("The agent opened {pr} as a draft: part of it needs testing by hand."),
            ready_hint,
        ),
        (CardAction::Decide, _) => {
            let points = match facts.points {
                0 => "decision points".to_string(),
                1 => "1 decision point".to_string(),
                n => format!("{n} decision points"),
            };
            (
                "Decision points".to_string(),
                format!("The findings ask {points}, each with the agent's advice."),
                "Decide posts your answers where the findings are, and can move the item to Ready."
                    .to_string(),
            )
        }
        (CardAction::StartFromFork, _) => (
            "A pull request from a fork".to_string(),
            note.to_string(),
            "Its branch can change the skills and CLAUDE.md the agent loads, so it starts only \
             once you say so."
                .to_string(),
        ),
        (CardAction::PickProject, _) => (
            "Which project?".to_string(),
            note.to_string(),
            "Pick the one that works on it, for this item or for every item of its repository \
             on this board."
                .to_string(),
        ),
    };
    Needs {
        action,
        title,
        text,
        hint,
        link: facts.link,
    }
}

/// `#N · owner/name · issue`; a draft issue has neither number nor repository.
fn reference(kind: ItemKind, number: Option<u32>, repository: Option<&str>) -> String {
    match (number, repository) {
        (Some(number), Some(repository)) => format!("#{number} · {repository} · {}", kind.noun()),
        _ => kind.noun().to_string(),
    }
}

/// The assignees, the user first as "you"; "nobody" without one.
fn assigned(assignees: &[String], viewer: Option<&str>) -> String {
    let is_viewer = |login: &String| viewer.is_some_and(|v| v.eq_ignore_ascii_case(login));
    let mut names: Vec<String> = assignees
        .iter()
        .filter(|login| !is_viewer(login))
        .map(|login| format!("@{login}"))
        .collect();
    if assignees.iter().any(is_viewer) {
        names.insert(0, "you".to_string());
    }
    match names.as_slice() {
        [] => "nobody".to_string(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// The worktree's folder with its parent, as `codescope.worktrees/411-fuzzy-paths`.
fn worktree_label(path: &Path) -> String {
    let name = path.file_name().map(|n| n.to_string_lossy());
    let parent = path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy());
    match (parent, name) {
        (Some(parent), Some(name)) => format!("{parent}/{name}"),
        _ => path.to_string_lossy().into_owned(),
    }
}

fn pull_request_row(pr: &PullRequest) -> (String, (String, Mark)) {
    let state = match (pr.state, pr.is_draft) {
        (PullRequestState::Merged, _) => "merged",
        (PullRequestState::Open, true) => "draft",
        (PullRequestState::Open, false) => "open",
    };
    let checks = match pr.checks {
        Checks::None => ("no checks", Mark::Plain),
        Checks::Running => ("checks running", Mark::Busy),
        Checks::Passing => ("checks passing", Mark::Good),
        Checks::Failing => ("checks failing", Mark::Bad),
    };
    (format!("#{} · {state}", pr.number), (checks.0.to_string(), checks.1))
}

fn repairs_row(repair: Option<&Repair>) -> String {
    let Some(repair) = repair else {
        return format!("none · at most {REPAIR_ATTEMPTS} in a row");
    };
    let fault = match repair.fault {
        FaultKind::Conflict => "a conflict with its base",
        FaultKind::Behind => "catching up with its base",
        FaultKind::Checks => "failed checks",
    };
    format!("{} of {REPAIR_ATTEMPTS} in a row · for {fault}", repair.attempts)
}

/// The round's session: working, idle and for how long, or without a tab. `session` is `None`
/// without a tab, and holds `None` for a tab whose turns CodeScope can't read.
fn session_row(session: Option<Option<SessionState>>, idle_for: Option<Duration>) -> String {
    match session {
        None => "no tab open".to_string(),
        Some(Some(SessionState::Busy | SessionState::PendingToolUse)) => "working".to_string(),
        Some(Some(SessionState::Idle)) => match idle_for {
            Some(idle) => format!("idle · {}", codescope_core::time::span(idle.as_secs_f64())),
            None => "idle".to_string(),
        },
        Some(Some(SessionState::Unknown) | None) => "open".to_string(),
    }
}

/// When the round started, and by a move into which column.
fn started_row(round: Round, trigger_at: &str, now: f64) -> String {
    let column = match round {
        Round::Research => ColumnRole::Research,
        Round::Implementation | Round::Review => ColumnRole::Ready,
    };
    let moved = format!("moved to {}", column.label());
    match codescope_core::time::secs_since(trigger_at, now) {
        Some(secs) => format!("{} ago · {moved}", codescope_core::time::span(secs)),
        None => moved,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codescope_core::board::pull_request::Health;

    fn pull_request(state: PullRequestState, is_draft: bool, checks: Checks) -> PullRequest {
        PullRequest {
            id: "PR_1".into(),
            number: 419,
            url: "https://github.com/acme/app/pull/419".into(),
            author: Some("maui1911".into()),
            state,
            is_draft,
            checks,
            head_oid: "c419".into(),
            health: Health::default(),
            comments: Vec::new(),
        }
    }

    fn facts(asks: Option<&str>) -> Facts {
        Facts {
            pr: Some(419),
            moves: true,
            points: 2,
            asks: asks.map(str::to_string),
            link: Some(("Open PR", "https://github.com/acme/app/pull/419".into())),
        }
    }

    #[test]
    fn the_reference_names_number_repository_and_kind() {
        let reference = |kind, number, repo| super::reference(kind, number, repo);
        assert_eq!(
            reference(ItemKind::Issue, Some(411), Some("acme/app")),
            "#411 · acme/app · issue"
        );
        assert_eq!(
            reference(ItemKind::PullRequest, Some(12), Some("acme/app")),
            "#12 · acme/app · pull request"
        );
        assert_eq!(reference(ItemKind::Draft, None, None), "draft issue");
    }

    #[test]
    fn the_user_comes_first_among_the_assignees_as_you() {
        let logins = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(assigned(&[], Some("maui1911")), "nobody");
        assert_eq!(assigned(&logins(&["Maui1911"]), Some("maui1911")), "you");
        assert_eq!(assigned(&logins(&["ana", "maui1911"]), Some("maui1911")), "you and @ana");
        assert_eq!(assigned(&logins(&["ana", "bob", "cy"]), None), "@ana, @bob and @cy");
    }

    #[test]
    fn a_worktree_shows_as_its_folder_with_its_parent() {
        let path = Path::new("D:/Dev/CodeScope.worktrees/411-fuzzy-paths");
        assert_eq!(worktree_label(path), "CodeScope.worktrees/411-fuzzy-paths");
    }

    #[test]
    fn a_pull_request_row_names_its_state_and_marks_its_checks() {
        let draft = pull_request(PullRequestState::Open, true, Checks::Passing);
        assert_eq!(
            pull_request_row(&draft),
            ("#419 · draft".to_string(), ("checks passing".to_string(), Mark::Good))
        );
        let open = pull_request(PullRequestState::Open, false, Checks::Failing);
        assert_eq!(pull_request_row(&open).0, "#419 · open");
        assert_eq!(pull_request_row(&open).1, ("checks failing".to_string(), Mark::Bad));
        let merged = pull_request(PullRequestState::Merged, false, Checks::None);
        assert_eq!(
            pull_request_row(&merged),
            ("#419 · merged".to_string(), ("no checks".to_string(), Mark::Plain))
        );
    }

    #[test]
    fn repairs_count_against_the_limit() {
        assert_eq!(repairs_row(None), "none · at most 2 in a row");
        let repair = Repair {
            attempts: 1,
            fault: FaultKind::Checks,
            started_at: "2026-11-27T10:00:00Z".into(),
            head: "c419".into(),
        };
        assert_eq!(repairs_row(Some(&repair)), "1 of 2 in a row · for failed checks");
    }

    #[test]
    fn the_session_says_whether_it_works_and_how_long_it_has_been_idle() {
        assert_eq!(session_row(None, None), "no tab open");
        assert_eq!(session_row(Some(Some(SessionState::Busy)), None), "working");
        assert_eq!(session_row(Some(Some(SessionState::PendingToolUse)), None), "working");
        let six = Some(Duration::from_secs(6 * 60 + 5));
        assert_eq!(session_row(Some(Some(SessionState::Idle)), six), "idle · 6 min");
        assert_eq!(session_row(Some(Some(SessionState::Idle)), None), "idle");
        assert_eq!(session_row(Some(None), None), "open");
    }

    #[test]
    fn a_round_started_by_a_move_into_its_trigger_column() {
        let now = codescope_core::time::parse_iso8601_secs("2026-11-27T10:06:00Z").unwrap();
        assert_eq!(
            started_row(Round::Implementation, "2026-11-27T10:00:00Z", now),
            "6 min ago · moved to Ready"
        );
        assert_eq!(started_row(Round::Review, "garbled", now), "moved to Ready");
        assert_eq!(
            started_row(Round::Research, "2026-11-26T10:00:00Z", now),
            "24 h ago · moved to Research"
        );
    }

    #[test]
    fn testing_by_hand_says_whom_ready_for_review_asks() {
        let needs = needs(CardAction::ReadyForReview, ItemKind::Issue, facts(Some("ana")), "");
        assert_eq!(needs.title, "Test this by hand");
        assert_eq!(
            needs.text,
            "The agent opened PR #419 as a draft: part of it needs testing by hand."
        );
        assert_eq!(
            needs.hint,
            "Ready for review marks the PR ready, moves the item to In review and asks ana for a \
             review."
        );
        let nobody = super::needs(CardAction::ReadyForReview, ItemKind::Issue, facts(None), "");
        assert_eq!(
            nobody.hint,
            "Ready for review marks the PR ready and moves the item to In review."
        );
    }

    #[test]
    fn a_draft_pull_request_item_and_decision_points_get_their_own_words() {
        let draft = needs(CardAction::ReadyForReview, ItemKind::PullRequest, facts(None), "");
        assert_eq!(draft.title, "Draft pull request");
        assert_eq!(
            draft.text,
            "PR #419 is a draft. The item moves to In review once it is marked ready."
        );
        let decide = needs(CardAction::Decide, ItemKind::Issue, facts(None), "");
        assert_eq!(
            decide.text,
            "The findings ask 2 decision points, each with the agent's advice."
        );
        let fork = needs(CardAction::StartFromFork, ItemKind::PullRequest, facts(None), "Fork");
        assert_eq!(fork.text, "Fork");
        let several = "Several projects have maui1911/CodeScope as a remote";
        let pick = needs(CardAction::PickProject, ItemKind::Issue, facts(None), several);
        assert_eq!((pick.title.as_str(), pick.text.as_str()), ("Which project?", several));
    }

    #[test]
    fn ready_for_review_outside_in_progress_promises_only_the_ready_pull_request() {
        let stays = || Facts {
            moves: false,
            ..facts(Some("ana"))
        };
        let issue = needs(CardAction::ReadyForReview, ItemKind::Issue, stays(), "");
        assert_eq!(issue.hint, "Ready for review marks the PR ready.");
        let draft = needs(CardAction::ReadyForReview, ItemKind::PullRequest, stays(), "");
        assert_eq!(draft.text, "PR #419 is a draft.");
        assert_eq!(draft.hint, "Ready for review marks the PR ready.");
    }

    #[test]
    fn only_an_item_outside_todo_and_done_moves_back_to_todo() {
        assert!(can_move_back(Some(ColumnRole::InProgress)));
        assert!(can_move_back(Some(ColumnRole::Ready)));
        assert!(can_move_back(None));
        assert!(!can_move_back(Some(ColumnRole::Todo)));
        assert!(!can_move_back(Some(ColumnRole::Done)));
    }
}
