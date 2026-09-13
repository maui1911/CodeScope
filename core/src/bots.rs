//! Pure-data reader for the bot control plane, for the Bots inbox.
//!
//! `labs/agent-bots` keeps its live state in a control plane directory
//! (`labs/agent-bots/.state/`): task files under `tasks/`, an
//! append-only event log in `board.md`, and one handoff per finished
//! run under `handoffs/`. This module turns that directory into a flat
//! list of [`InboxItem`]s — the pure-data half of the read-only inbox
//! described in `labs/agent-bots/README.md` §7.4 ("Stage 2 — the
//! inbox"). The panel that renders it lives on the gpui side.
//!
//! The module never writes. The runner is the single writer of every
//! file in the control plane (README §3.2); this side only reads, and
//! the app decides when to call [`load_inbox`] again.
//!
//! The formats follow what the runner writes, not a separate schema:
//! [`frontmatter_field`] matches `live_task_field` in
//! `labs/agent-bots/run/live-task.sh` line for line.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// The control plane under a project checkout:
/// `<project_root>/labs/agent-bots/.state` when that is a directory,
/// else `None`.
pub fn lab_control_plane(project_root: &Path) -> Option<PathBuf> {
    let plane = project_root.join("labs").join("agent-bots").join(".state");
    plane.is_dir().then_some(plane)
}

/// Read `key` from a document's frontmatter, the way the runner's
/// `live_task_field` does: only lines after the first `---` line and
/// before the next one count, the first line starting with `key:` wins,
/// and its value is trimmed. A `key:` line in the body is never read,
/// and `base` does not match a `base_sha:` line. `None` when absent.
pub fn frontmatter_field(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    let mut fences = 0;
    for line in text.lines() {
        if is_fence(line) {
            fences += 1;
            if fences >= 2 {
                break;
            }
            continue;
        }
        if fences == 1 && line.starts_with(&prefix) {
            return Some(line[prefix.len()..].trim().to_string());
        }
    }
    None
}

/// `^---[[:space:]]*$`, the fence the runner counts.
fn is_fence(line: &str) -> bool {
    line.strip_prefix("---").is_some_and(|rest| rest.trim().is_empty())
}

/// A task's or handoff's `status:`. The vocabulary is
/// `labs/agent-bots/contract/templates/TASK.md`'s; anything else is kept
/// verbatim in [`TaskStatus::Other`] rather than guessed at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Todo,
    Dispatched,
    Blocked,
    NeedsReview,
    Done,
    /// Unrecognised or empty — the raw value.
    Other(String),
}

impl TaskStatus {
    /// Parse a status value as the runner writes it (`needs-review`,
    /// not `NeedsReview`). Unrecognised input, empty included, is
    /// [`TaskStatus::Other`].
    pub fn parse(s: &str) -> TaskStatus {
        match s {
            "todo" => TaskStatus::Todo,
            "dispatched" => TaskStatus::Dispatched,
            "blocked" => TaskStatus::Blocked,
            "needs-review" => TaskStatus::NeedsReview,
            "done" => TaskStatus::Done,
            other => TaskStatus::Other(other.to_string()),
        }
    }

    /// Whether a human has to act: `Blocked` and `NeedsReview` only.
    pub fn needs_attention(&self) -> bool {
        matches!(self, TaskStatus::Blocked | TaskStatus::NeedsReview)
    }
}

/// One row of `board.md`'s `| when | task | event | ref |` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardEvent {
    /// `when` — a UTC timestamp, `2026-09-12T18:21:57Z`.
    pub at: String,
    pub task: String,
    pub event: String,
    /// `ref` — everything after the third separator, ` | ` included.
    pub detail: String,
}

/// Parse the board's event rows, in file order. A row is a line
/// starting with `| ` with at least four cells; the prose above the
/// table, the header row and the separator row are skipped. The fourth
/// cell runs to the final ` |`, so a detail containing ` | ` is kept
/// whole.
pub fn parse_board(text: &str) -> Vec<BoardEvent> {
    text.lines().filter_map(parse_board_row).collect()
}

fn parse_board_row(line: &str) -> Option<BoardEvent> {
    let rest = line.strip_prefix("| ")?;
    let (at, rest) = rest.split_once(" | ")?;
    let (task, rest) = rest.split_once(" | ")?;
    let (event, rest) = rest.split_once(" | ")?;
    let rest = rest.trim_end();
    let detail = rest.strip_suffix(" |").or_else(|| rest.strip_suffix('|'))?;
    let cells = [at.trim(), task.trim(), event.trim(), detail.trim()];
    if cells == ["when", "task", "event", "ref"] {
        return None;
    }
    if cells.iter().all(|c| !c.is_empty() && c.chars().all(|ch| ch == '-')) {
        return None;
    }
    let [at, task, event, detail] = cells.map(str::to_string);
    Some(BoardEvent { at, task, event, detail })
}

/// A parsed `handoffs/*.md` file. `# Evidence` and `# Artifact` are left
/// to the renderer; only what the inbox list needs is lifted out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handoff {
    pub path: PathBuf,
    pub task: String,
    pub from: String,
    pub at: String,
    pub status: TaskStatus,
    /// Trimmed `# Blockers` section, multi-line kept. The runner writes
    /// the literal `none` when there are none.
    pub blockers: String,
    /// Trimmed `# Next action` section, multi-line kept.
    pub next_action: String,
}

/// Parse a handoff. `task`, `from`, `at` and `status` come from the
/// frontmatter (`None` when `task:` is missing); `blockers` and
/// `next_action` are the trimmed text of their `# ` sections, up to the
/// next `# ` heading or the end of the file. A missing section is empty.
pub fn parse_handoff(path: &Path, text: &str) -> Option<Handoff> {
    let task = frontmatter_field(text, "task")?;
    let field = |key| frontmatter_field(text, key).unwrap_or_default();
    Some(Handoff {
        path: path.to_path_buf(),
        task,
        from: field("from"),
        at: field("at"),
        status: TaskStatus::parse(&field("status")),
        blockers: section(text, "Blockers"),
        next_action: section(text, "Next action"),
    })
}

/// The trimmed body of the `# <heading>` section after the frontmatter.
fn section(text: &str, heading: &str) -> String {
    let mut fences = 0;
    let mut body: Option<Vec<&str>> = None;
    for line in text.lines() {
        if fences < 2 {
            if is_fence(line) {
                fences += 1;
            }
            continue;
        }
        if let Some(title) = line.strip_prefix("# ") {
            if body.is_some() {
                break;
            }
            if title.trim() == heading {
                body = Some(Vec::new());
            }
            continue;
        }
        if let Some(lines) = body.as_mut() {
            lines.push(line);
        }
    }
    body.map(|lines| lines.join("\n").trim().to_string()).unwrap_or_default()
}

/// One task in the inbox: its live task file, the last board event for
/// it and its newest handoff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxItem {
    pub id: String,
    pub title: String,
    pub owner: String,
    pub status: TaskStatus,
    pub branch: Option<String>,
    /// Recorded by the runner at dispatch.
    pub worktree: Option<String>,
    /// Recorded by the runner at dispatch.
    pub base_sha: Option<String>,
    pub last_event: Option<BoardEvent>,
    pub handoff: Option<Handoff>,
}

/// Build the inbox from a control plane directory (see
/// [`lab_control_plane`]). Never writes.
///
/// One item per `tasks/*.md` with an `id:`. `last_event` is the last
/// `board.md` row for that id; `handoff` is the newest
/// `handoffs/*__<id>.md` by file name (names start with a sortable UTC
/// timestamp). A missing `tasks/`, `board.md` or `handoffs/` is empty,
/// not an error, and an unreadable individual file is skipped.
///
/// Sorted: items that need attention first, then newest
/// `last_event.at` first with items without an event after those with
/// one (as `overview::sort_rows` orders), then by id.
pub fn load_inbox(plane: &Path) -> io::Result<Vec<InboxItem>> {
    let task_paths = match md_files(&plane.join("tasks")) {
        Ok(paths) => paths,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let board = std::fs::read_to_string(plane.join("board.md")).unwrap_or_default();
    let mut last_events: HashMap<String, BoardEvent> = HashMap::new();
    for event in parse_board(&board) {
        last_events.insert(event.task.clone(), event);
    }

    // Newest first, so the first handoff that parses for an id wins.
    let mut handoff_paths = md_files(&plane.join("handoffs")).unwrap_or_default();
    handoff_paths.sort_by(|a, b| b.file_name().cmp(&a.file_name()));

    let mut items: Vec<InboxItem> = task_paths
        .iter()
        .filter_map(|path| {
            let text = std::fs::read_to_string(path).ok()?;
            let id = frontmatter_field(&text, "id")?;
            let suffix = format!("__{id}.md");
            let handoff = handoff_paths
                .iter()
                .filter(|p| {
                    p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.ends_with(&suffix))
                })
                .find_map(|p| parse_handoff(p, &std::fs::read_to_string(p).ok()?));
            let status = frontmatter_field(&text, "status").unwrap_or_default();
            Some(InboxItem {
                title: frontmatter_field(&text, "title").unwrap_or_default(),
                owner: frontmatter_field(&text, "owner").unwrap_or_default(),
                status: TaskStatus::parse(&status),
                branch: frontmatter_field(&text, "branch"),
                worktree: frontmatter_field(&text, "worktree"),
                base_sha: frontmatter_field(&text, "base_sha"),
                last_event: last_events.get(&id).cloned(),
                handoff,
                id,
            })
        })
        .collect();
    sort_items(&mut items);
    Ok(items)
}

/// The `*.md` files directly inside `dir`, unordered. Entries that cannot
/// be listed are skipped.
fn md_files(dir: &Path) -> io::Result<Vec<PathBuf>> {
    Ok(std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|ext| ext == "md"))
        .collect())
}

fn sort_items(items: &mut [InboxItem]) {
    items.sort_by(|a, b| {
        let attention = b.status.needs_attention().cmp(&a.status.needs_attention());
        // Timestamps are fixed-width UTC, so string order is time order.
        let ka = a.last_event.as_ref().map(|e| e.at.as_str());
        let kb = b.last_event.as_ref().map(|e| e.at.as_str());
        let recency = match (ka, kb) {
            (Some(ka), Some(kb)) => kb.cmp(ka),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        attention.then(recency).then_with(|| a.id.cmp(&b.id))
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Captured verbatim from `labs/agent-bots/.state/` (long paths elided
    // with `…`). Edge cases are small edits to these, not new text.

    const BOARD: &str = r#"# Board

Append-only event log. Never edit a line that is already here -
task status lives on the task file, this is the audit trail.

| when | task | event | ref |
|------|------|-------|-----|
| 2026-09-11T10:26:48Z | T-0001 | dispatched | bot/fixer/T-0001 @ 87f72cbf60f0 |
| 2026-09-11T10:26:48Z | T-0001 | agent-skipped | - |
| 2026-09-12T18:17:58Z | T-0010 | dispatched | bot/fixer/T-0010 @ 7d4e61d9894a |
| 2026-09-12T18:21:50Z | T-0010 | agent-ran | exit 0 |
| 2026-09-12T18:21:57Z | T-0010 | verified | exit 0 |
| 2026-09-12T18:21:57Z | T-0010 | pushed | bot/fixer/T-0010 @ 4760f768dc67 |
| 2026-09-12T18:21:57Z | T-0010 | handoff | done |
| 2026-09-12T18:21:58Z | T-0010 | cleaned | - |
"#;

    const TASK_T0011: &str = r#"---
id: T-0011
title: Drop a silent Busy session to Idle after a quiet window
owner: fixer
status: done
base: labs/agent-bots
branch: bot/fixer/T-0011
touches: core/src/agents/claude/telemetry.rs
verify: grep -q 'BUSY_QUIET_TIMEOUT' core/src/agents/claude/telemetry.rs && cargo test -p codescope-core --lib agents::claude::telemetry
schedule: auto
worktree: C:/dev/codescope-public.worktrees/bot-fixer-T-0011
base_sha: 9dcb4000e9fe7f43112c59a2c1a7464e5fd642eb
origin_repo: C:/dev/codescope-public
---

# Objective

A Claude Code session that `ClaudeTranscriptTail` reports as
"#;

    const HANDOFF_T0011_NAME: &str = "2026-09-13T13-06-01Z_fixer__to__human__T-0011.md";
    const HANDOFF_T0011: &str = r#"---
task: T-0011
produces: commit
from: fixer
to: human
at: 2026-09-13T13:06:01Z
status: done
---

# Objective

Drop a silent Busy session to Idle after a quiet window

# Artifact

    branch:   bot/fixer/T-0011 (pushed into C:/dev/codescope-public)
    surface:  C:/dev/codescope-public.worktrees/bot-fixer-T-0011

# Evidence

    base:     9dcb4000e9fe7f43112c59a2c1a7464e5fd642eb
    head:     8a9268eaafd121ecba8c293f46a8c7f05e16316d
    commits:  1

# Status

done

# Blockers

none

# Next action

Human reviews 'git -C C:/dev/codescope-public log --patch labs/agent-bots..bot/fixer/T-0011', then opens a PR. C:/dev/codescope-public.worktrees/bot-fixer-T-0011 is a throwaway clone and can go.
"#;

    const HANDOFF_T0901_NAME: &str = "2026-09-13T10-02-06Z_fixer__to__human__T-0901.md";
    const HANDOFF_T0901: &str = r#"---
task: T-0901
produces: commit
from: fixer
to: human
at: 2026-09-13T10:02:06Z
status: blocked
---

# Objective

A verifier that reaches for the approval gate

# Status

blocked

# Blockers

verifier exited 1:
bot-approve: refusing to approve from inside a bot run.
The point of an approval is that something other than the loop decided.
Full output: /c/dev/codescope-public/labs/agent-bots/.state/runs/2026-09-13/T-0901-….log

# Next action

Human triages the blocker above. Task stays open; worktree kept at C:/dev/codescope-public.worktrees/bot-fixer-T-0901.
"#;

    /// The captured T-0011 task with its id and status swapped.
    fn task_as(id: &str, status: &str) -> String {
        TASK_T0011
            .replace("id: T-0011", &format!("id: {id}"))
            .replace("status: done", &format!("status: {status}"))
    }

    fn put(dir: &Path, name: &str, text: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join(name), text).unwrap();
    }

    #[test]
    fn lab_control_plane_finds_state_directory() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(lab_control_plane(root.path()), None);
        let state = root.path().join("labs").join("agent-bots").join(".state");
        fs::create_dir_all(&state).unwrap();
        assert_eq!(lab_control_plane(root.path()), Some(state));
    }

    #[test]
    fn lab_control_plane_ignores_a_state_file() {
        let root = tempfile::tempdir().unwrap();
        put(&root.path().join("labs").join("agent-bots"), ".state", "not a dir");
        assert_eq!(lab_control_plane(root.path()), None);
    }

    #[test]
    fn frontmatter_field_reads_captured_task() {
        assert_eq!(frontmatter_field(TASK_T0011, "id").as_deref(), Some("T-0011"));
        assert_eq!(
            frontmatter_field(TASK_T0011, "title").as_deref(),
            Some("Drop a silent Busy session to Idle after a quiet window")
        );
        assert_eq!(
            frontmatter_field(TASK_T0011, "verify").as_deref(),
            Some(
                "grep -q 'BUSY_QUIET_TIMEOUT' core/src/agents/claude/telemetry.rs && \
                 cargo test -p codescope-core --lib agents::claude::telemetry"
            )
        );
        assert_eq!(frontmatter_field(TASK_T0011, "missing"), None);
    }

    #[test]
    fn frontmatter_field_never_reads_the_body() {
        let text = task_as("T-0011", "blocked").replace(
            "# Objective\n",
            "# Objective\n\nstatus: done\nowner_note: body\n",
        );
        assert_eq!(frontmatter_field(&text, "status").as_deref(), Some("blocked"));
        assert_eq!(frontmatter_field(&text, "owner_note"), None);
    }

    #[test]
    fn frontmatter_field_prefix_key_does_not_match_longer_key() {
        let text = TASK_T0011.replace("base: labs/agent-bots\n", "");
        assert_eq!(frontmatter_field(&text, "base"), None);
        assert_eq!(
            frontmatter_field(TASK_T0011, "base").as_deref(),
            Some("labs/agent-bots")
        );
    }

    #[test]
    fn frontmatter_field_first_line_wins_and_value_is_trimmed() {
        let text = TASK_T0011
            .replace("owner: fixer\n", "owner:   fixer  \nowner: reviewer\n");
        assert_eq!(frontmatter_field(&text, "owner").as_deref(), Some("fixer"));
    }

    #[test]
    fn frontmatter_field_without_frontmatter_is_none() {
        let body = TASK_T0011.replace("---\n", "");
        assert_eq!(frontmatter_field(&body, "id"), None);
    }

    #[test]
    fn task_status_parses_vocabulary() {
        assert_eq!(TaskStatus::parse("todo"), TaskStatus::Todo);
        assert_eq!(TaskStatus::parse("dispatched"), TaskStatus::Dispatched);
        assert_eq!(TaskStatus::parse("blocked"), TaskStatus::Blocked);
        assert_eq!(TaskStatus::parse("needs-review"), TaskStatus::NeedsReview);
        assert_eq!(TaskStatus::parse("done"), TaskStatus::Done);
    }

    #[test]
    fn task_status_unrecognised_is_other() {
        assert_eq!(TaskStatus::parse(""), TaskStatus::Other(String::new()));
        assert_eq!(
            TaskStatus::parse("NeedsReview"),
            TaskStatus::Other("NeedsReview".into())
        );
    }

    #[test]
    fn needs_attention_is_blocked_and_needs_review_only() {
        assert!(TaskStatus::Blocked.needs_attention());
        assert!(TaskStatus::NeedsReview.needs_attention());
        assert!(!TaskStatus::Todo.needs_attention());
        assert!(!TaskStatus::Dispatched.needs_attention());
        assert!(!TaskStatus::Done.needs_attention());
        assert!(!TaskStatus::Other("blocked ".into()).needs_attention());
    }

    #[test]
    fn parse_board_reads_captured_rows_in_order() {
        let events = parse_board(BOARD);
        assert_eq!(events.len(), 8);
        assert_eq!(
            events[0],
            BoardEvent {
                at: "2026-09-11T10:26:48Z".into(),
                task: "T-0001".into(),
                event: "dispatched".into(),
                detail: "bot/fixer/T-0001 @ 87f72cbf60f0".into(),
            }
        );
        assert_eq!(events[1].detail, "-");
        assert_eq!(events[7].event, "cleaned");
        assert!(events.iter().all(|e| e.at != "when"));
    }

    #[test]
    fn parse_board_keeps_a_detail_containing_a_separator() {
        let board = BOARD.replace("| handoff | done |", "| handoff | blocked | see log |");
        let events = parse_board(&board);
        assert_eq!(events.len(), 8);
        assert_eq!(events[6].detail, "blocked | see log");
    }

    #[test]
    fn parse_board_skips_other_separators_and_short_rows() {
        let board = BOARD
            .replace("|------|------|-------|-----|", "| --- | --- | --- | --- |")
            .replace("| 2026-09-11T10:26:48Z | T-0001 | agent-skipped | - |", "| x | y | z |");
        let events = parse_board(&board);
        assert_eq!(events.len(), 7);
        assert_eq!(events[1].task, "T-0010");
    }

    #[test]
    fn parse_handoff_reads_captured_done_handoff() {
        let path = Path::new("handoffs").join(HANDOFF_T0011_NAME);
        let handoff = parse_handoff(&path, HANDOFF_T0011).unwrap();
        assert_eq!(handoff.path, path);
        assert_eq!(handoff.task, "T-0011");
        assert_eq!(handoff.from, "fixer");
        assert_eq!(handoff.at, "2026-09-13T13:06:01Z");
        assert_eq!(handoff.status, TaskStatus::Done);
        assert_eq!(handoff.blockers, "none");
        assert!(handoff.next_action.starts_with("Human reviews 'git -C"));
        assert!(handoff.next_action.ends_with("is a throwaway clone and can go."));
    }

    #[test]
    fn parse_handoff_keeps_a_multi_line_blocker() {
        let handoff = parse_handoff(Path::new(HANDOFF_T0901_NAME), HANDOFF_T0901).unwrap();
        assert_eq!(handoff.status, TaskStatus::Blocked);
        assert_eq!(
            handoff.blockers,
            "verifier exited 1:\n\
             bot-approve: refusing to approve from inside a bot run.\n\
             The point of an approval is that something other than the loop decided.\n\
             Full output: /c/dev/codescope-public/labs/agent-bots/.state/runs/2026-09-13/T-0901-….log"
        );
        assert_eq!(
            handoff.next_action,
            "Human triages the blocker above. Task stays open; worktree kept at \
             C:/dev/codescope-public.worktrees/bot-fixer-T-0901."
        );
    }

    #[test]
    fn parse_handoff_without_task_is_none() {
        let text = HANDOFF_T0011.replace("task: T-0011\n", "");
        assert_eq!(parse_handoff(Path::new(HANDOFF_T0011_NAME), &text), None);
    }

    #[test]
    fn load_inbox_builds_item_from_captured_plane() {
        let plane = tempfile::tempdir().unwrap();
        put(&plane.path().join("tasks"), "T-0011.md", TASK_T0011);
        put(plane.path(), "board.md", &BOARD.replace("T-0010", "T-0011"));
        put(&plane.path().join("handoffs"), HANDOFF_T0011_NAME, HANDOFF_T0011);

        let items = load_inbox(plane.path()).unwrap();
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.id, "T-0011");
        assert_eq!(item.title, "Drop a silent Busy session to Idle after a quiet window");
        assert_eq!(item.owner, "fixer");
        assert_eq!(item.status, TaskStatus::Done);
        assert_eq!(item.branch.as_deref(), Some("bot/fixer/T-0011"));
        assert_eq!(
            item.worktree.as_deref(),
            Some("C:/dev/codescope-public.worktrees/bot-fixer-T-0011")
        );
        assert_eq!(
            item.base_sha.as_deref(),
            Some("9dcb4000e9fe7f43112c59a2c1a7464e5fd642eb")
        );
        let last = item.last_event.as_ref().unwrap();
        assert_eq!(last.event, "cleaned");
        assert_eq!(last.at, "2026-09-12T18:21:58Z");
        let handoff = item.handoff.as_ref().unwrap();
        assert_eq!(handoff.path, plane.path().join("handoffs").join(HANDOFF_T0011_NAME));
        assert_eq!(handoff.blockers, "none");
    }

    #[test]
    fn load_inbox_without_tasks_directory_is_empty() {
        let plane = tempfile::tempdir().unwrap();
        put(plane.path(), "board.md", BOARD);
        assert!(load_inbox(plane.path()).unwrap().is_empty());
    }

    #[test]
    fn load_inbox_without_board_or_handoffs_has_no_event() {
        let plane = tempfile::tempdir().unwrap();
        put(&plane.path().join("tasks"), "T-0011.md", TASK_T0011);
        let items = load_inbox(plane.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].last_event, None);
        assert_eq!(items[0].handoff, None);
    }

    #[test]
    fn load_inbox_newest_handoff_wins() {
        let plane = tempfile::tempdir().unwrap();
        let handoffs = plane.path().join("handoffs");
        put(&plane.path().join("tasks"), "T-0901.md", &task_as("T-0901", "done"));
        put(&handoffs, HANDOFF_T0901_NAME, HANDOFF_T0901);
        let newer = HANDOFF_T0011
            .replace("task: T-0011", "task: T-0901")
            .replace("at: 2026-09-13T13:06:01Z", "at: 2026-09-13T13:06:02Z");
        let newer_name = HANDOFF_T0011_NAME.replace("T-0011", "T-0901");
        put(&handoffs, &newer_name, &newer);
        // Another task's handoff whose id only extends this one.
        put(&handoffs, "2026-09-14T00-00-00Z_fixer__to__human__T-09011.md", HANDOFF_T0901);

        let items = load_inbox(plane.path()).unwrap();
        let handoff = items[0].handoff.as_ref().unwrap();
        assert_eq!(handoff.path, handoffs.join(&newer_name));
        assert_eq!(handoff.status, TaskStatus::Done);
        assert_eq!(handoff.blockers, "none");
    }

    #[test]
    fn load_inbox_skips_task_without_id_and_unreadable_files() {
        let plane = tempfile::tempdir().unwrap();
        let tasks = plane.path().join("tasks");
        put(&tasks, "T-0011.md", TASK_T0011);
        put(&tasks, "no-id.md", &TASK_T0011.replace("id: T-0011\n", ""));
        put(&tasks, "notes.txt", &task_as("T-0999", "blocked"));
        fs::write(tasks.join("broken.md"), [0xff, 0xfe, 0x00, 0xc3]).unwrap();

        let items = load_inbox(plane.path()).unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["T-0011"]);
    }

    #[test]
    fn load_inbox_sorts_attention_then_recency_then_id() {
        let plane = tempfile::tempdir().unwrap();
        let tasks = plane.path().join("tasks");
        put(&tasks, "T-0011.md", TASK_T0011);
        put(&tasks, "T-0002.md", &task_as("T-0002", "done"));
        put(&tasks, "T-0001.md", &task_as("T-0001", "done"));
        put(&tasks, "T-0010.md", &task_as("T-0010", "done"));
        put(&tasks, "T-0901.md", &task_as("T-0901", "blocked"));
        put(&tasks, "T-0902.md", &task_as("T-0902", "needs-review"));
        put(plane.path(), "board.md", &format!(
            "{BOARD}| 2026-09-10T00:00:00Z | T-0902 | handoff | needs-review |\n"
        ));

        let items = load_inbox(plane.path()).unwrap();
        let ids: Vec<&str> = items.iter().map(|i| i.id.as_str()).collect();
        // Attention first (with an event before without), then the
        // newest board event, then events-less items by id.
        assert_eq!(ids, ["T-0902", "T-0901", "T-0010", "T-0001", "T-0002", "T-0011"]);
    }
}
