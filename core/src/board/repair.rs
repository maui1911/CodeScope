//! Repairs (ADR-0027): when the pull request a dispatch opened fails its checks, conflicts with
//! its base or falls behind it, CodeScope hands a repair to the dispatch's session, at most
//! [`REPAIR_ATTEMPTS`] in a row, and only while nobody has reviewed the pull request.

use serde::{Deserialize, Serialize};

use super::ColumnRole;
use super::dispatch::{FoundSkill, HandoverSession, TASK_FILE, WorktreeDispatch};
use super::pull_request::{Checks, FailedCheck, Mergeable, PullRequest, PullRequestState};

/// How many repairs run in a row before the dispatch needs the user.
pub const REPAIR_ATTEMPTS: u32 = 2;

/// What a repair fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// It conflicts with its base, named here.
    Conflict(String),
    /// GitHub asks it to catch up with its base, named here, before it can merge.
    Behind(String),
    /// These checks failed on its last commit.
    Checks(Vec<FailedCheck>),
}

impl Fault {
    /// Without the base or the checks, as the worktree's row keeps it.
    pub fn kind(&self) -> FaultKind {
        match self {
            Fault::Conflict(_) => FaultKind::Conflict,
            Fault::Behind(_) => FaultKind::Behind,
            Fault::Checks(_) => FaultKind::Checks,
        }
    }
}

/// The kind of a [`Fault`], as the worktree's row keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum FaultKind {
    Conflict,
    Behind,
    Checks,
}

impl FaultKind {
    /// What is wrong, as the start of a card line.
    pub fn label(self) -> &'static str {
        match self {
            FaultKind::Conflict => "It conflicts with its base",
            FaultKind::Behind => "It must catch up with its base",
            FaultKind::Checks => "Checks failed",
        }
    }
}

/// The repairs on a dispatch's pull request since it was last healthy, kept on the worktree's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Repair {
    /// How many ran in a row.
    pub attempts: u32,
    /// What the last one fixes.
    pub fault: FaultKind,
    /// When the last one was handed to the session, ISO 8601.
    pub started_at: String,
    /// The pull request's head when the last one started; its push moves it on.
    pub head: String,
}

/// What to do about repairs after a read of the pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairStep {
    /// Nothing to repair, or not now.
    Nothing,
    /// The pull request is healthy again: forget the repairs.
    Clear,
    /// Hand this repair to the session; it is the `attempt`th in a row.
    Start { fault: Fault, attempt: u32 },
    /// The last repair works on it.
    Running,
    /// The last repair stopped without a push.
    StoppedWithoutPush,
    /// [`REPAIR_ATTEMPTS`] repairs in a row didn't fix this.
    GaveUp(Fault),
}

/// What keeps `pr` from merging that a repair fixes; `None` while nothing does, or GitHub is still
/// working it out. A conflict comes first, because fixing it runs the checks again.
pub fn fault(pr: &PullRequest) -> Option<Fault> {
    let health = &pr.health;
    if health.mergeable == Mergeable::Conflicts {
        return Some(Fault::Conflict(health.base.clone()));
    }
    if health.behind {
        return Some(Fault::Behind(health.base.clone()));
    }
    let finished = pr.checks == Checks::Failing && !health.checks_running;
    finished.then(|| Fault::Checks(health.failed_checks.clone()))
}

fn is_healthy(pr: &PullRequest) -> bool {
    let health = &pr.health;
    let checks_passed = matches!(pr.checks, Checks::Passing | Checks::None);
    health.mergeable == Mergeable::Yes && !health.behind && checks_passed && !health.checks_running
}

/// The next step for repairs on the pull request a dispatch opened, from its last read, the
/// repairs so far, its session and the item's column.
pub fn repair_step(
    pr: &PullRequest,
    repair: Option<&Repair>,
    session: HandoverSession,
    role: Option<ColumnRole>,
) -> RepairStep {
    let in_its_columns = matches!(role, Some(ColumnRole::InProgress | ColumnRole::InReview));
    if pr.state != PullRequestState::Open || pr.health.reviewed || !in_its_columns {
        return RepairStep::Nothing;
    }
    if repair.is_some() && is_healthy(pr) {
        return RepairStep::Clear;
    }
    if repair.is_some_and(|last| last.head == pr.head_oid) {
        return match session {
            HandoverSession::Stopped => RepairStep::StoppedWithoutPush,
            HandoverSession::Busy | HandoverSession::Idle => RepairStep::Running,
        };
    }
    let attempts = repair.map_or(0, |last| last.attempts);
    match fault(pr) {
        None => RepairStep::Nothing,
        Some(fault) if attempts >= REPAIR_ATTEMPTS => RepairStep::GaveUp(fault),
        Some(fault) if session == HandoverSession::Idle => RepairStep::Start {
            fault,
            attempt: attempts + 1,
        },
        Some(_) => RepairStep::Nothing,
    }
}

/// The first message of a repair in the session, pointing at the task file.
pub fn repair_message(number: u32) -> String {
    format!("Repair the pull request of issue #{number}: read {TASK_FILE} and follow it.")
}

/// The task file of a repair of `pr`, which `dispatch` opened; `skill` is the project's repair
/// skill when the agent will find it.
pub fn repair_task(
    dispatch: &WorktreeDispatch,
    pr: &PullRequest,
    fault: &Fault,
    skill: Option<FoundSkill<'_>>,
) -> String {
    let wrong = match fault {
        Fault::Conflict(base) => format!("It conflicts with `{base}`.\n"),
        Fault::Behind(base) => {
            format!("GitHub asks it to catch up with `{base}` before it can merge.\n")
        }
        Fault::Checks(checks) => {
            let list: String = checks
                .iter()
                .map(|check| match check.url.as_str() {
                    "" => format!("- `{}`\n", check.name),
                    url => format!("- `{}`: {url}\n", check.name),
                })
                .collect();
            format!(
                "These checks failed on its last commit:\n\n{list}\n\
                 See them with `gh pr checks {url}`, and the log of a failed run with \
                 `gh run view <run id> --log-failed`.\n",
                url = pr.url,
            )
        }
    };
    let work = match (skill, fault) {
        (Some(FoundSkill { name, path }), _) => format!("Use the skill `{name}` in `{path}`."),
        (None, Fault::Checks(_)) => {
            "Find out why each check failed and fix the cause on this branch.".to_string()
        }
        (None, Fault::Conflict(base)) => format!(
            "Merge the latest `{base}` into the branch and resolve the conflicts. Don't rebase: \
             pushing a rebase needs force."
        ),
        (None, Fault::Behind(base)) => format!(
            "Merge the latest `{base}` into the branch. Don't rebase: pushing a rebase needs \
             force."
        ),
    };
    let rules = [
        "Fix only what is wrong above. Leave review comments, and anything else the pull request \
         could do better, alone.",
        "Before you change anything, pull the pull request's branch: someone may have pushed to \
         it since.",
        "Push the fix to the pull request's branch. Don't force-push, and don't open another pull \
         request.",
        "When a conflict needs a choice about what the code should do, or the failure doesn't \
         come from this branch (the base is red too, or a test is flaky), push nothing: say why \
         in the session and stop.",
        "Post no comments on the issue or the pull request. Don't move the item on the board: \
         CodeScope does that.",
    ]
    .map(|rule| format!("- {rule}"))
    .join("\n");
    format!(
        "# Repair: #{number}\n\n\
         Issue: {issue}\n\
         Pull request: {pr_url}\n\n\
         ## What is wrong\n\n\
         {wrong}\n\
         ## Work\n\n\
         {work}\n\n\
         ## Handover\n\n\
         CodeScope picks up the result of this session. Follow these rules, also where the \
         skill says otherwise.\n\n\
         {rules}\n",
        number = dispatch.number,
        issue = dispatch.url,
        pr_url = pr.url,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::ItemKind;
    use crate::board::dispatch::Round;
    use crate::board::pull_request::{Checks, Health, Mergeable, PullRequestState};

    fn check(name: &str) -> FailedCheck {
        FailedCheck {
            name: name.into(),
            url: format!("https://github.com/maui1911/CodeScope/actions/runs/1/job/{name}"),
        }
    }

    fn healthy() -> Health {
        Health {
            base: "main".into(),
            mergeable: Mergeable::Yes,
            ..Health::default()
        }
    }

    fn pr(checks: Checks, health: Health) -> PullRequest {
        PullRequest {
            id: "PR_417".into(),
            number: 417,
            url: "https://github.com/maui1911/CodeScope/pull/417".into(),
            state: PullRequestState::Open,
            is_draft: false,
            checks,
            head_oid: "c2".into(),
            health,
            comments: Vec::new(),
        }
    }

    fn failing() -> PullRequest {
        let health = Health {
            failed_checks: vec![check("build")],
            ..healthy()
        };
        pr(Checks::Failing, health)
    }

    fn repaired(attempts: u32, head: &str) -> Repair {
        Repair {
            attempts,
            fault: FaultKind::Checks,
            started_at: "2026-11-27T11:00:00Z".into(),
            head: head.into(),
        }
    }

    const IN_PROGRESS: Option<ColumnRole> = Some(ColumnRole::InProgress);

    fn step(pr: &PullRequest, repair: Option<&Repair>, session: HandoverSession) -> RepairStep {
        repair_step(pr, repair, session, IN_PROGRESS)
    }

    #[test]
    fn a_conflict_comes_first_and_failed_checks_count_once_all_finished() {
        assert_eq!(fault(&failing()), Some(Fault::Checks(vec![check("build")])));
        let running = Health {
            checks_running: true,
            ..failing().health
        };
        assert_eq!(fault(&pr(Checks::Failing, running)), None, "another check still runs");
        let conflict = Health {
            mergeable: Mergeable::Conflicts,
            behind: true,
            ..failing().health
        };
        assert_eq!(fault(&pr(Checks::Failing, conflict)), Some(Fault::Conflict("main".into())));
        let behind = Health {
            behind: true,
            ..failing().health
        };
        assert_eq!(fault(&pr(Checks::Failing, behind)), Some(Fault::Behind("main".into())));
        assert_eq!(fault(&pr(Checks::Passing, healthy())), None);
        let unknown = Health {
            mergeable: Mergeable::Unknown,
            ..healthy()
        };
        assert_eq!(fault(&pr(Checks::Running, unknown)), None);
    }

    #[test]
    fn a_repair_starts_when_the_session_waits_and_nobody_reviewed() {
        let start = RepairStep::Start {
            fault: Fault::Checks(vec![check("build")]),
            attempt: 1,
        };
        assert_eq!(step(&failing(), None, HandoverSession::Idle), start);
        assert_eq!(
            repair_step(&failing(), None, HandoverSession::Idle, Some(ColumnRole::InReview)),
            start
        );
        assert_eq!(step(&failing(), None, HandoverSession::Busy), RepairStep::Nothing);
        let reviewed = Health {
            reviewed: true,
            ..failing().health
        };
        assert_eq!(
            step(&pr(Checks::Failing, reviewed), None, HandoverSession::Idle),
            RepairStep::Nothing,
            "a push after a review invalidates approvals"
        );
        for role in [None, Some(ColumnRole::Ready), Some(ColumnRole::Todo)] {
            assert_eq!(
                repair_step(&failing(), None, HandoverSession::Idle, role),
                RepairStep::Nothing,
                "the user moved it out of CodeScope's columns: {role:?}"
            );
        }
        let merged = PullRequest {
            state: PullRequestState::Merged,
            ..failing()
        };
        assert_eq!(step(&merged, None, HandoverSession::Idle), RepairStep::Nothing);
    }

    #[test]
    fn a_repair_runs_until_it_pushes_and_stops_short_without_a_push() {
        let started = repaired(1, "c2");
        assert_eq!(step(&failing(), Some(&started), HandoverSession::Busy), RepairStep::Running);
        assert_eq!(step(&failing(), Some(&started), HandoverSession::Idle), RepairStep::Running);
        assert_eq!(
            step(&failing(), Some(&started), HandoverSession::Stopped),
            RepairStep::StoppedWithoutPush
        );
        let rerun = pr(Checks::Passing, healthy());
        assert_eq!(
            step(&rerun, Some(&started), HandoverSession::Stopped),
            RepairStep::Clear,
            "a failed check ran again and passed, without a push"
        );
    }

    #[test]
    fn two_repairs_in_a_row_are_the_most_and_a_healthy_read_starts_the_count_again() {
        let fault = Fault::Checks(vec![check("build")]);
        assert_eq!(
            step(&failing(), Some(&repaired(1, "c1")), HandoverSession::Idle),
            RepairStep::Start {
                fault: fault.clone(),
                attempt: 2
            }
        );
        assert_eq!(
            step(&failing(), Some(&repaired(2, "c1")), HandoverSession::Busy),
            RepairStep::GaveUp(fault)
        );
        let rerun = pr(Checks::Running, healthy());
        let twice = repaired(2, "c1");
        assert_eq!(step(&rerun, Some(&twice), HandoverSession::Idle), RepairStep::Nothing);
        let fixed = pr(Checks::Passing, healthy());
        let healed = step(&fixed, Some(&repaired(1, "c1")), HandoverSession::Idle);
        assert_eq!(healed, RepairStep::Clear);
        let no_checks = pr(Checks::None, healthy());
        assert_eq!(step(&no_checks, Some(&twice), HandoverSession::Busy), RepairStep::Clear);
        assert_eq!(step(&fixed, None, HandoverSession::Idle), RepairStep::Nothing);
    }

    fn record() -> WorktreeDispatch {
        WorktreeDispatch {
            board_url: "https://github.com/users/maui1911/projects/3".into(),
            item_id: "PVTI_1".into(),
            content_id: "I_411".into(),
            kind: ItemKind::Issue,
            repository: "maui1911/CodeScope".into(),
            number: 411,
            title: "Fuzzy paths".into(),
            url: "https://github.com/maui1911/CodeScope/issues/411".into(),
            round: Round::Implementation,
            trigger_at: "2026-11-27T10:00:00Z".into(),
            agent: None,
            findings: None,
            pull_request: None,
            review: None,
            fork: None,
            repair: None,
            forwarded: None,
            kept: None,
            linked_at: None,
        }
    }

    #[test]
    fn the_repair_task_says_what_is_wrong_and_keeps_to_it() {
        let checks = Fault::Checks(vec![check("build"), check("test")]);
        let task = repair_task(&record(), &failing(), &checks, None);
        for part in [
            "# Repair: #411\n\n",
            "Issue: https://github.com/maui1911/CodeScope/issues/411\n",
            "Pull request: https://github.com/maui1911/CodeScope/pull/417\n",
            "## What is wrong\n\nThese checks failed on its last commit:\n\n\
             - `build`: https://github.com/maui1911/CodeScope/actions/runs/1/job/build\n\
             - `test`: https://github.com/maui1911/CodeScope/actions/runs/1/job/test\n",
            "`gh pr checks https://github.com/maui1911/CodeScope/pull/417`",
            "## Work\n\nFind out why each check failed and fix the cause on this branch.",
            "- Fix only what is wrong above. Leave review comments, and anything else the pull \
             request could do better, alone.",
            "- Before you change anything, pull the pull request's branch",
            "- Push the fix to the pull request's branch. Don't force-push, and don't open \
             another pull request.",
            "- When a conflict needs a choice about what the code should do, or the failure \
             doesn't come from this branch (the base is red too, or a test is flaky), push \
             nothing: say why in the session and stop.",
            "- Post no comments on the issue or the pull request. Don't move the item on the \
             board: CodeScope does that.",
        ] {
            assert!(task.contains(part), "missing {part:?} in\n{task}");
        }

        let task = repair_task(&record(), &failing(), &Fault::Conflict("main".into()), None);
        assert!(task.contains("## What is wrong\n\nIt conflicts with `main`.\n"), "{task}");
        assert!(
            task.contains("Merge the latest `main` into the branch and resolve the conflicts. \
                           Don't rebase: pushing a rebase needs force."),
            "{task}"
        );
        let task = repair_task(&record(), &failing(), &Fault::Behind("main".into()), None);
        assert!(
            task.contains("GitHub asks it to catch up with `main` before it can merge.\n"),
            "{task}"
        );
        assert!(task.contains("Merge the latest `main` into the branch."), "{task}");

        let path = ".claude/skills/fix-ci/SKILL.md";
        let skill = Some(FoundSkill { name: "fix-ci", path });
        let task = repair_task(&record(), &failing(), &checks, skill);
        assert!(
            task.contains("## Work\n\nUse the skill `fix-ci` in `.claude/skills/fix-ci/SKILL.md`."),
            "{task}"
        );
        assert_eq!(
            repair_message(411),
            "Repair the pull request of issue #411: read .codescope/dispatch.md and follow it."
        );
    }

    #[test]
    fn a_repair_is_kept_on_the_worktree_s_row() {
        let mut row = record();
        assert!(!serde_json::to_string(&row).unwrap().contains("repair"));
        row.repair = Some(repaired(1, "c1"));
        let json = serde_json::to_string(&row).unwrap();
        let kept = r#""repair":{"attempts":1,"fault":"checks","startedAt":"2026-11-27T11:00:00Z","#;
        assert!(json.contains(kept) && json.contains(r#""head":"c1"}"#), "{json}");
        assert_eq!(serde_json::from_str::<WorktreeDispatch>(&json).unwrap(), row);
        assert_eq!(FaultKind::Checks.label(), "Checks failed");
        assert_eq!(FaultKind::Conflict.label(), "It conflicts with its base");
        assert_eq!(FaultKind::Behind.label(), "It must catch up with its base");
    }
}
