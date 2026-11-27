//! The pull request an implementation round opens, as the board poll reads it, the comment that
//! reports it on the issue, and whether a review round on it did something.

use serde::{Deserialize, Serialize};

use super::dispatch::Round;
use super::{ColumnRole, ItemKind};
use crate::time::parse_iso8601_secs;

const MARKER: &str =
    "<!-- dispatch v=1 phase=implementation outcome=pull-request by=codescope -->";

/// Whether a pull request is still open or was merged; closed ones are never read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequestState {
    Open,
    Merged,
}

/// The combined result of the checks on a pull request's last commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checks {
    /// The commit has no checks.
    None,
    Running,
    Passing,
    Failing,
}

impl Checks {
    /// From GitHub's `statusCheckRollup.state`; `None` when the commit has no rollup.
    pub fn from_rollup(state: Option<&str>) -> Checks {
        match state {
            None => Checks::None,
            Some("SUCCESS") => Checks::Passing,
            Some("FAILURE" | "ERROR") => Checks::Failing,
            Some(_) => Checks::Running,
        }
    }
}

/// A pull request on a dispatched worktree's branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    /// Node id, for mutations.
    pub id: String,
    pub number: u32,
    pub url: String,
    /// Its author's login; `None` for a deleted account.
    pub author: Option<String>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub checks: Checks,
    /// The commit its branch points at.
    pub head_oid: String,
    /// What keeps it from merging, as the last read found it.
    pub health: Health,
    /// Its last 50 conversation comments, oldest first.
    pub comments: Vec<PullRequestComment>,
}

/// A conversation comment on a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestComment {
    pub url: String,
    /// `None` for a deleted account, a bot or an app.
    pub author: Option<String>,
    /// ISO 8601 in UTC, as GitHub writes it.
    pub created_at: String,
    /// Written by the user signed in to `gh`.
    pub by_viewer: bool,
}

/// Whether GitHub can merge a pull request into its base without a conflict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mergeable {
    /// GitHub hasn't worked it out yet.
    #[default]
    Unknown,
    Yes,
    Conflicts,
}

/// A check that failed on a pull request's last commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCheck {
    /// The check run's name, or the status's context.
    pub name: String,
    /// Its page on GitHub; empty when it has none.
    pub url: String,
}

/// What stands between a pull request and its merge, as the last read found it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Health {
    /// The branch it merges into.
    pub base: String,
    /// `Unknown` for a while after a push.
    pub mergeable: Mergeable,
    /// GitHub asks it to catch up with its base before it can merge.
    pub behind: bool,
    /// Someone submitted a review on it.
    pub reviewed: bool,
    /// The checks that failed on its last commit.
    pub failed_checks: Vec<FailedCheck>,
    /// Whether a check on its last commit hasn't finished.
    pub checks_running: bool,
}

/// The pull request a round produced, from those on its branch newest first: the newest open
/// one, else the newest merged one.
pub fn round_pull_request(newest_first: Vec<PullRequest>) -> Option<PullRequest> {
    let pick = newest_first
        .iter()
        .position(|pr| pr.state == PullRequestState::Open)
        .unwrap_or(0);
    newest_first.into_iter().nth(pick)
}

/// A pull request that closes an issue when merged: one in the issue's *Development* list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedPullRequest {
    pub number: u32,
    pub open: bool,
    /// The branch it merges from, in its head repository.
    pub head_ref_name: String,
    /// Set when that branch lives in another repository, such as a fork.
    pub is_cross_repository: bool,
    /// ISO 8601.
    pub created_at: String,
    /// When it was last closed or merged, ISO 8601; `None` when it never was.
    pub closed_at: Option<String>,
}

/// The branch a pull request item merges from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestHead {
    pub branch: String,
    /// Set when the branch lives in another repository, such as a fork.
    pub is_cross_repository: bool,
    /// The repository the branch lives in, `owner/name`; `None` once it is deleted.
    pub repository: Option<String>,
    /// Whether its author lets the base repository's maintainers push to the branch.
    pub maintainer_can_modify: bool,
}

/// A pull request CodeScope reported on the issue, or the pull request that is the item, kept on
/// the worktree's `projects.json` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostedPullRequest {
    pub number: u32,
    pub url: String,
    /// The comment that reported it on the issue; empty when the item is the pull request.
    pub comment_url: String,
    /// Set once CodeScope moved the item to In review, or found it there; it never moves it
    /// again.
    #[serde(default)]
    pub in_review: bool,
    /// When CodeScope reported it on the issue, ISO 8601 by GitHub's clock; `None` when the item
    /// is the pull request, or the report came before CodeScope kept its time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reported_at: Option<String>,
}

/// The comment that reports a pull request after `round` on the item of `kind`: what is open or
/// was updated, a `**Next step:**` line and the marker. Only a dispatch's draft is tested by hand.
pub fn pull_request_comment(pr: &PullRequest, round: Round, kind: ItemKind) -> String {
    let (state, again) = match round {
        Round::Review => ("is updated for the review", " again"),
        Round::Research | Round::Implementation => ("is open", ""),
    };
    let number = pr.number;
    let (opened, next_step) = match (pr.is_draft, kind) {
        (true, ItemKind::PullRequest) => (
            format!("Draft pull request #{number} {state}."),
            format!("review it{again}, and mark it ready for review once it is."),
        ),
        (true, ItemKind::Issue | ItemKind::Draft) => (
            format!("Draft pull request #{number} {state}: part of it needs testing by hand."),
            "test it as its *Test by hand* section says, then mark it ready for review."
                .to_string(),
        ),
        (false, _) => (format!("Pull request #{number} {state}."), format!("review it{again}.")),
    };
    format!("{opened}\n\n**Next step:** {next_step}\n{MARKER}\n")
}

/// Whether a review round that started at `since` did something on its pull request: pushed
/// past `start_head`, or replied in a review thread after the move (`replied_at`, ISO 8601).
pub fn review_round_worked(
    start_head: Option<&str>,
    head: &str,
    replied_at: Option<&str>,
    since: &str,
) -> bool {
    let pushed = start_head.is_some_and(|start| start != head);
    let replied = replied_at
        .and_then(parse_iso8601_secs)
        .zip(parse_iso8601_secs(since))
        .is_some_and(|(at, since)| at > since);
    pushed || replied
}

/// Whether CodeScope moves the item to In review: for an open pull request that is ready, and
/// only while the item is still in In progress, where CodeScope put it.
pub fn moves_to_in_review(pr: &PullRequest, role: Option<ColumnRole>) -> bool {
    pr.state == PullRequestState::Open && !pr.is_draft && role == Some(ColumnRole::InProgress)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull_request(number: u32, state: PullRequestState, is_draft: bool) -> PullRequest {
        PullRequest {
            id: format!("PR_{number}"),
            number,
            url: format!("https://github.com/maui1911/CodeScope/pull/{number}"),
            author: None,
            state,
            is_draft,
            checks: Checks::None,
            head_oid: "h1".into(),
            health: Health::default(),
            comments: Vec::new(),
        }
    }

    #[test]
    fn a_review_round_worked_when_it_pushed_or_replied_in_a_thread_after_its_move() {
        let since = "2026-11-26T10:00:00Z";
        assert!(review_round_worked(Some("h0"), "h1", None, since), "pushed");
        assert!(!review_round_worked(Some("h1"), "h1", None, since));
        assert!(review_round_worked(Some("h1"), "h1", Some("2026-11-26T10:00:01Z"), since));
        assert!(!review_round_worked(Some("h1"), "h1", Some("2026-11-26T09:59:59Z"), since));
        assert!(!review_round_worked(None, "h1", None, since), "no head to tell a push by");
        assert!(!review_round_worked(Some("h1"), "h1", Some("yesterday"), since));
    }

    #[test]
    fn after_a_review_round_the_comment_says_the_pull_request_was_updated() {
        let ready = pull_request(415, PullRequestState::Open, false);
        assert_eq!(
            pull_request_comment(&ready, Round::Review, ItemKind::Issue),
            "Pull request #415 is updated for the review.\n\n\
             **Next step:** review it again.\n\
             <!-- dispatch v=1 phase=implementation outcome=pull-request by=codescope -->\n"
        );
        let draft = pull_request(415, PullRequestState::Open, true);
        assert_eq!(
            pull_request_comment(&draft, Round::Review, ItemKind::Issue),
            "Draft pull request #415 is updated for the review: part of it needs testing by \
             hand.\n\n\
             **Next step:** test it as its *Test by hand* section says, then mark it ready for \
             review.\n\
             <!-- dispatch v=1 phase=implementation outcome=pull-request by=codescope -->\n"
        );
    }

    #[test]
    fn a_draft_pull_request_item_is_not_said_to_need_testing_by_hand() {
        let draft = pull_request(420, PullRequestState::Open, true);
        assert_eq!(
            pull_request_comment(&draft, Round::Review, ItemKind::PullRequest),
            "Draft pull request #420 is updated for the review.\n\n\
             **Next step:** review it again, and mark it ready for review once it is.\n\
             <!-- dispatch v=1 phase=implementation outcome=pull-request by=codescope -->\n"
        );
        let ready = pull_request(420, PullRequestState::Open, false);
        assert_eq!(
            pull_request_comment(&ready, Round::Review, ItemKind::PullRequest),
            pull_request_comment(&ready, Round::Review, ItemKind::Issue)
        );
    }

    #[test]
    fn checks_follow_the_rollup() {
        assert_eq!(Checks::from_rollup(None), Checks::None);
        assert_eq!(Checks::from_rollup(Some("SUCCESS")), Checks::Passing);
        assert_eq!(Checks::from_rollup(Some("FAILURE")), Checks::Failing);
        assert_eq!(Checks::from_rollup(Some("ERROR")), Checks::Failing);
        assert_eq!(Checks::from_rollup(Some("PENDING")), Checks::Running);
        assert_eq!(Checks::from_rollup(Some("EXPECTED")), Checks::Running);
    }

    #[test]
    fn the_newest_open_pull_request_wins_over_a_merged_one() {
        use PullRequestState::{Merged, Open};
        let found = round_pull_request(vec![
            pull_request(9, Merged, false),
            pull_request(8, Open, true),
            pull_request(7, Open, false),
        ]);
        assert_eq!(found.map(|pr| pr.number), Some(8));
        let found = round_pull_request(vec![pull_request(9, Merged, false)]);
        assert_eq!(found.map(|pr| pr.number), Some(9));
        assert_eq!(round_pull_request(Vec::new()), None);
    }

    #[test]
    fn the_comment_says_what_is_open_and_what_comes_next() {
        let ready = pull_request(415, PullRequestState::Open, false);
        assert_eq!(
            pull_request_comment(&ready, Round::Implementation, ItemKind::Issue),
            "Pull request #415 is open.\n\n\
             **Next step:** review it.\n\
             <!-- dispatch v=1 phase=implementation outcome=pull-request by=codescope -->\n"
        );
        let draft = pull_request(415, PullRequestState::Open, true);
        assert_eq!(
            pull_request_comment(&draft, Round::Implementation, ItemKind::Issue),
            "Draft pull request #415 is open: part of it needs testing by hand.\n\n\
             **Next step:** test it as its *Test by hand* section says, then mark it ready for \
             review.\n\
             <!-- dispatch v=1 phase=implementation outcome=pull-request by=codescope -->\n"
        );
    }

    #[test]
    fn only_a_ready_open_pull_request_moves_the_item_and_only_from_in_progress() {
        use PullRequestState::{Merged, Open};
        let in_progress = Some(ColumnRole::InProgress);
        assert!(moves_to_in_review(&pull_request(1, Open, false), in_progress));
        assert!(!moves_to_in_review(&pull_request(1, Open, true), in_progress));
        assert!(!moves_to_in_review(&pull_request(1, Merged, false), in_progress));
        for role in [Some(ColumnRole::InReview), Some(ColumnRole::Ready), None] {
            assert!(!moves_to_in_review(&pull_request(1, Open, false), role));
        }
    }

    #[test]
    fn a_record_written_before_the_move_reads_as_not_in_review() {
        let json = r#"{"number":415,"url":"u","commentUrl":"c"}"#;
        let posted: PostedPullRequest = serde_json::from_str(json).unwrap();
        assert!(!posted.in_review);
        assert_eq!(posted.reported_at, None);
    }
}
