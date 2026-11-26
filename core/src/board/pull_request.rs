//! The pull request an implementation round opens, as the board poll reads it, the comment that
//! reports it on the issue, and whether a review round on it did something.

use serde::{Deserialize, Serialize};

use super::ColumnRole;
use super::dispatch::Round;
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
    pub state: PullRequestState,
    pub is_draft: bool,
    pub checks: Checks,
    /// The commit its branch points at.
    pub head_oid: String,
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

/// A pull request CodeScope reported on the issue, kept on the worktree's `projects.json` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostedPullRequest {
    pub number: u32,
    pub url: String,
    pub comment_url: String,
    /// Set once CodeScope moved the item to In review, or found it there; it never moves it
    /// again.
    #[serde(default)]
    pub in_review: bool,
}

/// The comment that reports a pull request on its issue after `round`: what is open or was
/// updated, a `**Next step:**` line and the marker.
pub fn pull_request_comment(pr: &PullRequest, round: Round) -> String {
    let (state, again) = match round {
        Round::Review => ("is updated for the review", " again"),
        Round::Research | Round::Implementation => ("is open", ""),
    };
    let number = pr.number;
    let (opened, next_step) = if pr.is_draft {
        (
            format!("Draft pull request #{number} {state}: part of it needs testing by hand."),
            "test it as its *Test by hand* section says, then mark it ready for review."
                .to_string(),
        )
    } else {
        (format!("Pull request #{number} {state}."), format!("review it{again}."))
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
            state,
            is_draft,
            checks: Checks::None,
            head_oid: "h1".into(),
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
            pull_request_comment(&ready, Round::Review),
            "Pull request #415 is updated for the review.\n\n\
             **Next step:** review it again.\n\
             <!-- dispatch v=1 phase=implementation outcome=pull-request by=codescope -->\n"
        );
        let draft = pull_request(415, PullRequestState::Open, true);
        assert_eq!(
            pull_request_comment(&draft, Round::Review),
            "Draft pull request #415 is updated for the review: part of it needs testing by \
             hand.\n\n\
             **Next step:** test it as its *Test by hand* section says, then mark it ready for \
             review.\n\
             <!-- dispatch v=1 phase=implementation outcome=pull-request by=codescope -->\n"
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
            pull_request_comment(&ready, Round::Implementation),
            "Pull request #415 is open.\n\n\
             **Next step:** review it.\n\
             <!-- dispatch v=1 phase=implementation outcome=pull-request by=codescope -->\n"
        );
        let draft = pull_request(415, PullRequestState::Open, true);
        assert_eq!(
            pull_request_comment(&draft, Round::Implementation),
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
    }
}
