//! Whose turn a dispatched item is (ADR-0027), worked out from what GitHub shows everyone on the
//! board, so it reads the same on every teammate's screen.

use super::pull_request::Checks;
use super::{BoardItem, ColumnRole, ItemState};

/// What the turn of an item in *In progress* or *In review* reads of its open pull request: the
/// item itself, or the newest open one that closes its issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenPullRequest {
    pub number: u32,
    pub is_draft: bool,
    pub checks: Checks,
    /// Set when it conflicts with its base branch.
    pub conflicting: bool,
    /// The reviewers asked and not heard from yet: logins, and `org/team` for a team.
    pub requested: Vec<String>,
    /// When its last commit was made, ISO 8601.
    pub head_at: Option<String>,
    /// When someone other than its author last submitted a review, ISO 8601.
    pub reviewed_at: Option<String>,
}

/// Who a dispatched item waits on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    /// The agent working for the item's assignees.
    Agent,
    /// The checks on its pull request, while they run.
    Ci,
    /// The item's assignees.
    Assignees,
    /// The reviewers asked and not heard from yet; empty when nobody was asked by name.
    Reviewers(Vec<String>),
    Nobody,
}

/// A turn and when it began, ISO 8601; `None` when nothing on GitHub says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnSince {
    pub turn: Turn,
    pub since: Option<String>,
}

/// The turn of `item` in a column with `role`, with `pull_request` its open pull request; `None`
/// outside the progress columns, where nothing is dispatched.
pub fn turn(
    item: &BoardItem,
    role: Option<ColumnRole>,
    pull_request: Option<&OpenPullRequest>,
) -> Option<TurnSince> {
    let moved_at = item.column_set_at.as_deref();
    let at = |turn: Turn, event: Option<&str>| TurnSince {
        turn,
        since: later(moved_at, event).map(str::to_string),
    };
    let role = role?;
    let progress = matches!(
        role,
        ColumnRole::Researching
            | ColumnRole::NeedsDecision
            | ColumnRole::InProgress
            | ColumnRole::InReview
    );
    if !progress {
        return None;
    }
    if item.state != ItemState::Open {
        return Some(TurnSince {
            turn: Turn::Nobody,
            since: None,
        });
    }
    Some(match (role, pull_request) {
        (ColumnRole::Researching, _) | (ColumnRole::InProgress, None) => at(Turn::Agent, None),
        (ColumnRole::InProgress | ColumnRole::InReview, Some(pr)) => {
            let head_at = pr.head_at.as_deref();
            let reviewed_at = pr.reviewed_at.as_deref();
            if pr.checks == Checks::Running {
                at(Turn::Ci, head_at)
            } else if pr.checks == Checks::Failing || pr.conflicting {
                match reviewed_at {
                    None => at(Turn::Agent, head_at),
                    Some(_) => at(Turn::Assignees, later(head_at, reviewed_at)),
                }
            } else if pr.is_draft {
                at(Turn::Assignees, head_at)
            } else if !pr.requested.is_empty() {
                at(Turn::Reviewers(pr.requested.clone()), None)
            } else if reviewed_at.is_some() {
                at(Turn::Assignees, reviewed_at)
            } else {
                at(Turn::Reviewers(Vec::new()), None)
            }
        }
        _ => at(Turn::Assignees, None),
    })
}

/// Whether the turn of `item` in a column with `role` depends on its open pull request, which a
/// board read then reads for it.
pub fn reads_pull_request(item: &BoardItem, role: Option<ColumnRole>) -> bool {
    item.state == ItemState::Open
        && item.content_id.is_some()
        && matches!(role, Some(ColumnRole::InProgress | ColumnRole::InReview))
}

fn later<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::ItemKind;

    const MOVED: &str = "2026-11-27T10:00:00Z";
    const PUSHED: &str = "2026-11-27T11:00:00Z";
    const REVIEWED: &str = "2026-11-27T12:00:00Z";

    fn item(state: ItemState) -> BoardItem {
        BoardItem {
            id: "PVTI_1".into(),
            kind: ItemKind::Issue,
            number: Some(7),
            title: "Fix it".into(),
            url: None,
            repository: Some("o/r".into()),
            content_id: Some("I_7".into()),
            assignees: vec!["maui1911".into()],
            state,
            column_id: Some("col".into()),
            column_set_at: Some(MOVED.into()),
            updated_at: MOVED.into(),
        }
    }

    fn pull_request() -> OpenPullRequest {
        OpenPullRequest {
            number: 8,
            is_draft: false,
            checks: Checks::Passing,
            conflicting: false,
            requested: Vec::new(),
            head_at: Some(PUSHED.into()),
            reviewed_at: None,
        }
    }

    fn of(role: ColumnRole, pr: Option<OpenPullRequest>) -> Option<(Turn, Option<String>)> {
        turn(&item(ItemState::Open), Some(role), pr.as_ref()).map(|t| (t.turn, t.since))
    }

    fn since(turn: Turn, at: &str) -> Option<(Turn, Option<String>)> {
        Some((turn, Some(at.to_string())))
    }

    #[test]
    fn outside_the_progress_columns_nothing_has_a_turn() {
        for role in [ColumnRole::Todo, ColumnRole::Research, ColumnRole::Ready, ColumnRole::Done] {
            assert_eq!(of(role, Some(pull_request())), None, "{role:?}");
        }
        assert_eq!(turn(&item(ItemState::Open), None, None), None);
    }

    #[test]
    fn research_is_the_agents_and_a_decision_the_assignees_from_the_move_on() {
        assert_eq!(of(ColumnRole::Researching, None), since(Turn::Agent, MOVED));
        assert_eq!(of(ColumnRole::NeedsDecision, None), since(Turn::Assignees, MOVED));
        assert_eq!(of(ColumnRole::InProgress, None), since(Turn::Agent, MOVED));
    }

    #[test]
    fn a_pull_request_waits_on_ci_then_the_agent_or_the_assignees_then_the_reviewers() {
        let running = OpenPullRequest {
            checks: Checks::Running,
            ..pull_request()
        };
        assert_eq!(of(ColumnRole::InReview, Some(running)), since(Turn::Ci, PUSHED));
        let draft = OpenPullRequest {
            is_draft: true,
            ..pull_request()
        };
        assert_eq!(of(ColumnRole::InProgress, Some(draft)), since(Turn::Assignees, PUSHED));
        let asked = OpenPullRequest {
            requested: vec!["sanne".into(), "acme/web".into()],
            ..pull_request()
        };
        assert_eq!(
            of(ColumnRole::InReview, Some(asked)),
            since(Turn::Reviewers(vec!["sanne".into(), "acme/web".into()]), MOVED)
        );
        assert_eq!(
            of(ColumnRole::InReview, Some(pull_request())),
            since(Turn::Reviewers(Vec::new()), MOVED)
        );
        let reviewed = OpenPullRequest {
            reviewed_at: Some(REVIEWED.into()),
            ..pull_request()
        };
        assert_eq!(of(ColumnRole::InReview, Some(reviewed)), since(Turn::Assignees, REVIEWED));
    }

    #[test]
    fn a_red_or_conflicting_pull_request_is_the_agents_until_someone_reviewed_it() {
        let red = OpenPullRequest {
            checks: Checks::Failing,
            ..pull_request()
        };
        assert_eq!(of(ColumnRole::InReview, Some(red.clone())), since(Turn::Agent, PUSHED));
        let conflicting = OpenPullRequest {
            conflicting: true,
            is_draft: true,
            ..pull_request()
        };
        assert_eq!(of(ColumnRole::InProgress, Some(conflicting)), since(Turn::Agent, PUSHED));
        let reviewed = OpenPullRequest {
            reviewed_at: Some(REVIEWED.into()),
            requested: vec!["sanne".into()],
            ..red
        };
        assert_eq!(of(ColumnRole::InReview, Some(reviewed)), since(Turn::Assignees, REVIEWED));
    }

    #[test]
    fn a_turn_never_starts_before_the_move_into_its_column() {
        let old = OpenPullRequest {
            checks: Checks::Running,
            head_at: Some("2026-11-26T09:00:00Z".into()),
            ..pull_request()
        };
        assert_eq!(of(ColumnRole::InReview, Some(old)), since(Turn::Ci, MOVED));
    }

    #[test]
    fn only_open_items_in_progress_or_in_review_read_their_pull_request() {
        let open = item(ItemState::Open);
        assert!(reads_pull_request(&open, Some(ColumnRole::InProgress)));
        assert!(reads_pull_request(&open, Some(ColumnRole::InReview)));
        assert!(!reads_pull_request(&open, Some(ColumnRole::Researching)));
        assert!(!reads_pull_request(&open, None));
        assert!(!reads_pull_request(&item(ItemState::Closed), Some(ColumnRole::InReview)));
        let draft = BoardItem {
            content_id: None,
            ..open
        };
        assert!(!reads_pull_request(&draft, Some(ColumnRole::InReview)));
    }

    #[test]
    fn a_closed_item_in_a_progress_column_waits_on_nobody() {
        let closed = turn(&item(ItemState::Merged), Some(ColumnRole::InReview), None);
        assert_eq!(
            closed,
            Some(TurnSince {
                turn: Turn::Nobody,
                since: None,
            })
        );
        let merged_pr_issue = of(ColumnRole::InReview, None);
        assert_eq!(merged_pr_issue, since(Turn::Assignees, MOVED));
    }
}
