//! Which board items to dispatch and what their agent gets: the rules of `docs/board-protocol.md`
//! as pure functions, and the files a dispatch writes into its worktree.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use super::{Board, BoardItem, ColumnMapping, ColumnRole, ItemKind, ItemState, RoleKind};
use super::{mapped_column, normalize_column_name};
use crate::git::{parse_worktree_porcelain, run_git};
use crate::projects::normalise_project_path;
use crate::time::parse_iso8601_secs;

/// Where the agent reads what to do, relative to the worktree.
pub const TASK_FILE: &str = ".codescope/dispatch.md";
/// Where research leaves its findings, relative to the worktree.
pub const FINDINGS_FILE: &str = ".codescope/findings.md";
/// How many of a user's dispatches run at once when `settings.json` names no limit.
pub const DEFAULT_DISPATCH_LIMIT: usize = 3;

const SLUG_LENGTH: usize = 40;
const SKILL_FOLDERS: [&str; 6] = [
    ".claude",
    ".agents",
    ".codex",
    ".gemini",
    ".opencode",
    ".github",
];
const EXCLUDED_FILES: [&str; 2] = ["/.codescope/dispatch.md", "/.codescope/findings.md"];

/// The last move of an item on one board, from its issue's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusChange {
    /// Login of who moved it; `None` for a deleted account.
    pub actor: Option<String>,
    /// Set when one of the board's workflows moved it.
    pub was_automated: bool,
    /// Name of the column it moved into.
    pub status: String,
    /// ISO 8601, as the host reports it.
    pub created_at: String,
}

/// A stretch of work in one session: research, implementation, or handling a review.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Round {
    Research,
    Implementation,
    Review,
}

impl Round {
    /// The round a move into `role` starts, given the round the item's worktree ran last.
    pub fn after_move(role: ColumnRole, last: Option<Round>) -> Option<Round> {
        match (role, last) {
            (ColumnRole::Research, _) => Some(Round::Research),
            (ColumnRole::Ready, Some(Round::Implementation | Round::Review)) => Some(Round::Review),
            (ColumnRole::Ready, _) => Some(Round::Implementation),
            _ => None,
        }
    }

    /// The column an item moves into when the round starts.
    pub fn progress_role(self) -> ColumnRole {
        match self {
            Round::Research => ColumnRole::Researching,
            Round::Implementation | Round::Review => ColumnRole::InProgress,
        }
    }

    /// The skill that runs the round.
    pub fn skill(self) -> &'static str {
        match self {
            Round::Research => "research",
            Round::Implementation => "implement",
            Round::Review => "address-review",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Round::Research => "Research",
            Round::Implementation => "Implementation",
            Round::Review => "Review",
        }
    }
}

/// The board item a worktree was dispatched for, kept on its `projects.json` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeDispatch {
    pub board_url: String,
    /// The item's id on the board.
    pub item_id: String,
    /// The issue's own id.
    pub content_id: String,
    /// `owner/name`.
    pub repository: String,
    pub number: u32,
    pub title: String,
    pub url: String,
    /// The round started last.
    pub round: Round,
    /// When the move that started that round was made, as the host reports it.
    pub trigger_at: String,
}

/// Why an item in a trigger column does not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hold {
    Closed,
    Draft,
    PullRequest,
    AssignedToOther(String),
    /// The timeline has no move into the item's current column.
    NoMove,
    MovedBeforeAdded,
    MovedByWorkflow,
    /// Moved by someone else: on an item not assigned to the user alone, or without write access.
    MovedByOther(String),
    /// Whether the mover can write to the repository couldn't be read; the next board read asks
    /// again.
    AccessUnknown(String),
    /// This move already started a round.
    Handled,
    NoProject(String),
    SeveralProjects(String),
    NoProgressColumn(ColumnRole),
    LimitReached(usize),
}

impl Hold {
    /// Whether the item is none of the user's business, so its card shows nothing.
    pub fn is_quiet(&self) -> bool {
        matches!(self, Hold::AssignedToOther(_) | Hold::Handled)
    }

    /// One line for the item's card.
    pub fn label(&self) -> String {
        match self {
            Hold::Closed => "Closed, so nothing starts".into(),
            Hold::Draft => "A draft: convert it to an issue to start".into(),
            Hold::PullRequest => "Pull request items don't start yet".into(),
            Hold::AssignedToOther(login) => format!("Assigned to @{login}"),
            Hold::NoMove => "No move on its timeline: move it again to start".into(),
            Hold::MovedBeforeAdded => {
                "Moved before the board was added: move it again to start".into()
            }
            Hold::MovedByWorkflow => "Moved by a workflow: move it yourself to start".into(),
            Hold::MovedByOther(login) => format!("Moved by @{login}: move it yourself to start"),
            Hold::AccessUnknown(login) => {
                format!("Couldn't check whether @{login} can write: retrying")
            }
            Hold::Handled => "Started".into(),
            Hold::NoProject(repository) => format!("No project has {repository} as a remote"),
            Hold::SeveralProjects(repository) => {
                format!("Several projects have {repository} as a remote")
            }
            Hold::NoProgressColumn(role) => {
                format!("Add a column {} to the board to start", role.label())
            }
            Hold::LimitReached(limit) => format!("Waits: {limit} dispatches are running"),
        }
    }
}

/// What [`decide`] knows about an item besides the board.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// The login this CodeScope works for.
    pub viewer: &'a str,
    /// The item's last move on this board.
    pub change: Option<&'a StatusChange>,
    /// Whether the mover can write to the item's repository; `None` until asked.
    pub mover_can_write: Option<bool>,
    /// When the board was added, ISO 8601; earlier moves start nothing.
    pub board_added_at: &'a str,
    /// Projects with a remote for the item's repository.
    pub projects: usize,
    /// This user's dispatch of the item, if any.
    pub dispatch: Option<&'a WorktreeDispatch>,
    /// This user's dispatches that are running now.
    pub running: usize,
    pub limit: usize,
}

/// The round a move into a trigger column starts, or why it doesn't. `None` when the item is
/// not in a trigger column.
pub fn decide(
    board: &Board,
    mapping: &ColumnMapping,
    item: &BoardItem,
    facts: &Facts,
) -> Option<Result<Round, Hold>> {
    let role = board.role_of(item, mapping)?;
    let round = Round::after_move(role, facts.dispatch.map(|d| d.round))?;
    Some(check(board, mapping, item, facts, round).map(|()| round))
}

fn check(
    board: &Board,
    mapping: &ColumnMapping,
    item: &BoardItem,
    facts: &Facts,
    round: Round,
) -> Result<(), Hold> {
    match (item.kind, item.state) {
        (_, ItemState::Closed | ItemState::Merged) => return Err(Hold::Closed),
        (ItemKind::Draft, _) => return Err(Hold::Draft),
        (ItemKind::PullRequest, _) => return Err(Hold::PullRequest),
        (ItemKind::Issue, ItemState::Open) => {}
    }
    let assigned_to_viewer = is_assigned_to(item, facts.viewer);
    if !assigned_to_viewer && let Some(other) = item.assignees.first() {
        return Err(Hold::AssignedToOther(other.clone()));
    }

    let column = item
        .column_id
        .as_deref()
        .and_then(|id| board.column(id))
        .ok_or(Hold::NoMove)?;
    let change = facts
        .change
        .filter(|c| normalize_column_name(&c.status) == normalize_column_name(&column.name))
        .ok_or(Hold::NoMove)?;
    if facts.dispatch.is_some_and(|d| d.trigger_at == change.created_at) {
        return Err(Hold::Handled);
    }
    if !is_at_or_after(&change.created_at, facts.board_added_at) {
        return Err(Hold::MovedBeforeAdded);
    }
    if change.was_automated {
        return Err(Hold::MovedByWorkflow);
    }
    let Some(mover) = change.actor.as_deref() else {
        return Err(Hold::MovedByOther("ghost".into()));
    };
    if !mover.eq_ignore_ascii_case(facts.viewer) {
        match (is_assigned_to_only(item, facts.viewer), facts.mover_can_write) {
            (true, Some(true)) => {}
            (true, None) => return Err(Hold::AccessUnknown(mover.to_string())),
            _ => return Err(Hold::MovedByOther(mover.to_string())),
        }
    }

    let repository = item.repository.clone().unwrap_or_default();
    match facts.projects {
        0 => return Err(Hold::NoProject(repository)),
        1 => {}
        _ => return Err(Hold::SeveralProjects(repository)),
    }
    if mapped_column(board, mapping, round.progress_role()).is_none() {
        return Err(Hold::NoProgressColumn(round.progress_role()));
    }
    if facts.running >= facts.limit {
        return Err(Hold::LimitReached(facts.limit));
    }
    Ok(())
}

fn is_assigned_to(item: &BoardItem, login: &str) -> bool {
    item.assignees.iter().any(|a| a.eq_ignore_ascii_case(login))
}

fn is_assigned_to_only(item: &BoardItem, login: &str) -> bool {
    item.assignees.len() == 1 && is_assigned_to(item, login)
}

fn is_at_or_after(at: &str, since: &str) -> bool {
    match (parse_iso8601_secs(at), parse_iso8601_secs(since)) {
        (Some(at), Some(since)) => at >= since,
        _ => false,
    }
}

/// Content ids of the open issues in trigger columns: the items whose moves [`decide`] needs.
pub fn trigger_content_ids(board: &Board, mapping: &ColumnMapping) -> Vec<String> {
    board
        .items
        .iter()
        .filter(|item| item.kind == ItemKind::Issue && item.state == ItemState::Open)
        .filter(|item| {
            board.role_of(item, mapping).map(ColumnRole::kind) == Some(RoleKind::Trigger)
        })
        .filter_map(|item| item.content_id.clone())
        .collect()
}

/// The login whose write access [`decide`] needs for this move, if any: someone other than the
/// viewer who moved an item assigned to the viewer alone.
pub fn mover_to_check<'a>(
    item: &BoardItem,
    change: &'a StatusChange,
    viewer: &str,
) -> Option<&'a str> {
    let mover = change.actor.as_deref()?;
    (!change.was_automated
        && !mover.eq_ignore_ascii_case(viewer)
        && is_assigned_to_only(item, viewer))
    .then_some(mover)
}

/// Whether an agent works on the item now, which counts towards the dispatch limit.
pub fn is_running(board: &Board, mapping: &ColumnMapping, item: &BoardItem) -> bool {
    matches!(
        board.role_of(item, mapping),
        Some(ColumnRole::Researching | ColumnRole::InProgress)
    )
}

/// `<number>-<slug>`: the title in lower-case words joined by `-`, at most 40 characters.
pub fn branch_name(number: u32, title: &str) -> String {
    let mut slug = String::new();
    for word in title
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        let word = word.to_ascii_lowercase();
        if slug.is_empty() {
            slug = word.chars().take(SLUG_LENGTH).collect();
        } else if slug.len() + 1 + word.len() <= SLUG_LENGTH {
            slug.push('-');
            slug.push_str(&word);
        } else {
            break;
        }
    }
    if slug.is_empty() {
        number.to_string()
    } else {
        format!("{number}-{slug}")
    }
}

/// `owner/name` of a GitHub remote URL, HTTPS or SSH; `None` for another host.
pub fn repository_from_remote(url: &str) -> Option<String> {
    let url = url.trim();
    let after_scheme = ["https://", "http://", "ssh://", "git://"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme));
    let rest = match after_scheme {
        Some(rest) => {
            let authority = rest.split('/').next().unwrap_or(rest);
            match authority.rfind('@') {
                Some(at) => &rest[at + 1..],
                None => rest,
            }
        }
        None => match url.split_once('@') {
            Some((user, host)) if !user.contains(['/', ':']) => host,
            _ => url,
        },
    };
    let path = rest
        .strip_prefix("github.com")?
        .strip_prefix(['/', ':'])?
        .trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    let valid = !owner.is_empty() && !name.is_empty() && !name.contains('/');
    valid.then(|| format!("{owner}/{name}"))
}

/// A local project and its git remotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRemotes {
    pub project_id: String,
    pub path: String,
    /// `(name, URL)` pairs.
    pub remotes: Vec<(String, String)>,
}

/// The projects with a remote for `repository`, each with the name of that remote.
pub fn projects_for_repository<'a>(
    projects: &'a [ProjectRemotes],
    repository: &str,
) -> Vec<(&'a ProjectRemotes, &'a str)> {
    projects
        .iter()
        .filter_map(|project| {
            project
                .remotes
                .iter()
                .find(|(_, url)| {
                    repository_from_remote(url)
                        .is_some_and(|r| r.eq_ignore_ascii_case(repository))
                })
                .map(|(name, _)| (project, name.as_str()))
        })
        .collect()
}

/// The first message of a dispatched session: one line without quotes, pointing at [`TASK_FILE`].
pub fn task_message(number: u32) -> String {
    format!("Work on issue #{number}: read {TASK_FILE} and follow it.")
}

/// The agent's launch line with `task` as the first message of an interactive session; `None`
/// when CodeScope doesn't know how that CLI takes one.
pub fn launch_with_task(launch: &str, task: &str) -> Option<String> {
    let program = launch.split_whitespace().next()?;
    let name = Path::new(program).file_stem()?.to_str()?.to_ascii_lowercase();
    let quoted = format!("'{}'", task.replace('\'', ""));
    match name.as_str() {
        "claude" | "codex" | "pi" => Some(format!("{launch} {quoted}")),
        "gemini" | "copilot" => Some(format!("{launch} -i {quoted}")),
        _ => None,
    }
}

/// The `SKILL.md` of skill `name`: in the worktree's repository first, then in the user's own
/// skills under `home`, in the folders of every agent that reads Agent Skills.
pub fn find_skill(worktree: &Path, home: Option<&Path>, name: &str) -> Option<PathBuf> {
    [Some(worktree), home]
        .into_iter()
        .flatten()
        .find_map(|root| {
            SKILL_FOLDERS
                .iter()
                .map(|folder| root.join(folder).join("skills").join(name).join("SKILL.md"))
                .find(|path| path.is_file())
        })
}

/// What [`TASK_FILE`] says for a round: the item, the skill or CodeScope's own prompt, and the
/// handover rules the board protocol depends on. `skill` is where the skill's `SKILL.md` is.
pub fn task_file(dispatch: &WorktreeDispatch, skill: Option<&str>) -> String {
    let WorktreeDispatch {
        number,
        repository,
        title,
        url,
        round,
        ..
    } = dispatch;
    let work = match skill {
        Some(path) => format!("Use the skill `{}` in `{path}`.", round.skill()),
        None => built_in_work(*round),
    };
    let handover = handover_rules(*round, *number, repository)
        .iter()
        .map(|rule| format!("- {rule}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "# {label}: #{number} {title}\n\n\
         Issue: {url}\n\
         Read it with its comments: `gh issue view {number} --repo {repository} --comments`\n\n\
         ## Work\n\n\
         {work}\n\n\
         ## Handover\n\n\
         CodeScope picks up the result of this session. Follow these rules, also where the \
         skill says otherwise.\n\n\
         {handover}\n",
        label = round.label(),
    )
}

fn built_in_work(round: Round) -> String {
    match round {
        Round::Research => format!(
            "Find out what the issue asks for and, for a bug, why it happens. Read the code \
             involved and name the cause (for a feature: what is missing) and an approach. Don't \
             change any code; only write `{FINDINGS_FILE}`."
        ),
        Round::Implementation => format!(
            "Implement the issue. When `{FINDINGS_FILE}` exists it holds the research, and the \
             issue's comments after it hold the answers to its decision points. Where the \
             repository has tests, write the test first. Commit in small steps."
        ),
        Round::Review => "Handle the review of the pull request for this issue: read the \
             review comments and threads and fix what they ask. Fix failing checks and merge \
             conflicts as well."
            .to_string(),
    }
}

fn handover_rules(round: Round, number: u32, repository: &str) -> Vec<String> {
    let board = "Don't move the item on the board: CodeScope does that.".to_string();
    match round {
        Round::Research => vec![
            format!(
                "Write the findings to `{FINDINGS_FILE}` under the headings `## Cause`, \
                 `## Approach` and, only when something is open, `## Decision points`."
            ),
            "Write each open choice as a heading `### B1 <question>` (B2, B3, … for the next), \
             a sentence or two of context, the options as a list starting with **A**, **B**, \
             … and a line `Advice: A, <reason>`."
                .to_string(),
            format!("Don't post the findings on the issue. {board}"),
            "Stop when the file is written.".to_string(),
        ],
        Round::Implementation => vec![
            format!(
                "Push the branch and open a pull request against {repository} with \
                 `gh pr create --assignee @me`. Its body says `Closes #{number}`."
            ),
            "When everything was tested automatically, open it ready for review. When \
             something needs testing by hand, such as how a screen looks or another operating \
             system, open a draft (`--draft`) with a section `## Test by hand` that lists the \
             steps and what to expect."
                .to_string(),
            board,
            "When you need an answer before you can go on, ask it here and stop.".to_string(),
        ],
        Round::Review => vec![
            "Push the fixes to the pull request's branch and reply to each comment you \
             handled."
                .to_string(),
            board,
            "When a comment needs a decision from a person, ask it here and stop.".to_string(),
        ],
    }
}

/// `info/exclude` with the handover files added; `None` when it lists them already.
pub fn exclude_with_handover_files(existing: &str) -> Option<String> {
    let missing: Vec<&str> = EXCLUDED_FILES
        .into_iter()
        .filter(|file| !existing.lines().any(|line| line.trim() == *file))
        .collect();
    if missing.is_empty() {
        return None;
    }
    let mut next = existing.to_string();
    if !next.is_empty() && !next.ends_with('\n') {
        next.push('\n');
    }
    for file in missing {
        next.push_str(file);
        next.push('\n');
    }
    Some(next)
}

/// Keep the handover files out of `git status` in every worktree of the repository.
pub fn exclude_handover_files(worktree: &Path) -> Result<()> {
    let output = run_git(worktree, &["rev-parse", "--git-common-dir"])?;
    let common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let common = if common.is_absolute() {
        common
    } else {
        worktree.join(common)
    };
    let path = common.join("info").join("exclude");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let Some(next) = exclude_with_handover_files(&existing) else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&path, next).with_context(|| format!("write {}", path.display()))
}

/// Write [`TASK_FILE`] into the worktree; refuses when the file or its folder is a link.
pub fn write_task_file(worktree: &Path, content: &str) -> Result<()> {
    let path = worktree.join(TASK_FILE);
    for part in [path.parent(), Some(path.as_path())].into_iter().flatten() {
        if std::fs::symlink_metadata(part).is_ok_and(|meta| meta.file_type().is_symlink()) {
            bail!("{} is a link", part.display());
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    std::fs::write(&path, content).with_context(|| format!("write {}", path.display()))
}

/// Create a dispatch's worktree at `path` on `branch`, from `remote`'s own default branch,
/// fetched first and not tracked. A worktree of `repo` already at `path` on `branch` is kept;
/// an existing local `branch` is used only when the remote's default branch contains it.
pub fn add_dispatch_worktree(repo: &Path, path: &Path, branch: &str, remote: &str) -> Result<()> {
    if path.exists() {
        if is_worktree_on(repo, path, branch) {
            return Ok(());
        }
        bail!("{} already exists", path.display());
    }
    let default_branch = remote_default_branch(repo, remote)?;
    run_git(repo, &["fetch", "--quiet", remote, &default_branch])?;
    let base = format!("refs/remotes/{remote}/{default_branch}");
    if !ref_exists(repo, &base) {
        bail!("fetching {remote} left no {remote}/{default_branch} to start from");
    }
    let local = format!("refs/heads/{branch}");
    if ref_exists(repo, &local)
        && run_git(repo, &["merge-base", "--is-ancestor", &local, &base]).is_err()
    {
        bail!(
            "the local branch {branch} has commits {remote}/{default_branch} doesn't: \
             remove or rename it, then move the item again"
        );
    }
    let path_arg = path.to_string_lossy();
    run_git(
        repo,
        &["worktree", "add", "--no-track", "-B", branch, &path_arg, &base],
    )
    .map(|_| ())
}

fn remote_default_branch(repo: &Path, remote: &str) -> Result<String> {
    let output = run_git(repo, &["ls-remote", "--symref", remote, "HEAD"])?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| {
            let name = line.strip_prefix("ref: refs/heads/")?.strip_suffix("\tHEAD")?;
            Some(name.to_string())
        })
        .with_context(|| format!("{remote} names no default branch"))
}

fn is_worktree_on(repo: &Path, path: &Path, branch: &str) -> bool {
    let Ok(output) = run_git(repo, &["worktree", "list", "--porcelain"]) else {
        return false;
    };
    let wanted = normalise_project_path(&path.to_string_lossy());
    parse_worktree_porcelain(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .any(|w| normalise_project_path(&w.path) == wanted && w.branch.as_deref() == Some(branch))
}

fn ref_exists(repo: &Path, reference: &str) -> bool {
    run_git(repo, &["rev-parse", "--verify", "--quiet", reference]).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::{Column, suggest_mapping};
    use crate::process::no_window_command;

    const VIEWER: &str = "maui1911";
    const ADDED: &str = "2026-11-25T09:00:00Z";
    const MOVED: &str = "2026-11-25T10:00:00Z";

    fn board_with(item: BoardItem) -> Board {
        let columns = [
            ("t", "Todo"),
            ("r", "Research"),
            ("rs", "Researching"),
            ("nd", "Needs decision"),
            ("y", "Ready"),
            ("p", "In progress"),
            ("v", "In review"),
            ("d", "Done"),
        ];
        Board {
            id: "PVT_1".into(),
            status_field_id: "PVTSSF_1".into(),
            title: "CodeScope".into(),
            url: "https://github.com/users/maui1911/projects/3".into(),
            closed: false,
            columns: columns
                .iter()
                .map(|(id, name)| Column {
                    id: id.to_string(),
                    name: name.to_string(),
                })
                .collect(),
            items: vec![item],
            truncated: false,
        }
    }

    fn issue_in(column: &str) -> BoardItem {
        BoardItem {
            id: "PVTI_1".into(),
            kind: ItemKind::Issue,
            number: Some(411),
            title: "Fuzzy paths".into(),
            url: Some("https://github.com/maui1911/CodeScope/issues/411".into()),
            repository: Some("maui1911/CodeScope".into()),
            content_id: Some("I_411".into()),
            assignees: Vec::new(),
            state: ItemState::Open,
            column_id: Some(column.into()),
            updated_at: MOVED.into(),
        }
    }

    fn moved_by(actor: &str, status: &str) -> StatusChange {
        StatusChange {
            actor: Some(actor.into()),
            was_automated: false,
            status: status.into(),
            created_at: MOVED.into(),
        }
    }

    fn facts<'a>(change: &'a StatusChange) -> Facts<'a> {
        Facts {
            viewer: VIEWER,
            change: Some(change),
            mover_can_write: None,
            board_added_at: ADDED,
            projects: 1,
            dispatch: None,
            running: 0,
            limit: DEFAULT_DISPATCH_LIMIT,
        }
    }

    fn dispatch(round: Round, trigger_at: &str) -> WorktreeDispatch {
        WorktreeDispatch {
            board_url: "https://github.com/users/maui1911/projects/3".into(),
            item_id: "PVTI_1".into(),
            content_id: "I_411".into(),
            repository: "maui1911/CodeScope".into(),
            number: 411,
            title: "Fuzzy paths".into(),
            url: "https://github.com/maui1911/CodeScope/issues/411".into(),
            round,
            trigger_at: trigger_at.into(),
        }
    }

    fn run_decide(item: BoardItem, facts: &Facts) -> Option<Result<Round, Hold>> {
        let board = board_with(item);
        let mapping = suggest_mapping(&board.columns);
        decide(&board, &mapping, &board.items[0], facts)
    }

    #[test]
    fn your_own_move_into_research_starts_research() {
        let change = moved_by(VIEWER, "Research");
        assert_eq!(run_decide(issue_in("r"), &facts(&change)), Some(Ok(Round::Research)));
    }

    #[test]
    fn your_own_move_into_ready_starts_implementation() {
        let change = moved_by("MAUI1911", "Ready");
        assert_eq!(run_decide(issue_in("y"), &facts(&change)), Some(Ok(Round::Implementation)));
    }

    #[test]
    fn items_outside_trigger_columns_are_not_decided() {
        let change = moved_by(VIEWER, "In progress");
        assert_eq!(run_decide(issue_in("p"), &facts(&change)), None);
        assert_eq!(run_decide(issue_in("t"), &facts(&change)), None);
    }

    #[test]
    fn ready_after_research_implements_and_after_implementation_reviews() {
        let change = moved_by(VIEWER, "Ready");
        let researched = dispatch(Round::Research, "2026-11-25T09:30:00Z");
        let implemented = dispatch(Round::Implementation, "2026-11-25T09:30:00Z");
        let after_research = Facts {
            dispatch: Some(&researched),
            ..facts(&change)
        };
        let after_implementation = Facts {
            dispatch: Some(&implemented),
            ..facts(&change)
        };
        assert_eq!(
            run_decide(issue_in("y"), &after_research),
            Some(Ok(Round::Implementation))
        );
        assert_eq!(
            run_decide(issue_in("y"), &after_implementation),
            Some(Ok(Round::Review))
        );
    }

    #[test]
    fn closed_drafts_and_pull_requests_hold() {
        let change = moved_by(VIEWER, "Ready");
        let closed = BoardItem {
            state: ItemState::Closed,
            ..issue_in("y")
        };
        let draft = BoardItem {
            kind: ItemKind::Draft,
            content_id: None,
            ..issue_in("y")
        };
        let pull = BoardItem {
            kind: ItemKind::PullRequest,
            ..issue_in("y")
        };
        assert_eq!(run_decide(closed, &facts(&change)), Some(Err(Hold::Closed)));
        assert_eq!(run_decide(draft, &facts(&change)), Some(Err(Hold::Draft)));
        assert_eq!(run_decide(pull, &facts(&change)), Some(Err(Hold::PullRequest)));
    }

    #[test]
    fn an_item_assigned_to_someone_else_is_theirs() {
        let change = moved_by(VIEWER, "Ready");
        let theirs = BoardItem {
            assignees: vec!["sanne".into()],
            ..issue_in("y")
        };
        let shared = BoardItem {
            assignees: vec!["sanne".into(), VIEWER.into()],
            ..issue_in("y")
        };
        assert_eq!(
            run_decide(theirs, &facts(&change)),
            Some(Err(Hold::AssignedToOther("sanne".into())))
        );
        assert_eq!(run_decide(shared, &facts(&change)), Some(Ok(Round::Implementation)));
    }

    #[test]
    fn a_move_must_be_into_the_current_column() {
        let elsewhere = moved_by(VIEWER, "Todo");
        assert_eq!(run_decide(issue_in("y"), &facts(&elsewhere)), Some(Err(Hold::NoMove)));
        let change = moved_by(VIEWER, "Ready");
        let without = Facts {
            change: None,
            ..facts(&change)
        };
        assert_eq!(run_decide(issue_in("y"), &without), Some(Err(Hold::NoMove)));
        let renamed = moved_by(VIEWER, "🚀 READY");
        assert_eq!(
            run_decide(issue_in("y"), &facts(&renamed)),
            Some(Ok(Round::Implementation))
        );
    }

    #[test]
    fn moves_before_the_board_was_added_start_nothing() {
        let change = StatusChange {
            created_at: "2026-11-25T08:59:59Z".into(),
            ..moved_by(VIEWER, "Ready")
        };
        assert_eq!(
            run_decide(issue_in("y"), &facts(&change)),
            Some(Err(Hold::MovedBeforeAdded))
        );
        let at_add = StatusChange {
            created_at: "2026-11-25T09:00:00.000Z".into(),
            ..moved_by(VIEWER, "Ready")
        };
        assert_eq!(
            run_decide(issue_in("y"), &facts(&at_add)),
            Some(Ok(Round::Implementation))
        );
        let change = moved_by(VIEWER, "Ready");
        let never_added = Facts {
            board_added_at: "",
            ..facts(&change)
        };
        assert_eq!(
            run_decide(issue_in("y"), &never_added),
            Some(Err(Hold::MovedBeforeAdded))
        );
    }

    #[test]
    fn workflow_moves_start_nothing() {
        let change = StatusChange {
            was_automated: true,
            ..moved_by(VIEWER, "Ready")
        };
        assert_eq!(
            run_decide(issue_in("y"), &facts(&change)),
            Some(Err(Hold::MovedByWorkflow))
        );
    }

    #[test]
    fn a_teammate_with_write_access_hands_out_work_by_assigning_and_moving() {
        let change = moved_by("lead", "Ready");
        let assigned = BoardItem {
            assignees: vec![VIEWER.into()],
            ..issue_in("y")
        };
        let unknown = facts(&change);
        let can_write = Facts {
            mover_can_write: Some(true),
            ..facts(&change)
        };
        let cannot_write = Facts {
            mover_can_write: Some(false),
            ..facts(&change)
        };
        assert_eq!(
            run_decide(assigned.clone(), &unknown),
            Some(Err(Hold::AccessUnknown("lead".into())))
        );
        assert_eq!(
            run_decide(assigned.clone(), &can_write),
            Some(Ok(Round::Implementation))
        );
        assert_eq!(
            run_decide(assigned, &cannot_write),
            Some(Err(Hold::MovedByOther("lead".into())))
        );
        let unchecked = Hold::AccessUnknown("lead".into());
        assert!(!unchecked.is_quiet());
        assert_eq!(unchecked.label(), "Couldn't check whether @lead can write: retrying");
        assert_eq!(
            run_decide(issue_in("y"), &can_write),
            Some(Err(Hold::MovedByOther("lead".into())))
        );
    }

    #[test]
    fn with_several_assignees_only_the_one_who_moved_it_starts() {
        let shared = BoardItem {
            assignees: vec![VIEWER.into(), "sanne".into()],
            ..issue_in("y")
        };
        for mover in ["sanne", "lead"] {
            let change = moved_by(mover, "Ready");
            let can_write = Facts {
                mover_can_write: Some(true),
                ..facts(&change)
            };
            assert_eq!(
                run_decide(shared.clone(), &can_write),
                Some(Err(Hold::MovedByOther(mover.into())))
            );
            assert_eq!(mover_to_check(&shared, &change, VIEWER), None);
        }
        let own = moved_by(VIEWER, "Ready");
        assert_eq!(run_decide(shared, &facts(&own)), Some(Ok(Round::Implementation)));
    }

    #[test]
    fn a_deleted_account_counts_as_someone_else() {
        let change = StatusChange {
            actor: None,
            ..moved_by(VIEWER, "Ready")
        };
        assert_eq!(
            run_decide(issue_in("y"), &facts(&change)),
            Some(Err(Hold::MovedByOther("ghost".into())))
        );
        let yours = BoardItem {
            assignees: vec![VIEWER.into()],
            ..issue_in("y")
        };
        assert_eq!(
            run_decide(yours, &facts(&change)),
            Some(Err(Hold::MovedByOther("ghost".into())))
        );
    }

    #[test]
    fn a_move_that_already_started_a_round_is_handled() {
        let change = moved_by(VIEWER, "Ready");
        let started = dispatch(Round::Implementation, MOVED);
        let after = Facts {
            dispatch: Some(&started),
            ..facts(&change)
        };
        assert_eq!(run_decide(issue_in("y"), &after), Some(Err(Hold::Handled)));
    }

    #[test]
    fn the_item_needs_exactly_one_project() {
        let change = moved_by(VIEWER, "Ready");
        let none = Facts {
            projects: 0,
            ..facts(&change)
        };
        let several = Facts {
            projects: 2,
            ..facts(&change)
        };
        assert_eq!(
            run_decide(issue_in("y"), &none),
            Some(Err(Hold::NoProject("maui1911/CodeScope".into())))
        );
        assert_eq!(
            run_decide(issue_in("y"), &several),
            Some(Err(Hold::SeveralProjects("maui1911/CodeScope".into())))
        );
    }

    #[test]
    fn a_round_needs_its_progress_column() {
        let change = moved_by(VIEWER, "Research");
        let mut board = board_with(issue_in("r"));
        board.columns.retain(|c| c.name != "Researching");
        let mapping = suggest_mapping(&board.columns);
        assert_eq!(
            decide(&board, &mapping, &board.items[0], &facts(&change)),
            Some(Err(Hold::NoProgressColumn(ColumnRole::Researching)))
        );
    }

    #[test]
    fn the_limit_holds_new_rounds() {
        let change = moved_by(VIEWER, "Ready");
        let full = Facts {
            running: 3,
            ..facts(&change)
        };
        assert_eq!(run_decide(issue_in("y"), &full), Some(Err(Hold::LimitReached(3))));
    }

    #[test]
    fn only_someone_else_moving_your_item_needs_an_access_check() {
        let assigned = BoardItem {
            assignees: vec![VIEWER.into()],
            ..issue_in("y")
        };
        let by_lead = moved_by("lead", "Ready");
        let by_you = moved_by(VIEWER, "Ready");
        let by_workflow = StatusChange {
            was_automated: true,
            ..by_lead.clone()
        };
        assert_eq!(mover_to_check(&assigned, &by_lead, VIEWER), Some("lead"));
        assert_eq!(mover_to_check(&assigned, &by_you, VIEWER), None);
        assert_eq!(mover_to_check(&assigned, &by_workflow, VIEWER), None);
        assert_eq!(mover_to_check(&issue_in("y"), &by_lead, VIEWER), None);
    }

    #[test]
    fn trigger_ids_are_the_open_issues_in_trigger_columns() {
        let mut board = board_with(issue_in("r"));
        board.items.push(BoardItem {
            id: "PVTI_2".into(),
            content_id: Some("I_2".into()),
            ..issue_in("p")
        });
        board.items.push(BoardItem {
            id: "PVTI_3".into(),
            content_id: Some("I_3".into()),
            state: ItemState::Closed,
            ..issue_in("y")
        });
        board.items.push(BoardItem {
            id: "PVTI_4".into(),
            content_id: Some("I_4".into()),
            ..issue_in("y")
        });
        let mapping = suggest_mapping(&board.columns);
        assert_eq!(trigger_content_ids(&board, &mapping), vec!["I_411", "I_4"]);
        assert!(is_running(&board, &mapping, &board.items[1]));
        assert!(!is_running(&board, &mapping, &board.items[0]));
    }

    #[test]
    fn branch_names_slug_the_title() {
        assert_eq!(
            branch_name(411, "Command palette: fuzzy match on paths"),
            "411-command-palette-fuzzy-match-on-paths"
        );
        assert_eq!(
            branch_name(7, "Make the session list remember which groups were collapsed"),
            "7-make-the-session-list-remember-which"
        );
        assert_eq!(branch_name(9, "✨"), "9");
        assert_eq!(branch_name(3, &"x".repeat(60)), format!("3-{}", "x".repeat(40)));
    }

    #[test]
    fn reads_the_repository_from_github_remotes() {
        for url in [
            "https://github.com/maui1911/CodeScope.git",
            "https://github.com/maui1911/CodeScope",
            "https://github.com/maui1911/CodeScope/",
            "https://token@github.com/maui1911/CodeScope.git",
            "https://user:token@github.com/maui1911/CodeScope.git",
            "git@github.com:maui1911/CodeScope.git",
            "ssh://git@github.com/maui1911/CodeScope.git",
        ] {
            assert_eq!(
                repository_from_remote(url).as_deref(),
                Some("maui1911/CodeScope"),
                "{url}"
            );
        }
        for url in [
            "https://gitlab.com/maui1911/CodeScope.git",
            "https://github.com/maui1911",
            "https://github.com.evil.example/maui1911/CodeScope",
            "https://github.com@evil.example/maui1911/CodeScope",
            "https://evil.example/x@github.com/maui1911/CodeScope",
            "D:/repos/CodeScope",
        ] {
            assert_eq!(repository_from_remote(url), None, "{url}");
        }
    }

    #[test]
    fn finds_the_project_through_any_remote() {
        let projects = vec![
            ProjectRemotes {
                project_id: "fork".into(),
                path: "D:/fork".into(),
                remotes: vec![
                    ("origin".into(), "https://github.com/me/CodeScope.git".into()),
                    ("upstream".into(), "git@github.com:maui1911/CodeScope.git".into()),
                ],
            },
            ProjectRemotes {
                project_id: "other".into(),
                path: "D:/other".into(),
                remotes: vec![("origin".into(), "https://github.com/me/other".into())],
            },
        ];
        let found = projects_for_repository(&projects, "MAUI1911/codescope");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0.project_id, "fork");
        assert_eq!(found[0].1, "upstream");
    }

    #[test]
    fn hands_the_task_to_each_agent_its_own_way() {
        let task = task_message(411);
        assert!(!task.contains(['\'', '"', '\n']), "{task}");
        assert_eq!(
            launch_with_task("claude --verbose", &task).unwrap(),
            format!("claude --verbose '{task}'")
        );
        assert_eq!(
            launch_with_task("C:/tools/codex.exe", "go").unwrap(),
            "C:/tools/codex.exe 'go'"
        );
        assert_eq!(launch_with_task("gemini", "go").unwrap(), "gemini -i 'go'");
        assert_eq!(launch_with_task("copilot", "it's").unwrap(), "copilot -i 'its'");
        assert_eq!(launch_with_task("opencode", "go"), None);
        assert_eq!(launch_with_task("", "go"), None);
    }

    #[test]
    fn the_task_file_names_the_skill_or_the_built_in_work_and_always_the_handover() {
        let research = task_file(
            &dispatch(Round::Research, MOVED),
            Some(".claude/skills/research/SKILL.md"),
        );
        assert!(research.starts_with("# Research: #411 Fuzzy paths\n"), "{research}");
        assert!(research.contains("gh issue view 411 --repo maui1911/CodeScope --comments"));
        assert!(
            research.contains("Use the skill `research` in `.claude/skills/research/SKILL.md`.")
        );
        assert!(research.contains(FINDINGS_FILE));
        assert!(research.contains("### B1 <question>"));

        let implementation = task_file(&dispatch(Round::Implementation, MOVED), None);
        assert!(implementation.contains("Implement the issue."), "{implementation}");
        assert!(implementation.contains("Closes #411"));
        assert!(implementation.contains("--assignee @me"));
        assert!(implementation.contains("## Test by hand"));

        let review = task_file(&dispatch(Round::Review, MOVED), None);
        assert!(review.contains("Handle the review"), "{review}");
    }

    #[test]
    fn finds_a_skill_in_the_worktree_before_the_home_folder() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("wt");
        let home = dir.path().join("home");
        let personal = home.join(".claude/skills/implement/SKILL.md");
        let repo_skill = worktree.join(".agents/skills/implement/SKILL.md");
        for file in [&personal, &repo_skill] {
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "---\nname: implement\n---\n").unwrap();
        }
        assert_eq!(find_skill(&worktree, Some(&home), "implement"), Some(repo_skill.clone()));
        std::fs::remove_file(&repo_skill).unwrap();
        assert_eq!(find_skill(&worktree, Some(&home), "implement"), Some(personal));
        assert_eq!(find_skill(&worktree, Some(&home), "research"), None);
    }

    #[test]
    fn excludes_the_handover_files_once() {
        let first = exclude_with_handover_files("# git ls-files\n*.log").unwrap();
        assert_eq!(
            first,
            "# git ls-files\n*.log\n/.codescope/dispatch.md\n/.codescope/findings.md\n"
        );
        assert_eq!(exclude_with_handover_files(&first), None);
        assert_eq!(
            exclude_with_handover_files("").unwrap(),
            "/.codescope/dispatch.md\n/.codescope/findings.md\n"
        );
    }

    fn git(cwd: &Path, args: &[&str]) {
        let output = no_window_command("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn head(cwd: &Path, reference: &str) -> String {
        let output = run_git(cwd, &["rev-parse", reference]).unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[test]
    fn a_dispatch_worktree_starts_from_the_fetched_remote_and_hides_the_handover_files() {
        if no_window_command("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git(&upstream, &["config", "user.email", "test@example.invalid"]);
        git(&upstream, &["config", "user.name", "Test"]);
        git(&upstream, &["commit", "--allow-empty", "-m", "one", "-q"]);
        git(dir.path(), &["clone", "-q", "upstream", "repo"]);
        git(&upstream, &["commit", "--allow-empty", "-m", "two", "-q"]);

        let path = dir.path().join("repo.worktrees").join("411-fuzzy-paths");
        add_dispatch_worktree(&repo, &path, "411-fuzzy-paths", "origin").unwrap();
        assert_eq!(head(&path, "HEAD"), head(&upstream, "HEAD"));
        assert!(run_git(&path, &["rev-parse", "--abbrev-ref", "@{upstream}"]).is_err());

        exclude_handover_files(&path).unwrap();
        write_task_file(&path, "# Research\n").unwrap();
        std::fs::write(path.join(FINDINGS_FILE), "## Cause\n").unwrap();
        let status = run_git(&path, &["status", "--porcelain"]).unwrap();
        assert_eq!(String::from_utf8_lossy(&status.stdout), "");

        add_dispatch_worktree(&repo, &path, "411-fuzzy-paths", "origin").unwrap();
        assert_eq!(head(&path, "HEAD"), head(&upstream, "HEAD"), "its own worktree is kept");
        git(&repo, &["worktree", "remove", "--force", &path.to_string_lossy()]);
        add_dispatch_worktree(&repo, &path, "411-fuzzy-paths", "origin").unwrap();
        assert_eq!(head(&path, "HEAD"), head(&upstream, "HEAD"));

        let foreign = dir.path().join("repo.worktrees").join("412-other");
        std::fs::create_dir_all(&foreign).unwrap();
        let err = add_dispatch_worktree(&repo, &foreign, "412-other", "origin").unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn a_dispatch_worktree_starts_from_the_remotes_own_default_branch() {
        if no_window_command("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        let repo = dir.path().join("repo");
        for folder in [&upstream, &repo] {
            std::fs::create_dir_all(folder).unwrap();
            git(folder, &["-c", "init.defaultBranch=master", "init", "-q"]);
            git(folder, &["config", "user.email", "test@example.invalid"]);
            git(folder, &["config", "user.name", "Test"]);
            git(folder, &["commit", "--allow-empty", "-m", "start", "-q"]);
        }
        git(&upstream, &["commit", "--allow-empty", "-m", "upstream", "-q"]);
        git(&repo, &["remote", "add", "upstream", &upstream.to_string_lossy()]);

        let path = dir.path().join("repo.worktrees").join("7-seven");
        add_dispatch_worktree(&repo, &path, "7-seven", "upstream").unwrap();
        assert_eq!(head(&path, "HEAD"), head(&upstream, "HEAD"));

        let gone = add_dispatch_worktree(&repo, &dir.path().join("x").join("8-x"), "8-x", "nowhere");
        assert!(gone.is_err(), "a remote that can't be read is an error, not a fallback");
    }

    #[test]
    fn a_local_branch_with_commits_of_its_own_is_not_taken_over() {
        if no_window_command("git").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let upstream = dir.path().join("upstream");
        std::fs::create_dir_all(&upstream).unwrap();
        git(&upstream, &["-c", "init.defaultBranch=main", "init", "-q"]);
        git(&upstream, &["config", "user.email", "test@example.invalid"]);
        git(&upstream, &["config", "user.name", "Test"]);
        git(&upstream, &["commit", "--allow-empty", "-m", "one", "-q"]);
        git(dir.path(), &["clone", "-q", "upstream", "repo"]);
        let repo = dir.path().join("repo");
        git(&repo, &["config", "user.email", "test@example.invalid"]);
        git(&repo, &["config", "user.name", "Test"]);
        git(&repo, &["branch", "9-behind"]);
        git(&repo, &["checkout", "-q", "-b", "10-theirs"]);
        git(&repo, &["commit", "--allow-empty", "-m", "someone else's", "-q"]);
        git(&repo, &["checkout", "-q", "main"]);

        let behind = dir.path().join("repo.worktrees").join("9-behind");
        add_dispatch_worktree(&repo, &behind, "9-behind", "origin").unwrap();
        assert_eq!(head(&behind, "HEAD"), head(&upstream, "HEAD"));

        let theirs = dir.path().join("repo.worktrees").join("10-theirs");
        let err = add_dispatch_worktree(&repo, &theirs, "10-theirs", "origin").unwrap_err();
        assert!(err.to_string().contains("10-theirs"), "{err}");
        assert!(!theirs.exists());
    }

    #[cfg(unix)]
    #[test]
    fn the_task_file_is_not_written_through_a_link() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("elsewhere");
        std::fs::create_dir_all(&target).unwrap();
        let worktree = dir.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::os::unix::fs::symlink(&target, worktree.join(".codescope")).unwrap();
        assert!(write_task_file(&worktree, "# Research\n").is_err());
        assert!(!target.join("dispatch.md").exists());
    }
}
