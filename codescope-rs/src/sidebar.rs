//! Left-rail PROJECTS sidebar.
//!
//! Single-purpose view: lists every entry in the loaded
//! [`codescope_core::ProjectsConfig`] and lets the user select one.
//! The `+` button kicks off a directory picker and, on success,
//! appends a new project and persists `projects.json`. Right-click on
//! a project opens a context menu mirroring the C# build's
//! `BuildProjectMenu` — Reveal / Copy path / Open in Windows
//! Terminal / Remove project today, with `New worktree from branch…`
//! landing once the input dialog primitive exists.
//!
//! Layout (240 px wide):
//!
//! ```text
//! ┌───────────────┐
//! │ PROJECTS    + │ ← heading + add (file picker)
//! ├───────────────┤
//! │ filter…       │ ← (placeholder, wired later)
//! ├───────────────┤
//! │ ▍ project A   │ ← active = accent rail + frost bg
//! │   project B   │
//! │   project C   │
//! └───────────────┘
//! ```

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use codescope_core::{
    AgentProfile, AgentRegistry, AppPaths, LayoutState, Project, ProjectsConfig, Theme,
};
use codescope_core::git::GitStatus;
use codescope_core::pr::{CiStatus, PullRequestInfo};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Animation, AnimationExt, AppContext, ClipboardItem, Context, Corner, EventEmitter,
    ExternalPaths, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, anchored, deferred, div, point, px,
};

use crate::new_project_dialog::NewProjectDialogState;
use crate::new_worktree_dialog::NewWorktreeDialogState;
use crate::theme;

/// How often the dirty-state poller wakes up. 5 s is well under
/// the user's reaction-to-edit window (most workflows save +
/// glance at the sidebar within 1-2 seconds), and well over the
/// per-call I/O budget of `git status --porcelain` even on
/// thousand-file repos. Mirrors the C# build's
/// `WorktreePoller.Interval`.
const DIRTY_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How often the PR-status poller wakes up. 60 s matches the C# build's
/// `PullRequestStatusPoller.Interval` baseline (the C# build then layers
/// per-worktree exponential backoff up to 5 minutes; the Rust port
/// doesn't model backoff yet, but `gh` failures cache as
/// `Resolved { info: None }` so they don't retry hot anyway).
const PR_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// Default sidebar width when none is persisted in `layout.json`.
/// Mirrors the C# build's initial 240 px. The actual rendered width
/// is owned by `AppShell` and threaded through a parent wrapper —
/// drag-resize + collapse live there so the sidebar entity itself
/// doesn't have to know about its own chrome.
pub const SIDEBAR_DEFAULT_WIDTH: f32 = 240.0;
pub const SIDEBAR_MIN_WIDTH: f32 = 160.0;
pub const SIDEBAR_MAX_WIDTH: f32 = 600.0;

/// Open right-click context menu state. `None` when no menu is
/// showing. The position is in window coordinates so we can hand it
/// straight to [`anchored`] without recomputing on render.
///
/// One enum across project + worktree menus rather than two `Option`
/// fields so opening one always implicitly closes the other — the user
/// can never see two menus at once and we don't have to reconcile two
/// "did the row I anchor to disappear?" code paths on render.
enum OpenMenu {
    Project { project_idx: usize, position: Point<Pixels> },
    Worktree {
        project_idx: usize,
        /// Stable id of the right-clicked worktree. Looked up in
        /// `project.worktrees` at action time so a concurrent
        /// `projects.json` rewrite (or a primary worktree slipping
        /// into the list) can't shift the row underneath us. Indexing
        /// would break here because the sidebar enumerates *only*
        /// non-primary rows for display, while the action handlers
        /// search the full list — using the id sidesteps both axes.
        worktree_id: String,
        position: Point<Pixels>,
    },
}

/// Per-worktree state for the lazy `gh pr list` lookup. `Pending`
/// flips to `Resolved` once the background task lands; the resolved
/// value carries the branch it ran against so a subsequent open with
/// a *different* branch invalidates the cache and triggers a refetch
/// (the C# build's poller does the same — branch is the cache key on
/// its end).
#[derive(Debug, Clone, PartialEq, Eq)]
enum PrLookup {
    /// A `fetch_for_branch` task is already in flight for this
    /// worktree. Records the branch the call was launched against so
    /// the completion handler can detect a branch-switch-during-fetch
    /// and drop the now-stale result instead of caching it.
    Pending { branch: String },
    /// The most recent lookup completed against `branch` and resolved
    /// to `info` (`None` for "no open PR / call failed / gh not
    /// installed"). Carries the full [`PullRequestInfo`] when present
    /// so the sidebar can render the badge + CI glyph and surface
    /// "Open PR in browser" / "Copy PR URL" menu rows without
    /// re-fetching.
    Resolved {
        branch: String,
        info: Option<PullRequestInfo>,
    },
}

/// Click handler for a context-menu row. Boxed so we can stash it in
/// the closure passed to `cx.listener` without leaking the helper's
/// generic-over-fn shape into every menu-row construction site.
/// Receives the active window so a row that opens a follow-on dialog
/// (e.g. "New worktree…") can focus the dialog inline.
type MenuItemAction = Box<dyn Fn(&mut Sidebar, &mut Window, &mut Context<Sidebar>) + 'static>;

/// Events the sidebar emits up to its host (`AppShell`). Today there
/// is just one — "open a session at this path" — fired when the user
/// clicks a worktree row or right after the new-worktree dialog
/// successfully creates one. Keeping the surface in an enum so adding
/// more events (e.g. `OpenProjectFolder`) doesn't require changing
/// the trait wiring.
#[derive(Debug, Clone)]
pub enum SidebarEvent {
    /// Open a session for `working_directory`, using `title` for the
    /// tab strip. Title is just a label — the host decides what to
    /// actually show (in practice the worktree branch name suffixed
    /// with the project name).
    ///
    /// Default semantics are *focus-or-open*: the host activates an
    /// existing tab whose working directory matches and only spawns a
    /// new one when nothing matches. Set `force_new: true` to always
    /// spawn (used by the explicit "New session" / "New Claude
    /// session" rows in the project context menu and the worktree
    /// menu's "New Claude session" row).
    OpenSession {
        working_directory: PathBuf,
        title: SharedString,
        /// Optional command to auto-type at the shell prompt once the
        /// pty has come up. Used by the agent-launch rows ("New
        /// Claude session", and any future agent variants) to fire
        /// the agent inline; `None` just opens a plain shell. The
        /// host adds the trailing CR.
        auto_type: Option<SharedString>,
        /// When `true`, the host always spawns a fresh tab — used by
        /// the project menu's "New session" / "New Claude session"
        /// rows, the worktree menu's "New Claude session" row, and
        /// the new-worktree dialog's auto-spawn. When `false`, the
        /// host activates an existing tab whose `working_directory`
        /// matches and only spawns a new one if no match is found —
        /// the desired behaviour for a plain worktree row click and
        /// the worktree menu's "Open session" row, both of which
        /// would otherwise pile up a duplicate tab on every click.
        force_new: bool,
    },
    /// Surface a status notification to the user. The sidebar emits
    /// these from menu actions (pull / fetch / open remote / discard)
    /// so the AppShell's toast layer can render them; without this
    /// channel sidebar errors would only land in stderr where the
    /// user can't see them. Severity drives the toast colour stripe
    /// and lifetime.
    Toast { kind: ToastSeverity, title: SharedString, detail: Option<SharedString> },
    /// User clicked the sidebar's "Overview" footer button. The C#
    /// build maps Ctrl+Shift+O / this button to a full-window
    /// `OverviewView` that replaces the workspace layer. The Rust
    /// port doesn't have that view yet — AppShell currently surfaces
    /// a "coming soon" toast — so this event is a placeholder hook
    /// the host can wire up to the real Overview once it lands.
    OpenOverview,
    /// Reopen a soft-closed session by id. AppShell looks up the row,
    /// calls `SessionManager::reopen`, then spawns a tab pinned to
    /// the persisted `worktree_path` + `agent_id`. Mirrors C#
    /// `MainViewModel.ReopenClosedSessionAsync`.
    ReopenSession { session_id: String },
}

/// Toast severity emitted by the sidebar. AppShell maps these to its
/// own `ToastKind` (the indirection keeps `Sidebar` from importing
/// `app.rs` types).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // `Info` is reserved for future non-error notices.
pub enum ToastSeverity {
    Ok,
    Err,
    Info,
}

/// Owned snapshot of one non-primary worktree, captured before render
/// loops over the project list, so each row's listener closure can
/// `move` it without taking an outliving borrow on `self.projects`.
#[derive(Clone)]
struct WorktreeRowData {
    id: String,
    path: String,
    /// Cached canonical form of `path`
    /// (`codescope_core::path_canon::canonicalize_path`). Stored once
    /// per row build so the per-row + per-project busy/active lookups
    /// don't allocate a fresh `String` every render frame — the busy
    /// halo animation requests a per-frame redraw, and a project
    /// with many worktrees would otherwise turn into a steady
    /// allocation hotspot (Copilot review on PR #136).
    canonical_path: String,
    branch: Option<String>,
    /// Closed sessions belonging to this worktree, newest-first
    /// (matches `SessionManager::closed`'s sort). Already capped by
    /// the retention sweep, so we render every row here without a
    /// per-render slice. Used to drive the per-worktree history
    /// disclosure in the sidebar — mirrors C#
    /// `WorktreeViewModel.History`.
    closed_sessions: Vec<ClosedSessionRow>,
}

/// Resolve a persisted `Session.agent_id` to the display name the
/// sidebar's closed-session history row should show ("Claude Code",
/// "Copilot CLI", …). Routed through [`codescope_core::AgentRegistry`]
/// so the registry stays the single source of truth — no hard-coded
/// id → label map drifts out of sync when an agent is renamed.
///
/// Normalises the id before lookup so legacy / loosely-spelled values
/// resolve too:
/// - ASCII-lowercase (registry lookup is already case-insensitive,
///   this just lets us split on a known case).
/// - First `-` / `_` / whitespace-delimited token, so `"claude-code"`
///   (an alias the C# build round-trips in some configs) maps to
///   `"claude"`.
///
/// Returns `None` for unknown / empty ids; the caller falls back to
/// `"shell"` so plain pty tabs still render with a friendly label.
fn history_agent_display_name(agent_id: &str) -> Option<&'static str> {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<codescope_core::AgentRegistry> = OnceLock::new();
    let registry = REGISTRY.get_or_init(codescope_core::AgentRegistry::with_built_ins);

    let normalized = agent_id.to_ascii_lowercase();
    let first_token = normalized
        .split(|c: char| c == '-' || c == '_' || c.is_whitespace())
        .find(|s| !s.is_empty())?;
    let profile = registry.get_by_id(first_token)?;
    // Match the agent id back to a `&'static str` literal so the
    // closed-row label can stay a non-allocating return type — the
    // five built-in defaults are stable strings, and any custom
    // override would arrive via a different code path.
    Some(match profile.id.as_str() {
        "claude" => "Claude Code",
        "copilot" => "Copilot CLI",
        "opencode" => "OpenCode",
        "pi" => "Pi",
        "codex" => "Codex",
        _ => return None,
    })
}

/// Display data for a single closed-session row inside a worktree's
/// history disclosure. Snapshotted at render time so the per-row
/// listener closure can hold an owned `session_id` without keeping
/// a borrow on `self.projects`.
#[derive(Clone)]
struct ClosedSessionRow {
    session_id: String,
    label: SharedString,
    closed_at: Option<String>,
}

pub struct Sidebar {
    projects: ProjectsConfig,
    /// Index of the currently-selected project. `None` when no
    /// projects exist yet.
    selected: Option<usize>,
    theme: Arc<Theme>,
    /// Where `projects.json` and `layout.json` live. Threaded in so
    /// add/remove + selection changes can persist without re-detecting
    /// the env.
    paths: Arc<AppPaths>,
    /// In-memory copy of `layout.json` — kept in sync as the user
    /// changes selection so a save-on-change writes out the full
    /// (correct) struct, not just the field we touched.
    layout: LayoutState,
    /// Currently-open project context menu, if any.
    menu: Option<OpenMenu>,
    /// Currently-open "New worktree from branch…" dialog, if any.
    /// At most one dialog at a time — opening another would race
    /// against an in-flight `git worktree add` call from the first.
    dialog: Option<NewWorktreeDialogState>,
    /// Currently-open "Add project" dialog, if any. Mutually
    /// exclusive with `dialog`: the "+" button only opens this one
    /// when no other dialog is showing.
    new_project_dialog: Option<NewProjectDialogState>,
    /// Per-worktree clean/dirty state, keyed by absolute path.
    /// Updated by the background poller spawned via
    /// `start_dirty_poll`, which `AppShell::new` calls from inside
    /// the `cx.new(|cx| { … })` closure that builds the Sidebar
    /// entity (we can't call it from `Sidebar::new` itself because
    /// that signature has no `Context<Sidebar>`). `None` = unknown
    /// (still loading), `Some(true)` = dirty, `Some(false)` =
    /// clean. The render loop maps that to a tiny status dot next
    /// to each worktree row. Mirrors the C# build's
    /// `WorktreePoller` cache.
    dirty_state: HashMap<String, bool>,
    /// Rich per-worktree git status: branch, numstat diff, and
    /// ahead/behind counts. Keyed by absolute path, populated by
    /// `start_git_status_poll` (also every 5 s). `None` while the
    /// first poll tick hasn't completed yet; callers should treat
    /// absence as "unknown / loading" and fall back to cached
    /// `dirty_state` for the dirty dot. Mirrors the intent of the
    /// C# build's `WorktreePoller` but adds the richer data the
    /// status bar needs.
    git_status: HashMap<String, GitStatus>,
    /// Cached "open PR URL for this worktree" lookup, keyed by the
    /// worktree's absolute path. The value tracks both the branch
    /// the lookup ran against (so a branch switch invalidates the
    /// cache) and an in-flight `Pending` marker (so a rapid double-
    /// click on the worktree row can't spawn two concurrent `gh pr
    /// list` processes). A missing key means we haven't asked yet;
    /// opening the menu kicks off a fetch when the branch is known.
    /// Mirrors the cached `WorktreeViewModel.PullRequest` slot the
    /// C# build's `PullRequestStatusPoller` writes into. Cache is
    /// per-process, in-memory only — no on-disk persistence, since
    /// the URL can shift if the PR is closed and re-opened.
    pr_urls: HashMap<String, PrLookup>,
    /// Project ids the user has explicitly collapsed. Projects start
    /// expanded by default; toggling the chevron adds/removes the id
    /// here. Mirrors the C# `TreeViewItem.IsExpanded` state per
    /// project — except the C# build hangs that off WPF's tree
    /// control, while we keep an explicit `HashSet<String>` so the
    /// render loop can decide visibility without per-row state.
    /// Hydrated from `layout.collapsed_projects` on construction and
    /// flushed back via `save_layout` whenever the user toggles a
    /// chevron, so the disclosure state survives a restart.
    collapsed_projects: HashSet<String>,
    /// Worktree rows whose closed-session history disclosure is
    /// expanded. Keyed by `"{project_id}/{worktree_id}"` so primary
    /// rows from different projects don't collide. Defaults to
    /// collapsed; toggling the chevron flips the entry. Process-local
    /// only — mirrors `WorktreeViewModel.IsExpanded` in the C# build,
    /// which is also a per-process flag (WPF tree-view state).
    expanded_worktrees: HashSet<String>,
    /// Canonicalised paths (via [`codescope_core::path_canon::canonicalize_path`])
    /// whose adopted agent session is currently in `Busy` or
    /// `PendingToolUse` state. Drives the red `busy` dot on the
    /// worktree row plus the red propagation dot on a collapsed
    /// project row. Pushed by [`Sidebar::set_session_paths`] from
    /// `AppShell::start_telemetry_poll`. Mirrors the C# build's
    /// `WorktreeViewModel.HasBusySession`/`DotState` derived state.
    busy_paths: HashSet<String>,
    /// Canonicalised paths that have at least one live adopted
    /// session (regardless of busy/idle). Drives the left-edge accent
    /// rail on worktree rows and the idle (green) dot state. Mirrors
    /// the C# `WorktreeViewModel.HasActiveSession` boolean — same
    /// 2 px rail trigger on the sidebar row chrome.
    active_paths: HashSet<String>,
    /// Whether the Overview panel is currently on stage. Pushed by
    /// `AppShell::set_show_overview`; drives the footer "Overview"
    /// button's active look (accent rail + accent foreground) so the
    /// user can see at a glance which mode the workspace is in.
    /// Mirrors the C# `Sidebar.OverviewButton`'s `IsOverviewVisible`
    /// DataTrigger.
    overview_visible: bool,
    /// Registry of agent profiles (claude / codex / opencode / copilot
    /// / pi by default, plus any `settings.agents` overrides). Threaded
    /// in from `AppShell` so the worktree + project context menus can
    /// render one "New {DisplayName} session" row per profile and pick
    /// the user-flagged default. Mirrors C# `SidebarViewModel.AvailableAgents`
    /// driving `BuildAgentChoices` in `SidebarView.xaml.cs`.
    agent_registry: AgentRegistry,
    /// Case-insensitive substring filter applied to project and
    /// worktree rows (matched against project name, worktree branch,
    /// and folder leaf). Empty → no filtering. Lives in memory only;
    /// not persisted to `layout.json` because a stale filter on next
    /// launch would just look like the projects vanished. Mirrors C#
    /// `SidebarViewModel.FilterText` (process-local).
    filter: String,
    /// Focus handle for the filter input so on_key_down fires only
    /// when the user has clicked into the search box. Without it, the
    /// sidebar would swallow every keystroke globally.
    filter_focus: FocusHandle,
}

impl Sidebar {
    pub fn new(
        projects: ProjectsConfig,
        layout: LayoutState,
        theme: Arc<Theme>,
        paths: Arc<AppPaths>,
        agent_registry: AgentRegistry,
        filter_focus: FocusHandle,
    ) -> Self {
        // Restore last-opened project if it still exists. Falls back
        // to the first project when the saved id is gone (project
        // removed between sessions) or absent (first launch).
        let selected = match layout.selected_project_id.as_deref() {
            Some(id) => projects.projects.iter().position(|p| p.id == id),
            None => None,
        }
        .or_else(|| (!projects.projects.is_empty()).then_some(0));
        // Restore collapsed-project ids from disk, dropping any ids
        // that no longer match a known project so a removed-then-
        // re-added project doesn't inherit a stale collapsed flag.
        // Mirror the filtered set back into `layout.collapsed_projects`
        // (sorted) so any later `save_layout()` (selection change,
        // add/remove project) doesn't resurrect the stale ids we
        // dropped here.
        let live: HashSet<&str> =
            projects.projects.iter().map(|p| p.id.as_str()).collect();
        let collapsed_projects: HashSet<String> = layout
            .collapsed_projects
            .iter()
            .filter(|id| live.contains(id.as_str()))
            .cloned()
            .collect();
        let mut layout = layout;
        let mut filtered: Vec<String> = collapsed_projects.iter().cloned().collect();
        filtered.sort();
        layout.collapsed_projects = filtered;
        Self {
            projects,
            selected,
            theme,
            paths,
            layout,
            menu: None,
            dialog: None,
            new_project_dialog: None,
            dirty_state: HashMap::new(),
            git_status: HashMap::new(),
            pr_urls: HashMap::new(),
            collapsed_projects,
            expanded_worktrees: HashSet::new(),
            busy_paths: HashSet::new(),
            active_paths: HashSet::new(),
            overview_visible: false,
            agent_registry,
            filter: String::new(),
            filter_focus,
        }
    }

    /// Push the current Overview-panel visibility into the sidebar so
    /// the footer "Overview" button renders the right active look.
    /// Called by `AppShell::set_show_overview` so the indicator stays
    /// in lockstep with the panel state. No-op when the flag is
    /// unchanged so we don't notify on idle ticks.
    pub fn set_overview_visible(&mut self, value: bool, cx: &mut Context<Self>) {
        if self.overview_visible == value {
            return;
        }
        self.overview_visible = value;
        cx.notify();
    }

    /// Refresh the per-path agent activity snapshot used to colour
    /// the worktree-row state dot and surface the busy-child marker
    /// on collapsed project rows. Inputs are already canonicalised by
    /// the caller (see `codescope_core::path_canon::canonicalize_path`)
    /// — folding two paths to the same key here would either duplicate
    /// the work on every poll or hide subtle bugs where the caller
    /// forgot to canonicalise. No-op + no notify when neither set
    /// changed, so the 250 ms busy-poll doesn't trigger a redraw on
    /// every tick.
    ///
    /// Mirrors the implicit data flow the C# build gets for free:
    /// `WorktreeViewModel` listens to its child `SessionTabViewModel`s'
    /// `Status` changes and republishes `DotState` / `HasBusySession`.
    /// The Rust port doesn't have per-VM observable bindings, so
    /// `AppShell::start_telemetry_poll` recomputes both sets after
    /// each poll and pushes them down here.
    pub fn set_session_paths(
        &mut self,
        busy: HashSet<String>,
        active: HashSet<String>,
        cx: &mut Context<Self>,
    ) {
        if busy == self.busy_paths && active == self.active_paths {
            return;
        }
        self.busy_paths = busy;
        self.active_paths = active;
        cx.notify();
    }

    /// Flip the expand / collapse state for a worktree's history
    /// disclosure. Default is collapsed. Re-renders via `cx.notify`.
    fn toggle_worktree_expanded(&mut self, key: &str, cx: &mut Context<Self>) {
        if self.expanded_worktrees.contains(key) {
            self.expanded_worktrees.remove(key);
        } else {
            self.expanded_worktrees.insert(key.to_owned());
        }
        cx.notify();
    }

    /// Flip the collapse / expand state for the project at `id`.
    /// Default is expanded; toggling for the first time collapses,
    /// next toggle expands. Re-renders via `cx.notify`.
    fn toggle_project_collapsed(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.collapsed_projects.contains(id) {
            self.collapsed_projects.remove(id);
        } else {
            self.collapsed_projects.insert(id.to_owned());
        }
        self.sync_layout_collapsed_projects();
        self.save_layout();
        cx.notify();
    }

    /// Mirror the in-memory `collapsed_projects` set onto the in-memory
    /// `LayoutState` copy so the next `save_layout` writes the current
    /// disclosure state. Sorted for stable on-disk ordering — handy
    /// for diffs and hand-edits.
    fn sync_layout_collapsed_projects(&mut self) {
        let mut ids: Vec<String> = self.collapsed_projects.iter().cloned().collect();
        ids.sort();
        self.layout.collapsed_projects = ids;
    }

    /// Drop any `collapsed_projects` entries that no longer exist in
    /// the current `projects.projects` list. Called after every
    /// mutation that removes or replaces project rows so the set
    /// can't grow monotonically across a long-running session
    /// (project removed → its id sat there forever; same id later
    /// re-added would inherit the stale collapsed flag).
    fn prune_collapsed_projects(&mut self) {
        if self.collapsed_projects.is_empty() {
            return;
        }
        let live: HashSet<&str> = self.projects.projects.iter().map(|p| p.id.as_str()).collect();
        let before = self.collapsed_projects.len();
        self.collapsed_projects.retain(|id| live.contains(id.as_str()));
        if self.collapsed_projects.len() != before {
            // Pruning ran before persistence existed, so callers got
            // away with leaving the on-disk copy alone. Now that we
            // persist, mirror the cleaned set back into `layout` so
            // the next save reflects reality. Caller decides whether
            // to flush — keeping it lockstep with the existing
            // `replace_projects` / `remove_project` write order is
            // their job.
            self.sync_layout_collapsed_projects();
        }
    }

    /// Spawn the dirty-state polling loop. Runs every
    /// `DIRTY_POLL_INTERVAL` and walks every known worktree path,
    /// running `git status --porcelain`. Per-call latency is in the
    /// 10–50 ms range on small repos, low single-digit seconds on
    /// large ones — both well below the poll interval. The loop
    /// dies when the entity drops (`update` returns Err). Mirrors
    /// the C# build's `WorktreePoller`.
    ///
    /// Called from `AppShell::new` after the entity is constructed;
    /// can't run inside `Sidebar::new` itself because we don't have
    /// `cx.spawn` access until we're past the constructor.
    pub fn start_dirty_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DIRTY_POLL_INTERVAL).await;
                if this.upgrade().is_none() {
                    break;
                }
                // Snapshot every worktree path under each project
                // into a `HashSet` so duplicates (primary worktree
                // also listed under `worktrees`, two projects sharing
                // a path, …) only run `git status` once per tick.
                let paths: HashSet<String> = match this.update(cx, |this, _| {
                    this.projects
                        .projects
                        .iter()
                        .flat_map(|p| {
                            // Primary path counts too — the Sidebar
                            // only renders non-primary worktrees, but
                            // we still want the dirty-state cache
                            // populated for the project row's
                            // worktree.
                            std::iter::once(p.path.clone()).chain(
                                p.worktrees.iter().map(|wt| wt.path.clone()),
                            )
                        })
                        .collect()
                }) {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let known: HashSet<String> = paths.clone();
                let updates: Vec<(String, bool)> = cx
                    .background_spawn(async move {
                        paths
                            .into_iter()
                            .filter_map(|p| {
                                let path = std::path::PathBuf::from(&p);
                                codescope_core::git::is_dirty(&path)
                                    .ok()
                                    .map(|dirty| (p, dirty))
                            })
                            .collect()
                    })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        let mut changed = false;
                        // Prune cache entries for paths that no longer
                        // exist in the current project list — projects
                        // / worktrees can be removed between ticks and
                        // the map would otherwise grow stale.
                        let prev_len = this.dirty_state.len();
                        this.dirty_state.retain(|path, _| known.contains(path));
                        if this.dirty_state.len() != prev_len {
                            changed = true;
                        }
                        // Same prune for the PR URL cache — paths that
                        // are no longer in any project must drop their
                        // cached `Resolved` / `Pending` entries.
                        this.pr_urls.retain(|path, _| known.contains(path));
                        for (path, dirty) in updates {
                            let prev = this.dirty_state.insert(path, dirty);
                            if prev != Some(dirty) {
                                changed = true;
                            }
                        }
                        if changed {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Spawn the git-status polling loop. Runs every
    /// [`DIRTY_POLL_INTERVAL`] (shared constant with the dirty poll —
    /// same 5 s cadence) and walks every known worktree path, running
    /// three git queries per path: `symbolic-ref`, `diff --numstat`, and
    /// `rev-list --left-right --count`. Results are cached in
    /// `self.git_status` and exposed via [`Self::git_status_for`].
    ///
    /// Called from `AppShell::new` alongside `start_dirty_poll`;
    /// both loops share the same interval so the two polls run
    /// roughly in sync (within the same 5 s window) without
    /// coordinating with each other.
    pub fn start_git_status_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(DIRTY_POLL_INTERVAL).await;
                if this.upgrade().is_none() {
                    break;
                }
                // Snapshot every worktree path into a de-duplicated set
                // (same de-dup rationale as start_dirty_poll).
                let paths: HashSet<String> = match this.update(cx, |this, _| {
                    this.projects
                        .projects
                        .iter()
                        .flat_map(|p| {
                            std::iter::once(p.path.clone()).chain(
                                p.worktrees.iter().map(|wt| wt.path.clone()),
                            )
                        })
                        .collect()
                }) {
                    Ok(p) => p,
                    Err(_) => break,
                };
                let known: HashSet<String> = paths.clone();
                let updates: Vec<(String, GitStatus)> = cx
                    .background_spawn(async move {
                        paths
                            .into_iter()
                            .filter_map(|p| {
                                codescope_core::git::git_status(&p).map(|s| (p, s))
                            })
                            .collect()
                    })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        let mut changed = false;
                        // Prune stale paths (removed projects / worktrees).
                        let prev_len = this.git_status.len();
                        this.git_status.retain(|path, _| known.contains(path));
                        if this.git_status.len() != prev_len {
                            changed = true;
                        }
                        for (path, status) in updates {
                            let prev = this.git_status.insert(path, status.clone());
                            if prev.as_ref() != Some(&status) {
                                changed = true;
                            }
                        }
                        if changed {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Spawn the PR-status polling loop. Runs every
    /// [`PR_POLL_INTERVAL`] (60 s) and walks every worktree whose live
    /// branch is known, shelling out to `gh pr list` once per
    /// (path, branch) pair. Results land in `self.pr_urls` as
    /// `Resolved { info: Some(_) | None }` — `None` covers "no open
    /// PR / gh not installed / call failed" and stays cached so the
    /// next tick doesn't re-spawn until the branch changes.
    ///
    /// Network work is dispatched through `cx.background_spawn` so
    /// the UI thread never blocks on `gh`. Mirrors the C# build's
    /// `PullRequestStatusPoller` minus the per-worktree exponential
    /// backoff (which only kicks in on persistent failures; the
    /// uniform 60 s cadence is cheap enough at the typical worktree
    /// count to justify deferring backoff to a follow-up).
    ///
    /// Called from `AppShell::new` alongside `start_dirty_poll` and
    /// `start_git_status_poll`. Same lifetime — dies when the entity
    /// drops.
    pub fn start_pr_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(PR_POLL_INTERVAL).await;
                if this.upgrade().is_none() {
                    break;
                }
                // Snapshot every (path, branch) pair we should poll.
                // A worktree without a live branch (no git_status tick
                // yet and no persisted branch) is skipped — the next
                // tick will pick it up once the git-status poller
                // populates the cache. Pairs are de-duplicated by
                // path to avoid double-polling when the same path is
                // listed under multiple projects.
                let targets: Vec<(String, String)> = match this
                    .update(cx, |this, _| {
                        let mut seen: HashSet<String> = HashSet::new();
                        let mut out: Vec<(String, String)> = Vec::new();
                        for project in &this.projects.projects {
                            let entries = std::iter::once((
                                project.path.clone(),
                                None::<String>,
                            ))
                            .chain(
                                project.worktrees.iter().map(|wt| {
                                    (wt.path.clone(), wt.branch.clone())
                                }),
                            );
                            for (path, persisted_branch) in entries {
                                if !seen.insert(path.clone()) {
                                    continue;
                                }
                                let branch = this
                                    .git_status
                                    .get(&path)
                                    .map(|s| s.branch.clone())
                                    .or(persisted_branch);
                                if let Some(branch) = branch
                                    && !branch.is_empty()
                                {
                                    out.push((path, branch));
                                }
                            }
                        }
                        out
                    }) {
                    Ok(v) => v,
                    Err(_) => break,
                };

                if targets.is_empty() {
                    continue;
                }

                // Run all `gh` calls sequentially on the background
                // executor. Per-call latency is the dominant cost
                // here (network + auth), but gh's own concurrency
                // story is best-effort and we'd rather not fan out
                // and hammer the API — at worst a 60 s tick walks N
                // worktrees serially and finishes well inside the
                // next tick window. Each result records the branch
                // it was fetched against so the apply step below can
                // detect a branch switch between dispatch and
                // completion (rare but possible).
                let results: Vec<(String, String, Option<PullRequestInfo>)> = cx
                    .background_spawn(async move {
                        targets
                            .into_iter()
                            .map(|(path, branch)| {
                                let info = codescope_core::pr::fetch_for_branch(
                                    std::path::Path::new(&path),
                                    &branch,
                                );
                                (path, branch, info)
                            })
                            .collect()
                    })
                    .await;

                if this
                    .update(cx, |this, cx| {
                        let mut changed = false;
                        for (path, branch, info) in results {
                            // Drop the result if the worktree was
                            // removed or its branch shifted while gh
                            // was running. The lazy `open_worktree_menu`
                            // fetch + the next tick will reconverge.
                            let live_branch = this
                                .git_status
                                .get(&path)
                                .map(|s| s.branch.clone())
                                .or_else(|| {
                                    this.projects
                                        .projects
                                        .iter()
                                        .flat_map(|p| {
                                            std::iter::once((p.path.clone(), None))
                                                .chain(p.worktrees.iter().map(|wt| {
                                                    (wt.path.clone(), wt.branch.clone())
                                                }))
                                        })
                                        .find(|(p, _)| p == &path)
                                        .and_then(|(_, b)| b)
                                });
                            if live_branch.as_deref() != Some(branch.as_str()) {
                                continue;
                            }
                            // A `Pending` against a different branch
                            // means a fresher lazy fetch is in flight;
                            // don't clobber it with this poll's stale
                            // answer.
                            if matches!(
                                this.pr_urls.get(&path),
                                Some(PrLookup::Pending { branch: b }) if b != &branch
                            ) {
                                continue;
                            }
                            let next = PrLookup::Resolved {
                                branch: branch.clone(),
                                info,
                            };
                            let prev = this.pr_urls.insert(path, next.clone());
                            if prev.as_ref() != Some(&next) {
                                changed = true;
                            }
                        }
                        if changed {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Return the cached [`GitStatus`] for the given worktree path.
    /// Returns `None` while the first poll tick has not completed yet,
    /// or for paths that are not git repos. Callers should treat `None`
    /// as "loading" and fall back to a neutral display (e.g. no branch
    /// label, no ahead/behind count).
    ///
    /// `allow(dead_code)` until the status-bar integration PR wires
    /// the consumer — the cache itself is populated by the polling
    /// loop and remains useful for the next consumer to pick up.
    #[allow(dead_code)]
    pub fn git_status_for(&self, path: &str) -> Option<&GitStatus> {
        self.git_status.get(path)
    }

    /// Read-only handle to the in-memory project list. Exposed so the
    /// dialog module can read project metadata without re-borrowing
    /// every private field individually.
    pub(crate) fn projects(&self) -> &ProjectsConfig {
        &self.projects
    }

    /// Same as [`Self::projects`] for the path bundle. Used by the
    /// dialog to persist `projects.json` after a successful create.
    pub(crate) fn paths_ref(&self) -> &AppPaths {
        &self.paths
    }

    /// Workspace-wide worktree counts for the status bar's right
    /// cluster. Returns `(total, dirty)`:
    /// - `total` — every distinct worktree path across every project
    ///   (primary trees included), de-duplicated so a path that
    ///   appears in two projects (or as both primary and an explicit
    ///   `worktrees[]` entry) is only counted once.
    /// - `dirty` — number of those paths whose `dirty_state` entry
    ///   is `true`. `dirty_state` is a `HashMap<String, bool>`
    ///   keyed by absolute path: a missing key means the first poll
    ///   tick has not landed yet ("unknown"), `false` means
    ///   confirmed clean, and `true` means dirty. Both unknown and
    ///   clean paths are excluded from the count.
    pub(crate) fn worktree_counts(&self) -> (usize, usize) {
        let mut paths: HashSet<&str> =
            HashSet::new();
        for project in &self.projects.projects {
            paths.insert(project.path.as_str());
            for wt in &project.worktrees {
                paths.insert(wt.path.as_str());
            }
        }
        let total = paths.len();
        let dirty = paths
            .iter()
            .filter(|p| self.dirty_state.get(**p).copied().unwrap_or(false))
            .count();
        (total, dirty)
    }

    /// Commit a freshly-built `ProjectsConfig` into in-memory state
    /// after the dialog has already saved it to disk. Save-then-commit
    /// ordering matches `add_project` / `remove_project`.
    pub(crate) fn replace_projects(&mut self, next: ProjectsConfig) {
        self.projects = next;
        let prev_collapsed = self.layout.collapsed_projects.clone();
        self.prune_collapsed_projects();
        if self.layout.collapsed_projects != prev_collapsed {
            self.save_layout();
        }
    }

    /// Dialog accessors used by the dialog module's helpers. Kept
    /// `pub(crate)` so the dialog can mutate state without us having
    /// to expose the full struct.
    pub(crate) fn dialog(&self) -> Option<&NewWorktreeDialogState> {
        self.dialog.as_ref()
    }
    pub(crate) fn dialog_mut(&mut self) -> Option<&mut NewWorktreeDialogState> {
        self.dialog.as_mut()
    }
    pub(crate) fn set_dialog(&mut self, dialog: Option<NewWorktreeDialogState>) {
        self.dialog = dialog;
    }
    pub(crate) fn take_dialog(&mut self) -> Option<NewWorktreeDialogState> {
        self.dialog.take()
    }

    /// Add-project dialog accessors. Mutually exclusive with the
    /// new-worktree dialog — both render via `deferred(anchored(...))`
    /// at the same priority and we'd rather not have to decide which
    /// one wins on a frame. The "+" button gates open on
    /// `new_project_dialog().is_none() && dialog().is_none()`.
    pub(crate) fn new_project_dialog(&self) -> Option<&NewProjectDialogState> {
        self.new_project_dialog.as_ref()
    }
    pub(crate) fn new_project_dialog_mut(&mut self) -> Option<&mut NewProjectDialogState> {
        self.new_project_dialog.as_mut()
    }
    pub(crate) fn set_new_project_dialog(&mut self, dialog: Option<NewProjectDialogState>) {
        self.new_project_dialog = dialog;
    }
    pub(crate) fn take_new_project_dialog(&mut self) -> Option<NewProjectDialogState> {
        self.new_project_dialog.take()
    }

    /// Drop the open context menu without notifying — the caller is
    /// already going to call `cx.notify()` for a different reason.
    pub(crate) fn close_menu_no_notify(&mut self) {
        self.menu = None;
    }

    pub fn select(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.projects.projects.len() || self.selected == Some(idx) {
            // Out-of-range or re-clicking the active row — no-op,
            // skip the synchronous `layout.json` write.
            return;
        }
        self.selected = Some(idx);
        self.layout.selected_project_id =
            Some(self.projects.projects[idx].id.clone());
        self.save_layout();
        cx.notify();
    }

    /// Persist `layout.json` after a project-selection change.
    /// After PR #75 only `selected_project_id` is sidebar-owned —
    /// visibility and width moved to `AppShell`, so the sidebar
    /// entity doesn't have to know about its own chrome. A naive
    /// write of `self.layout` would clobber AppShell-owned fields
    /// (`group_weights`, `focused_group_index`, `sidebar_visible`,
    /// `sidebar_width`), so we reload from disk, overwrite only
    /// our slot, and save. Mirrors AppShell's `save_layout` to
    /// keep the merge-on-write story consistent.
    fn save_layout(&self) {
        let mut on_disk = match LayoutState::load(&self.paths) {
            Ok(state) => state,
            Err(err) => {
                eprintln!(
                    "warning: failed to read layout.json before save \
                     (using in-memory copy as base): {err:#}"
                );
                self.layout.clone()
            }
        };
        on_disk.selected_project_id = self.layout.selected_project_id.clone();
        on_disk.collapsed_projects = self.layout.collapsed_projects.clone();
        if let Err(err) = on_disk.save(&self.paths) {
            eprintln!("warning: failed to save layout.json: {err:#}");
        }
    }

    /// The project the user currently has selected, if any. AppShell
    /// reads this when spawning a new tab so the terminal lands in
    /// the right cwd.
    pub fn active_project(&self) -> Option<&Project> {
        self.selected.and_then(|idx| self.projects.projects.get(idx))
    }

    /// Apply a fresh theme snapshot. Called by the AppShell when the
    /// user changes themes — the sidebar redraws on the next frame.
    pub fn apply_theme(&mut self, theme: Arc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Open the project context menu at `position` (window coords)
    /// for the project at `idx`. No-op if the index is out of range.
    fn open_project_menu(
        &mut self,
        idx: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if idx >= self.projects.projects.len() {
            return;
        }
        self.menu = Some(OpenMenu::Project { project_idx: idx, position });
        cx.notify();
    }

    /// Open the worktree context menu at `position` for the worktree
    /// identified by `worktree_id` inside `project_idx`. No-op when
    /// either has shifted out from under the click (rare but possible
    /// if the projects file was rewritten between the right-click
    /// event being queued and us getting around to handling it).
    fn open_worktree_menu(
        &mut self,
        project_idx: usize,
        worktree_id: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.projects.get(project_idx) else { return };
        let Some(worktree) = project.worktrees.iter().find(|wt| wt.id == worktree_id) else {
            return;
        };
        // Resolve the live branch: the polled `git_status` cache is
        // authoritative (a `git switch` in another window will land
        // there long before anyone touches `projects.json`), the
        // persisted `worktree.branch` is the fallback. For a primary
        // worktree the persisted field is `None` — without the
        // git-status fallback we'd permanently suppress PR detection
        // on the primary row.
        let live_branch = self
            .git_status
            .get(&worktree.path)
            .map(|s| s.branch.clone())
            .or_else(|| worktree.branch.clone());

        // Kick off a one-shot `gh pr list` lookup the first time we
        // see a (path, branch) pair. Subsequent opens read the cache.
        // If the branch is currently unknown (no git_status tick yet
        // and no persisted branch), skip insertion entirely so the
        // next menu open can retry once data lands.
        if let Some(branch) = live_branch {
            let needs_fetch = match self.pr_urls.get(&worktree.path) {
                None => true,
                // An in-flight lookup against the *same* branch is
                // fine — let it complete. If the branch under the
                // pending fetch differs from the live branch, the
                // user switched mid-fetch; we'll spawn a fresh task
                // and the older one will be discarded on completion.
                Some(PrLookup::Pending { branch: pending }) => pending != &branch,
                // Branch switched out from under the cached lookup —
                // refetch. This fixes the "user switched branches and
                // the menu still shows the old PR / no PR" race.
                Some(PrLookup::Resolved { branch: cached, .. }) => cached != &branch,
            };
            if needs_fetch {
                self.pr_urls.insert(
                    worktree.path.clone(),
                    PrLookup::Pending { branch: branch.clone() },
                );
                let path = worktree.path.clone();
                let path_for_task = std::path::PathBuf::from(&path);
                let branch_for_task = branch.clone();
                cx.spawn(async move |this, cx| {
                    let info = cx
                        .background_spawn(async move {
                            codescope_core::pr::fetch_for_branch(
                                &path_for_task,
                                &branch_for_task,
                            )
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| {
                        // Branch may have shifted while the gh call
                        // was running. If the slot we wrote `Pending`
                        // into has been taken over by another lookup
                        // (or by a `Resolved` with a different
                        // branch), drop our result on the floor — the
                        // newer task will produce a fresher answer.
                        let still_ours = matches!(
                            this.pr_urls.get(&path),
                            Some(PrLookup::Pending { branch: b }) if b == &branch
                        );
                        if still_ours {
                            this.pr_urls.insert(
                                path,
                                PrLookup::Resolved { branch: branch.clone(), info },
                            );
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }

        self.menu = Some(OpenMenu::Worktree { project_idx, worktree_id, position });
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Reveal the project's working tree in the OS file browser.
    /// Mirrors the C# `RevealInExplorerCommand`.
    fn reveal_in_explorer(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.projects.get(idx) else { return };
        reveal_path_in_file_browser(&project.path);
        self.close_menu(cx);
    }

    /// `wt -d <path>` — opens Windows Terminal with its starting
    /// directory pinned to the project root. Mirrors C#'s
    /// `OpenInWindowsTerminalCommand`. No-op on non-Windows.
    fn open_in_windows_terminal(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.projects.get(idx) else { return };
        open_path_in_windows_terminal(&project.path);
        self.close_menu(cx);
    }

    /// Copy the project's absolute path to the system clipboard.
    /// Mirrors C#'s `CopyPathCommand` (Ctrl+Alt+C in the C# build —
    /// keybinding wiring lands when the global shortcut layer does).
    fn copy_path(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.projects.get(idx) else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(project.path.clone()));
        self.close_menu(cx);
    }

    /// Reveal a non-primary worktree in the OS file browser. Same
    /// platform fan-out as the project version — kept as a separate
    /// method so the menu wiring stays symmetric and future per-row
    /// behavior (e.g. selecting the worktree's branch dot in Explorer)
    /// has a place to land.
    fn reveal_worktree_in_explorer(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.worktree_path(project_idx, worktree_id) {
            reveal_path_in_file_browser(&path);
        }
        self.close_menu(cx);
    }

    fn open_worktree_in_windows_terminal(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.worktree_path(project_idx, worktree_id) {
            open_path_in_windows_terminal(&path);
        }
        self.close_menu(cx);
    }

    fn copy_worktree_path(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        cx: &mut Context<Self>,
    ) {
        if let Some(path) = self.worktree_path(project_idx, worktree_id) {
            cx.write_to_clipboard(ClipboardItem::new_string(path));
        }
        self.close_menu(cx);
    }

    /// Copy the worktree's branch name to the system clipboard. The
    /// menu row gates itself on `branch.is_some()` so this only
    /// surfaces when the worktree has a tracked branch (detached
    /// worktrees skip the row), but we double-check inside in case
    /// the row is reused from a future entry point. Mirrors C#'s
    /// `CopyBranchCommand`.
    fn copy_worktree_branch(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.projects.get(project_idx) else {
            self.close_menu(cx);
            return;
        };
        let branch = project
            .worktrees
            .iter()
            .find(|wt| wt.id == worktree_id)
            .and_then(|wt| wt.branch.clone());
        if let Some(branch) = branch {
            cx.write_to_clipboard(ClipboardItem::new_string(branch));
        }
        self.close_menu(cx);
    }

    /// Copy the cached PR URL for the worktree to the system
    /// clipboard, then surface a toast confirming the action. The
    /// menu row that drives this is gated on the cache holding a
    /// `Some(url)` value, so this is normally infallible by the time
    /// it runs — the inner `if let` is defensive against the cache
    /// being evicted between render and click. Mirrors C#'s
    /// `SidebarViewModel.CopyPullRequestUrlCommand`.
    fn copy_worktree_pr_url(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.worktree_path(project_idx, worktree_id) else {
            self.close_menu(cx);
            return;
        };
        // Re-derive the live branch and only copy when the cached
        // `Resolved.branch` still matches — if the user switched
        // branches between render and click the row is about to
        // disappear on the next open anyway, but a click that already
        // landed shouldn't paste the wrong URL.
        let live_branch = self
            .git_status
            .get(&path)
            .map(|s| s.branch.clone())
            .or_else(|| {
                self.projects
                    .projects
                    .get(project_idx)
                    .and_then(|p| p.worktrees.iter().find(|wt| wt.id == worktree_id))
                    .and_then(|wt| wt.branch.clone())
            });
        if let (Some(live), Some(PrLookup::Resolved { branch, info: Some(info) })) =
            (live_branch.as_ref(), self.pr_urls.get(&path))
            && branch == live
        {
            let url = info.url.clone();
            cx.write_to_clipboard(ClipboardItem::new_string(url.clone()));
            cx.emit(SidebarEvent::Toast {
                kind: ToastSeverity::Ok,
                title: "PR URL copied".into(),
                detail: Some(url.into()),
            });
        }
        self.close_menu(cx);
    }

    /// Open the cached PR URL for the worktree in the user's default
    /// browser. The menu row that drives this is gated on the cache
    /// holding a `Some(info)` value for the *current* branch — same
    /// guard as `copy_worktree_pr_url`. Mirrors C#'s
    /// `WorktreeViewModel.OpenPullRequestCommand`.
    fn open_worktree_pr_in_browser(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.worktree_path(project_idx, worktree_id) else {
            self.close_menu(cx);
            return;
        };
        let live_branch = self
            .git_status
            .get(&path)
            .map(|s| s.branch.clone())
            .or_else(|| {
                self.projects
                    .projects
                    .get(project_idx)
                    .and_then(|p| p.worktrees.iter().find(|wt| wt.id == worktree_id))
                    .and_then(|wt| wt.branch.clone())
            });
        if let (Some(live), Some(PrLookup::Resolved { branch, info: Some(info) })) =
            (live_branch.as_ref(), self.pr_urls.get(&path))
            && branch == live
        {
            let url = info.url.clone();
            open_url_in_browser(&url);
        }
        self.close_menu(cx);
    }

    /// Run `git pull --ff-only` on the worktree's path. Spawned on
    /// the background executor so the UI thread doesn't block on
    /// network I/O. Result surfaces as a toast — Ok on success, Err
    /// with stderr-derived detail on failure.
    fn pull_worktree(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.worktree_path(project_idx, worktree_id) else {
            self.close_menu(cx);
            return;
        };
        let label = self
            .worktree_display_label(project_idx, worktree_id)
            .unwrap_or_else(|| "this worktree".into());
        self.close_menu(cx);
        let path = std::path::PathBuf::from(path);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { codescope_core::git::pull_ff_only(&path) })
                .await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(_) => cx.emit(SidebarEvent::Toast {
                    kind: ToastSeverity::Ok,
                    title: format!("Pulled '{label}'").into(),
                    detail: None,
                }),
                Err(err) => cx.emit(SidebarEvent::Toast {
                    kind: ToastSeverity::Err,
                    title: format!("Pull failed for '{label}'").into(),
                    detail: Some(format!("{err:#}").into()),
                }),
            });
        })
        .detach();
    }

    /// `git rebase origin/<project-default-branch>` on the worktree's
    /// path. Spawned on the background executor so the UI doesn't
    /// block on the rebase. Result surfaces as a toast — Ok on
    /// success, Err with stderr-derived detail on conflict /
    /// failure (typical case is conflicts the user has to resolve
    /// manually). Mirrors C# `SidebarViewModel.RebaseOntoDefaultAsync`.
    fn rebase_worktree_onto_default(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.projects.get(project_idx) else {
            self.close_menu(cx);
            return;
        };
        let default_branch = project.default_branch.clone();
        let base_ref = format!("origin/{default_branch}");
        let Some(path) = self.worktree_path(project_idx, worktree_id) else {
            self.close_menu(cx);
            return;
        };
        let label = self
            .worktree_display_label(project_idx, worktree_id)
            .unwrap_or_else(|| "this worktree".into());
        self.close_menu(cx);
        let path = std::path::PathBuf::from(path);
        let base_ref_for_task = base_ref.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    codescope_core::git::rebase_onto(&path, &base_ref_for_task)
                })
                .await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(stdout) => {
                    // Surface the trimmed `git rebase` stdout in the
                    // toast detail when it has anything to show
                    // ("Successfully rebased and updated <ref>." or
                    // a list of cherry-picked commit summaries) —
                    // gives the user a one-glance confirmation
                    // beyond just the title. Empty stdout (rare,
                    // happens when the branch was already up to
                    // date) drops the detail line.
                    let detail = if stdout.is_empty() {
                        None
                    } else {
                        Some(stdout.into())
                    };
                    cx.emit(SidebarEvent::Toast {
                        kind: ToastSeverity::Ok,
                        title: format!("Rebased '{label}' onto {base_ref}").into(),
                        detail,
                    });
                }
                Err(err) => cx.emit(SidebarEvent::Toast {
                    kind: ToastSeverity::Err,
                    title: format!("Rebase failed for '{label}'").into(),
                    detail: Some(format!("{err:#}").into()),
                }),
            });
        })
        .detach();
    }

    /// Resolve the worktree's project remote URL, normalise it to
    /// a browser URL, and open it via the OS handler.
    ///
    /// **No-op when there's no `origin` remote** — we log to stderr
    /// and silently return; the menu row stays visible so the user
    /// gets a friendly explanation rather than a hidden affordance
    /// that flickers based on async probe state. The C# build
    /// gates the row up-front via `HasOriginRemote`; doing the
    /// equivalent here would require a sync `.git/config` scan on
    /// every render. We can revisit when there's a project-level
    /// "git capabilities" cache to back it. Mirrors C#'s
    /// `OpenRemoteRepositoryCommand`.
    fn open_worktree_remote_in_browser(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects.projects.get(project_idx) else {
            self.close_menu(cx);
            return;
        };
        // The worktree row's project is the project we look up the
        // origin URL from — different worktrees of the same project
        // share the same remote. We could also walk the worktree's
        // own .git but it's a `gitdir:` reference back to the
        // primary repo, so this is equivalent and faster.
        let _ = worktree_id; // silence unused: kept for API symmetry
        let project_path = std::path::PathBuf::from(&project.path);
        self.close_menu(cx);
        cx.spawn(async move |_, cx| {
            spawn_open_remote_in_browser(project_path, cx).await;
        })
        .detach();
    }

    /// Look up the on-disk path for a worktree by `worktree_id`.
    /// Returns `None` if the project / worktree has shifted out from
    /// under us (race with `add_project` / `remove_project` / external
    /// `projects.json` rewrite, or simply a stale id from a closed
    /// menu). Callers should silently no-op on `None` — the user will
    /// see the menu close and re-open with fresh data on the next
    /// click.
    fn worktree_path(&self, project_idx: usize, worktree_id: &str) -> Option<String> {
        self.projects
            .projects
            .get(project_idx)
            .and_then(|p| p.worktrees.iter().find(|wt| wt.id == worktree_id))
            .map(|wt| wt.path.clone())
    }

    /// Snapshot the metadata the "Remove worktree…" flow needs before
    /// we hand control off to an async task. Returning the values
    /// up-front means we don't have to re-borrow `self.projects` after
    /// the await point — the worktree may have moved or vanished by
    /// then.
    /// Friendly user-facing label for a worktree — branch name when
    /// tracked, otherwise the folder leaf. Used by every prompt /
    /// confirm dialog that names a worktree (Remove, Discard, …)
    /// so the strings stay consistent across actions.
    fn worktree_display_label(&self, project_idx: usize, worktree_id: &str) -> Option<String> {
        let wt = self
            .projects
            .projects
            .get(project_idx)?
            .worktrees
            .iter()
            .find(|wt| wt.id == worktree_id)?;
        Some(wt.branch.clone().unwrap_or_else(|| {
            std::path::Path::new(&wt.path)
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| wt.path.clone())
        }))
    }

    fn worktree_remove_context(
        &self,
        project_idx: usize,
        worktree_id: &str,
    ) -> Option<WorktreeRemoveContext> {
        let project = self.projects.projects.get(project_idx)?;
        let wt = project.worktrees.iter().find(|wt| wt.id == worktree_id)?;
        if wt.is_primary {
            return None;
        }
        let display_label = self
            .worktree_display_label(project_idx, worktree_id)
            .unwrap_or_else(|| wt.path.clone());
        Some(WorktreeRemoveContext {
            project_id: project.id.clone(),
            worktree_id: wt.id.clone(),
            project_path: project.path.clone(),
            worktree_path: wt.path.clone(),
            display_label,
        })
    }

    /// Confirm-then-run `git reset --hard HEAD` + `git clean -fd`
    /// for a worktree. Destructive — uses a `Critical`-level prompt
    /// so the user has to actively confirm. Mirrors C#'s
    /// `DiscardChangesCommand`.
    fn discard_worktree_changes(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = self.worktree_path(project_idx, worktree_id) else {
            self.close_menu(cx);
            return;
        };
        // Friendly label for the prompt — shared with the rest of
        // the worktree-action surface so wording stays consistent
        // (`Remove worktree '<label>'?` and `Discard all changes
        // in '<label>'?` line up).
        let label = self
            .worktree_display_label(project_idx, worktree_id)
            .unwrap_or_else(|| "this worktree".into());
        self.close_menu(cx);
        let prompt_msg = format!("Discard all changes in '{label}'?");
        let detail = format!(
            "Path: {path}\n\nThis runs `git reset --hard HEAD` followed \
             by `git clean -fd`. Untracked files and modifications to \
             tracked files will be lost — there's no undo."
        );
        let rx = window.prompt(
            gpui::PromptLevel::Critical,
            &prompt_msg,
            Some(&detail),
            &["Discard", "Cancel"],
            cx,
        );
        let path = std::path::PathBuf::from(path);
        let label_for_toast = label.clone();
        cx.spawn(async move |this, cx| {
            // 0 = first button ("Discard"). Anything else = cancel.
            match rx.await {
                Ok(0) => {}
                _ => return,
            }
            let result = cx
                .background_spawn(
                    async move { codescope_core::git::discard_all_changes(&path) },
                )
                .await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(_) => cx.emit(SidebarEvent::Toast {
                    kind: ToastSeverity::Ok,
                    title: format!("Discarded changes in '{label_for_toast}'").into(),
                    detail: None,
                }),
                Err(err) => cx.emit(SidebarEvent::Toast {
                    kind: ToastSeverity::Err,
                    title: format!("Discard failed for '{label_for_toast}'").into(),
                    detail: Some(format!("{err:#}").into()),
                }),
            });
        })
        .detach();
    }

    /// Drop a non-primary worktree from this project. Calls
    /// `git worktree remove` (force=false first; on failure prompts
    /// the user before retrying with `--force`), then rewrites
    /// `projects.json` so the row disappears from the sidebar.
    /// Mirrors `SidebarViewModel.RemoveWorktreeAsync` minus the
    /// session-close pre-step (the Rust port doesn't yet track which
    /// tabs are pinned to which worktree, so the user is responsible
    /// for closing them — the force-prompt covers the file-locked
    /// case).
    fn remove_worktree(
        &mut self,
        project_idx: usize,
        worktree_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ctx) = self.worktree_remove_context(project_idx, worktree_id) else {
            self.close_menu(cx);
            return;
        };
        self.close_menu(cx);

        let confirm_msg = format!("Delete worktree '{}'?", ctx.display_label);
        let confirm_detail = format!(
            "Path: {}\n\nOpen sessions stay running but lose their working directory. \
             Unpushed commits stay on the branch.",
            ctx.worktree_path
        );
        let rx = window.prompt(
            gpui::PromptLevel::Warning,
            &confirm_msg,
            Some(&confirm_detail),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            // 0 = first button ("Delete"). Anything else = cancel /
            // dialog dismissed.
            match rx.await {
                Ok(0) => {}
                _ => return,
            }
            run_remove_worktree_flow(this, ctx, cx).await;
        })
        .detach();
    }

    /// Run `git fetch --all --prune` against the project's primary
    /// repo. Spawned on the background executor so the UI thread
    /// doesn't block on network I/O. Mirrors C#'s `FetchAllCommand`.
    fn fetch_all_for_project(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.projects.get(idx) else {
            self.close_menu(cx);
            return;
        };
        let project_name = project.name.clone();
        let repo = std::path::PathBuf::from(&project.path);
        self.close_menu(cx);
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move { codescope_core::git::fetch_all_prune(&repo) })
                .await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(_) => cx.emit(SidebarEvent::Toast {
                    kind: ToastSeverity::Ok,
                    title: format!("Fetched '{project_name}'").into(),
                    detail: None,
                }),
                Err(err) => cx.emit(SidebarEvent::Toast {
                    kind: ToastSeverity::Err,
                    title: format!("Fetch failed for '{project_name}'").into(),
                    detail: Some(format!("{err:#}").into()),
                }),
            });
        })
        .detach();
    }

    /// Resolve the project's `remote.origin.url` and open the
    /// browser-shape URL via the OS handler. Same plumbing as
    /// `open_worktree_remote_in_browser` but at project scope.
    /// No-op + log when no origin is configured. Mirrors C#'s
    /// `OpenRemoteRepositoryCommand` (project scope).
    fn open_project_remote_in_browser(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.projects.get(idx) else {
            self.close_menu(cx);
            return;
        };
        let project_path = std::path::PathBuf::from(&project.path);
        self.close_menu(cx);
        cx.spawn(async move |_, cx| {
            spawn_open_remote_in_browser(project_path, cx).await;
        })
        .detach();
    }

    /// Drop a project from the sidebar list and persist `projects.json`.
    /// Does **not** touch anything on disk — the working tree stays
    /// where it is; the user just removes it from CodeScope's view.
    /// Save-then-commit ordering matches `add_project` so a write
    /// failure leaves both disk and UI in their previous state.
    fn remove_project(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.projects.projects.len() {
            return;
        }
        let prev_selected_id = self.layout.selected_project_id.clone();
        let prev_collapsed = self.layout.collapsed_projects.clone();
        let mut next = self.projects.clone();
        next.projects.remove(idx);
        if let Err(err) = next.save(&self.paths) {
            eprintln!("warning: failed to save projects.json: {err:#}");
            return;
        }
        self.projects = next;
        self.prune_collapsed_projects();
        // Selection housekeeping: if we just removed the selected
        // project, fall back to the previous row (or `None` when the
        // list is empty). Otherwise shift the cursor left when an
        // earlier row was removed so it keeps pointing at the same
        // project.
        self.selected = match self.selected {
            Some(sel) if sel == idx => {
                if self.projects.projects.is_empty() { None } else { Some(sel.min(self.projects.projects.len() - 1)) }
            }
            Some(sel) if sel > idx => Some(sel - 1),
            other => other,
        };
        self.layout.selected_project_id =
            self.selected.and_then(|i| self.projects.projects.get(i).map(|p| p.id.clone()));
        // Persist layout when either the persisted id or the
        // collapsed-projects list actually changed — removing the
        // active row rewrites the id, and removing a collapsed row
        // prunes its entry. Removing an unrelated, expanded row
        // leaves both intact and skips the write.
        if self.layout.selected_project_id != prev_selected_id
            || self.layout.collapsed_projects != prev_collapsed
        {
            self.save_layout();
        }
        self.close_menu(cx);
    }

    /// Append a project at `path` and persist. Newly-added project
    /// becomes the selection — the user just chose it, so dropping
    /// them straight into it is what they expect.
    ///
    /// Save-then-commit ordering: we build a candidate `ProjectsConfig`,
    /// write it to disk, and only swap it into `self.projects` (and
    /// touch `selected` / `layout.json`) once the write succeeds. A
    /// failed write therefore leaves both disk and UI in their
    /// previous consistent state, instead of producing an in-memory
    /// row that disappears on relaunch — and (worse) a `layout.json`
    /// pointing at a project id that never made it to `projects.json`.
    pub fn add_project(&mut self, path: String, cx: &mut Context<Self>) {
        // Refuse exact duplicates by path. Two rows pointing at the
        // same directory would let a user "add" the same project
        // twice and then wonder why both rows behave identically.
        if let Some(idx) = self.projects.projects.iter().position(|p| p.path == path) {
            self.select(idx, cx);
            return;
        }
        let project = Project::new(path);
        let new_id = project.id.clone();
        // Clone-then-save: failure leaves `self.projects` untouched.
        let mut next = self.projects.clone();
        next.projects.push(project);
        if let Err(err) = next.save(&self.paths) {
            eprintln!("warning: failed to save projects.json: {err:#}");
            return;
        }
        // Disk is committed; now mirror the change in memory.
        self.projects = next;
        let new_idx = self.projects.projects.len() - 1;
        self.selected = Some(new_idx);
        self.layout.selected_project_id = Some(new_id);
        self.save_layout();
        cx.notify();
    }
}

impl EventEmitter<SidebarEvent> for Sidebar {}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let selected = self.selected;
        // Snapshot project + worktree metadata (every worktree,
        // primary included — see the per-project comment below for
        // why) up front so each row's `cx.listener` closure can hold
        // owned values without overlapping the immutable borrow
        // `iter()` would otherwise extend across the rest of
        // `render`.
        let rows: Vec<(usize, String, SharedString, String, Vec<WorktreeRowData>)> = self
            .projects
            .projects
            .iter()
            .enumerate()
            .map(|(idx, p)| {
                // Render every worktree, primary included. The C#
                // sidebar template is keyed off the same flat
                // `Worktrees` list and shows the primary row too so
                // the user sees the branch they're currently on. The
                // earlier `!wt.is_primary` filter was a porting
                // mistake — it hid the primary checkout and made a
                // perfectly normal "single repo on main" project
                // render with the misleading "(no worktrees)"
                // placeholder from #101.
                // Pre-bucket the project's closed sessions by
                // worktree id so each `WorktreeRowData` carries its
                // own already-sorted history slice. The bucket key
                // matches the retention sweep's
                // `s.worktree_id.unwrap_or_default()` so primary
                // rows whose worktree id was never written collapse
                // cleanly into the orphan bucket.
                let mut closed_by_wt: HashMap<String, Vec<ClosedSessionRow>> = HashMap::new();
                for s in &p.sessions {
                    if s.closed_at.is_none() {
                        continue;
                    }
                    let key = s.worktree_id.clone().unwrap_or_default();
                    // History rows show the agent type that was
                    // running — "Claude Code", "Copilot CLI",
                    // "OpenCode", "Pi", "Codex", or "shell" for a
                    // plain pty tab. Custom `display_name` (set by
                    // explicit rename) still wins so a user-chosen
                    // label survives, but the bare session UUID is
                    // never the visible label anymore.
                    let agent_label = s
                        .agent_id
                        .as_deref()
                        .and_then(history_agent_display_name);
                    let label: SharedString = s
                        .display_name
                        .clone()
                        .or_else(|| agent_label.map(|s| s.to_string()))
                        .unwrap_or_else(|| "shell".to_string())
                        .into();
                    closed_by_wt
                        .entry(key)
                        .or_default()
                        .push(ClosedSessionRow {
                            session_id: s.id.clone(),
                            label,
                            closed_at: s.closed_at.clone(),
                        });
                }
                // Newest-first by `closed_at` — mirrors
                // `SessionManager::closed` so the sidebar disclosure
                // shows the most recently closed row at the top.
                for bucket in closed_by_wt.values_mut() {
                    bucket.sort_by(|a, b| {
                        let ka = a
                            .closed_at
                            .as_deref()
                            .and_then(codescope_core::time::parse_iso8601_secs);
                        let kb = b
                            .closed_at
                            .as_deref()
                            .and_then(codescope_core::time::parse_iso8601_secs);
                        kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
                    });
                }
                let mut worktrees: Vec<WorktreeRowData> = p
                    .worktrees
                    .iter()
                    .map(|wt| {
                        let closed_sessions = closed_by_wt.remove(&wt.id).unwrap_or_default();
                        WorktreeRowData {
                            id: wt.id.clone(),
                            canonical_path:
                                codescope_core::path_canon::canonicalize_path(&wt.path),
                            path: wt.path.clone(),
                            branch: wt.branch.clone(),
                            closed_sessions,
                        }
                    })
                    .collect();
                // Orphan / stale-worktree closed sessions — rows
                // whose `worktree_id` is `None` (collapses to "")
                // or refers to a worktree that no longer exists in
                // `p.worktrees` would otherwise drop off the UI
                // entirely (Copilot review on PR #127). Fold them
                // into the primary worktree's bucket so the user
                // can still reopen them; re-sort so the merged set
                // stays newest-first.
                if !closed_by_wt.is_empty() {
                    let leftover: Vec<ClosedSessionRow> = closed_by_wt
                        .into_values()
                        .flatten()
                        .collect();
                    // Resolve the target index in two steps so the
                    // primary lookup and the `first_mut` fallback
                    // don't both hold a `&mut` to `worktrees`.
                    let target_idx = worktrees
                        .iter()
                        .position(|w| w.id == "primary")
                        .or_else(|| (!worktrees.is_empty()).then_some(0));
                    if let Some(idx) = target_idx {
                        let target = &mut worktrees[idx];
                        target.closed_sessions.extend(leftover);
                        target.closed_sessions.sort_by(|a, b| {
                            let ka = a
                                .closed_at
                                .as_deref()
                                .and_then(codescope_core::time::parse_iso8601_secs);
                            let kb = b
                                .closed_at
                                .as_deref()
                                .and_then(codescope_core::time::parse_iso8601_secs);
                            kb.partial_cmp(&ka).unwrap_or(std::cmp::Ordering::Equal)
                        });
                    }
                    // If the project has zero worktrees the leftover
                    // rows still vanish — but that's a malformed
                    // projects.json (every project gets a synthetic
                    // primary at migration time), so we don't carve
                    // out a separate orphan disclosure for it.
                }
                (
                    idx,
                    p.id.clone(),
                    SharedString::from(p.name.clone()),
                    p.name.clone(),
                    worktrees,
                )
            })
            .collect();

        // Apply the filter input (case-insensitive substring match).
        // A project is kept when its name matches; an unmatched
        // project is still kept when at least one of its worktrees
        // matches by branch or folder leaf, in which case its
        // worktree list narrows to the matching subset. Empty filter
        // is the identity transform. Mirrors C#
        // `SidebarViewModel.FilterText` filtering — project hits keep
        // every worktree, worktree hits hoist the parent project.
        let rows: Vec<(usize, String, SharedString, String, Vec<WorktreeRowData>)> = {
            let needle = self.filter.trim().to_ascii_lowercase();
            if needle.is_empty() {
                rows
            } else {
                rows.into_iter()
                    .filter_map(|(idx, id, name, project_name, worktrees)| {
                        let project_match =
                            project_name.to_ascii_lowercase().contains(&needle);
                        if project_match {
                            return Some((idx, id, name, project_name, worktrees));
                        }
                        let kept: Vec<WorktreeRowData> = worktrees
                            .into_iter()
                            .filter(|wt| worktree_row_matches(wt, &needle))
                            .collect();
                        if kept.is_empty() {
                            None
                        } else {
                            Some((idx, id, name, project_name, kept))
                        }
                    })
                    .collect()
            }
        };

        let heading = div()
            .h(px(40.0))
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .text_size(px(11.0))
            // C# `SidebarView.xaml` paints PROJECTS with `Text.Faint`
            // (#FF606060), not `Text.Secondary`.
            .text_color(theme::text_faint())
            .child(div().flex_grow().child("PROJECTS"))
            .child(
                div()
                    .id("sidebar-add")
                    .w(px(20.0))
                    .h(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .text_color(theme::ink_ghost(&theme))
                    .cursor_pointer()
                    .hover({
                        let frost = theme::frost_10(&theme);
                        let ink = theme::ink(&theme);
                        move |s| s.bg(frost).text_color(ink)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.open_new_project_dialog(window, cx);
                        }),
                    )
                    .child("+"),
            );

        let empty_state = if self.projects.projects.is_empty() {
            Some(
                div()
                    .px_3()
                    .py_4()
                    // C# `SidebarView.xaml` dashed-box hint uses
                    // `FontSize="11.5"` for its body copy.
                    .text_size(px(11.5))
                    .text_color(theme::ink_ghost(&theme))
                    .child("No projects yet. Click + to add one."),
            )
        } else {
            None
        };

        // Each project expands to one project row + zero or more
        // worktree child rows. We collect a flat `Vec<AnyElement>`
        // rather than a single iterator because the project + child
        // rows are different element shapes and need to be flattened
        // into a single `.children(...)` call.
        let mut project_and_worktree_rows: Vec<gpui::AnyElement> = Vec::new();
        for (idx, id, name, project_name, worktrees) in rows.into_iter() {
            let active = selected == Some(idx);
            // Compute the "any child worktree currently has a busy
            // agent session" propagation flag up-front so the project
            // row (rendered before the child rows) can pull it in.
            // Mirrors C# `ProjectViewModel.HasBusyChild` — surfaces
            // as a small `signal_warn` dot next to the count badge so
            // a collapsed project still tells the user something is
            // running underneath. Uses the per-row cached
            // `canonical_path` (computed once when `rows` was built)
            // so the busy halo's per-frame redraw doesn't keep
            // canonicalising paths from raw strings every tick.
            let any_busy_child = worktrees.iter().any(|wt| {
                !wt.canonical_path.is_empty()
                    && self.busy_paths.contains(&wt.canonical_path)
            });
            // Sidebar row hover / selection fill — `#141414`
            // (Surface.Color.Elev). C# `SidebarView.xaml` hard-codes
            // `#141414` on both `IsMouseOver` and `IsSelected` triggers,
            // so both states resolve to this single colour instead of
            // the `frost_10` overlay we used before.
            let bg = if active {
                theme::surface_elev(&theme)
            } else {
                gpui::transparent_black()
            };
            let rail = if active {
                theme::accent(&theme)
            } else {
                gpui::transparent_black()
            };
            // Project name colour stays `Text.Primary` (white) in C#
            // `SidebarView.xaml` regardless of selection state — only
            // the row background + accent rail switch on active. Mirror
            // that: ink always; ink_dim was a previous Rust-side
            // departure we're rolling back for parity.
            let text_color = theme::ink(&theme);
            let frost_hover = theme::surface_elev(&theme);
            let ink_hover = theme::ink(&theme);

            let collapsed = self.collapsed_projects.contains(&id);
            // Chevron — points right when the project is collapsed,
            // down when expanded. Mirrors the C# `TreeViewItem`
            // chevron template (RotateTransform 0°/90° driven by
            // `IsExpanded`). Click toggles; the click handler stops
            // propagation so the chevron doesn't double-fire `select`.
            let chevron_glyph = if collapsed { "\u{25B8}" } else { "\u{25BE}" };
            let id_for_toggle = id.clone();
            let chevron = div()
                .id(("project-chevron", id_hash(&id)))
                .w(px(14.0))
                .h(px(14.0))
                .mr(px(6.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(10.0))
                // Chevron stroke — `Text.Secondary` (ink_muted) in C#
                // `SidebarView.xaml` (`Stroke="{DynamicResource Text.Secondary}"`).
                .text_color(theme::ink_muted(&theme))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.toggle_project_collapsed(&id_for_toggle, cx);
                    }),
                )
                .child(chevron_glyph);

            let project_row = div()
                .id(("project", id_hash(&id)))
                .h(px(32.0))
                .flex()
                .flex_row()
                .items_center()
                .pr_3()
                .border_l_2()
                .border_color(rail)
                // 4 px row inset; the chevron (14 px wide + 6 px right
                // margin) sits between the rail and the project name.
                // This is a small rightward shift versus the pre-chevron
                // 10 px inset (project name now starts ~14 px further
                // right), matching the C# TreeViewItem template's
                // disclosure indent rather than trying to claw the name
                // back to its old origin.
                .pl(px(4.0))
                .bg(bg)
                .text_color(text_color)
                .text_size(px(13.0))
                .cursor_pointer()
                .hover(move |s| {
                    if active { s } else { s.bg(frost_hover).text_color(ink_hover) }
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.select(idx, cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.open_project_menu(idx, event.position, cx);
                    }),
                )
                .child(chevron)
                .child(div().flex_grow().truncate().child(name));
            // C# `SidebarView.xaml` lines 553-561: collapsed (or just
            // expanded) project surfaces a small `Signal.Warn` (red)
            // dot when any child worktree's adopted session is busy.
            // The C# template gates this on `HasBusyChild` regardless
            // of collapse state — the dot is "attention propagates"
            // signalling, not a collapsed-only affordance.
            let project_row = if any_busy_child {
                project_row.child(
                    div()
                        .ml(px(6.0))
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded_full()
                        .bg(theme::signal_warn()),
                )
            } else {
                project_row
            };
            project_and_worktree_rows.push(project_row.into_any_element());

            // Skip rendering the worktree children + placeholder
            // entirely when the user has collapsed the project.
            // The chevron icon flipping right is the only visible
            // change in that case.
            if collapsed {
                continue;
            }

            // Empty-state placeholder when a project has no non-primary
            // worktrees. Mirrors the `(no worktrees)` row C# renders
            // under an expanded project with empty `Worktrees`. Single
            // dim row, indented to align with the worktree children
            // that *would* live here.
            if worktrees.is_empty() {
                // Stable id keyed off the project id (same hash
                // strategy `project_row` and `wt_row` use) so gpui
                // can track this placeholder across renders without
                // confusing it with a real worktree child when the
                // user adds the project's first worktree.
                let placeholder = div()
                    .id(("worktree-placeholder", id_hash(&id)))
                    .h(px(28.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .pl(px(34.0))
                    .pr_3()
                    .text_color(theme::ink_ghost(&theme))
                    .text_size(px(11.5))
                    .child("(no worktrees)");
                project_and_worktree_rows.push(placeholder.into_any_element());
            }
            // Non-primary worktree rows. Indented to make the parent /
            // child relationship obvious; click emits `OpenSession`
            // which the AppShell catches to spawn a tab pinned to the
            // worktree's path. Right-click opens the worktree context
            // menu (Reveal / Open in WT / Copy path / Remove…).
            for wt in worktrees.into_iter() {
                // Prefer the live branch from `git_status` over the
                // persisted `worktree.branch` — primary worktrees in
                // `projects.json` carry `branch: None` (the migration
                // synthesises them without a branch label) and the
                // git poller fills in the real value within a few
                // seconds of launch. Falling back to the persisted
                // branch then to the folder leaf keeps the row
                // useful while the first poll is still in flight.
                let wt_label: SharedString = self
                    .git_status
                    .get(&wt.path)
                    .map(|g| g.branch.clone())
                    .or_else(|| wt.branch.clone())
                    .unwrap_or_else(|| {
                        std::path::Path::new(&wt.path)
                            .file_name()
                            .and_then(|s| s.to_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| wt.path.clone())
                    })
                    .into();
                let wt_path_for_event = wt.path.clone();
                let project_idx_for_menu = idx;
                // The right-click handler captures the worktree id by
                // clone — looking up by id at action time makes the
                // menu robust against `projects.json` rewrites that
                // shift indices, and avoids the bug where enumerating
                // the *filtered* (non-primary) view here would mis-map
                // to the unfiltered `project.worktrees` later.
                let wt_id_for_menu = wt.id.clone();
                // Single spaces around `·` to match the C# build's
                // `$"{project.Name} · {branch}"` convention in
                // `MainViewModel.RefreshTabTitlesForWorktree`.
                let title_label = SharedString::from(format!(
                    "{} · {}",
                    project_name,
                    wt_label,
                ));
                // Worktree row hover — same `#141414` fill as project rows.
                let frost_hover = theme::surface_elev(&theme);
                let ink_hover = theme::ink(&theme);
                // Resolve agent-state for this worktree's 6 px dot.
                // Mirrors C# `WorktreeViewModel.DotState`:
                //   * `busy` — at least one adopted session is in
                //     `Busy` / `PendingToolUse` → `signal_warn` (red)
                //     plus a pulsing halo behind the dot.
                //   * `idle` — at least one adopted session live but
                //     none busy → `signal_ok` (green).
                //   * `rest` — no live sessions for this path → dim
                //     `#2A2A2A` grey (C# WPF DataTemplate constant;
                //     `ink_ghost` is the closest themeable analogue).
                // Dirty state intentionally does *not* feed the dot
                // colour any more — that lives in the right-aligned
                // `chg` status slug already. The old dirty colouring
                // was a porting mistake; see PR #133 / docs handoff.
                // Reuse the canonical form cached on `WorktreeRowData`
                // — see `any_busy_child` above for why we don't
                // canonicalise here per render frame.
                let wt_canon = wt.canonical_path.as_str();
                let has_active_session =
                    !wt_canon.is_empty() && self.active_paths.contains(wt_canon);
                let has_busy_session =
                    !wt_canon.is_empty() && self.busy_paths.contains(wt_canon);
                let dot_color = if has_busy_session {
                    theme::signal_warn()
                } else if has_active_session {
                    theme::signal_ok()
                } else {
                    theme::ink_ghost(&theme)
                };
                // Element id is keyed off `(project.id, worktree.id)`
                // so primary rows from different projects (which all
                // share `wt.id == "primary"`) don't collide in gpui's
                // id-based element reuse table.
                let wt_row_id = format!("{}/{}", id, wt.id);
                // 2 px accent rail on the left edge when the worktree
                // has a live session. Mirrors C# `SidebarView.xaml`
                // lines 308-334 — the Rectangle in column 0 of the
                // worktree DataTemplate whose Opacity flips to 1 on
                // `HasActiveSession`. There is no worktree-row
                // selection state in the Rust port yet, so the
                // `IsSelected` half of the WPF trigger is a no-op
                // here; if/when that lands, OR it in alongside
                // `has_active_session`.
                let rail_color = if has_active_session {
                    theme::accent(&theme)
                } else {
                    gpui::transparent_black()
                };
                let wt_row = div()
                    .id(("worktree", id_hash(&wt_row_id)))
                    .h(px(28.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .border_l_2()
                    .border_color(rail_color)
                    // 32 px content inset (was 34 before the 2 px rail
                    // was added) so the dot sits at the same column as
                    // before. Border lives outside `pl` in gpui's box
                    // model, so total left offset is still 34 px.
                    .pl(px(32.0))
                    .pr_3()
                    .gap_2()
                    // Branch label default — `Text.Secondary` =
                    // `Fig.Color.InkMuted` (#A6A6A6) per the C# template.
                    // Selected-row override (white + Medium) is a TODO —
                    // tracking row selection lands with #133 follow-up.
                    .text_color(theme::ink_muted(&theme))
                    // Worktree row text size — mirrors `FontSize="11.5"` on
                    // the `DisplayBranch` TextBlock in `SidebarView.xaml`.
                    .text_size(px(11.5))
                    .cursor_pointer()
                    .hover(move |s| s.bg(frost_hover).text_color(ink_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            // Plain row click — focus an existing
                            // tab for this worktree if one is open,
                            // otherwise spawn one. The worktree
                            // context menu's "New Claude session"
                            // row (and the project menu's "New
                            // session" / "New Claude session" rows)
                            // still pass `force_new: true` to always
                            // spawn a fresh tab.
                            cx.emit(SidebarEvent::OpenSession {
                                working_directory: PathBuf::from(&wt_path_for_event),
                                title: title_label.clone(),
                                auto_type: None,
                                force_new: false,
                            });
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener({
                            let id_for_menu = wt_id_for_menu.clone();
                            move |this, event: &MouseDownEvent, _, cx| {
                                this.open_worktree_menu(
                                    project_idx_for_menu,
                                    id_for_menu.clone(),
                                    event.position,
                                    cx,
                                );
                            }
                        }),
                    )
                    // Dot + halo container. The dot is always 6 px;
                    // the halo is a larger ellipse rendered *behind*
                    // it via absolute positioning, animating opacity
                    // from 0.55→0 on a 1.4 s repeat. Mirrors the WPF
                    // Storyboard in `SidebarView.xaml` lines 339-401
                    // — gpui doesn't ship a `ScaleTransform`-style
                    // transform on `div`, so we fix the halo at a
                    // bigger diameter (12 px) and animate opacity
                    // only. Visually it reads the same: a soft red
                    // pulse that radiates and fades. Per-frame
                    // redraws only run while at least one busy row
                    // is on-screen — `AnimationExt::with_animation`
                    // calls `window.request_animation_frame()` only
                    // for the duration of the animation.
                    .child({
                        let dot = div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(dot_color);
                        let container = div()
                            .relative()
                            .w(px(14.0))
                            .h(px(14.0))
                            .flex()
                            .items_center()
                            .justify_center();
                        if has_busy_session {
                            // Halo: 12 px circle, signal_warn, faded.
                            // Absolute-positioned so it overlaps the
                            // 6 px dot rather than displacing it in
                            // the flex row. Animation id is keyed off
                            // the worktree row id so multiple busy
                            // rows animate independently.
                            let halo_id = ("worktree-halo", id_hash(&wt_row_id));
                            let halo = div()
                                .absolute()
                                // Centre the 12 px halo inside the
                                // 14 px container so it overlaps the
                                // 6 px dot in the middle. Without
                                // explicit top/left offsets gpui
                                // defaults to (0,0), pinning the
                                // halo to the corner — Copilot
                                // review on PR #136.
                                .top(px(1.0))
                                .left(px(1.0))
                                .w(px(12.0))
                                .h(px(12.0))
                                .rounded_full()
                                .bg(theme::signal_warn())
                                .with_animation(
                                    halo_id,
                                    Animation::new(Duration::from_millis(1400)).repeat(),
                                    |this, delta| {
                                        // Match the WPF keyframe shape:
                                        // 0.0s → 0.55, 1.0s → 0, hold
                                        // until 1.4s. Convert the 0..1
                                        // animation delta accordingly.
                                        let pulse_end = 1.0_f32 / 1.4_f32; // ≈ 0.7143
                                        let opacity = if delta < pulse_end {
                                            0.55_f32 * (1.0_f32 - delta / pulse_end)
                                        } else {
                                            0.0_f32
                                        };
                                        this.opacity(opacity)
                                    },
                                );
                            container.child(halo).child(dot)
                        } else {
                            container.child(dot)
                        }
                    })
                    // Branch label — mono, mirrors `Fig.Font.Mono` on
                    // the `DisplayBranch` TextBlock in
                    // `SidebarView.xaml` (worktree DataTemplate).
                    .child(
                        div()
                            .flex_grow()
                            .truncate()
                            .font(theme::font_mono())
                            .child(wt_label),
                    );
                // Resolve the PR cache for this worktree (matched on
                // the *current* branch, same logic the menu uses) so
                // both the status slug and the badge can read from a
                // single source. `pr_info` is `Some` only when the
                // cached `Resolved.branch` still matches the live one
                // — a branch switch invalidates the cached view until
                // the next poll lands.
                let pr_info: Option<&PullRequestInfo> = {
                    let live_branch = self
                        .git_status
                        .get(&wt.path)
                        .map(|s| s.branch.clone())
                        .or_else(|| wt.branch.clone());
                    match (live_branch.as_ref(), self.pr_urls.get(&wt.path)) {
                        (
                            Some(live),
                            Some(PrLookup::Resolved { branch, info: Some(info) }),
                        ) if branch == live => Some(info),
                        _ => None,
                    }
                };
                let ci_status = pr_info.map(|i| i.ci_status).unwrap_or(CiStatus::None);

                // Right-aligned status slug — `chg` / `↑N ↓N` / `idle`
                // computed by `worktree_status_label_with_ci` from the
                // cached `git_status` snapshot + the PR's CI rollup.
                // Renders the same information the C#
                // `WorktreeViewModel.StatusLabel` slot shows, in
                // `Fig.Font.Mono` at 10 pt with a dim `ink_ghost`
                // foreground. `busy` (active agent) is still TODO —
                // the Rust port has no per-tab session model yet.
                let status_slug = self
                    .git_status
                    .get(&wt.path)
                    .map(|s| {
                        codescope_core::git::worktree_status_label_with_ci(s, ci_status)
                    })
                    .unwrap_or_default();
                let wt_row = if status_slug.is_empty() {
                    wt_row
                } else {
                    wt_row.child(
                        div()
                            .ml(px(8.0))
                            .mr(px(4.0))
                            .text_size(px(10.0))
                            // Status slug — `Text.Faint` (#606060) in
                            // the C# `WorktreeViewModel.StatusLabel`
                            // TextBlock, mono. The `ci!` slug stays in
                            // `text_faint` (matches the C# binding) —
                            // the dedicated CI glyph in the PR badge
                            // is where the failure colour lives.
                            .text_color(theme::text_faint())
                            .font(theme::font_mono())
                            .child(status_slug),
                    )
                };

                // PR badge — `#42 ✓` style, rendered to the right of
                // the status slug. Hidden when no PR is cached for
                // this worktree (lazy fetch in `open_worktree_menu` +
                // 60 s `start_pr_poll` warmup means the badge appears
                // within a minute of the worktree being visible). The
                // CI glyph colour shifts with the rollup:
                //   ✓ success  → `signal_ok`
                //   ✗ failure  → `signal_warn`
                //   ◐ pending  → `text_faint` (no signal colour yet)
                //   ·  none    → `text_faint`
                // Mirrors C# `WorktreeViewModel.PrBadgeText` +
                // `CiGlyph` on the sidebar's `BadgeBox` template.
                let wt_row = if let Some(info) = pr_info {
                    let glyph_color = match info.ci_status {
                        CiStatus::Success => theme::signal_ok(),
                        CiStatus::Failure => theme::signal_warn(),
                        CiStatus::Pending | CiStatus::None => theme::text_faint(),
                    };
                    let glyph: &'static str = match info.ci_status {
                        CiStatus::Success => "\u{2713}", // ✓
                        CiStatus::Pending => "\u{25D0}", // ◐
                        CiStatus::Failure => "\u{2717}", // ✗
                        CiStatus::None => "\u{00B7}",    // ·
                    };
                    let pr_number_label = format!("#{}", info.number);
                    wt_row
                        .child(
                            div()
                                .ml(px(6.0))
                                .text_size(px(10.0))
                                .text_color(theme::text_faint())
                                .font(theme::font_mono())
                                .child(pr_number_label),
                        )
                        .child(
                            div()
                                .ml(px(3.0))
                                .mr(px(4.0))
                                .text_size(px(10.0))
                                .text_color(glyph_color)
                                .font(theme::font_mono())
                                .child(glyph),
                        )
                } else {
                    wt_row
                };
                // History disclosure chevron — only when the worktree
                // has any closed-session rows. Mirrors C# `SidebarView`'s
                // chevron Border which is gated on `HasHistory`.
                // ▸ collapsed, ▾ expanded; click flips
                // `expanded_worktrees`. Hit-target is a 14×14 box so the
                // glyph is comfortable to click without the user having
                // to land on the 8 px arrow itself.
                let wt_key = wt_row_id.clone();
                let has_history = !wt.closed_sessions.is_empty();
                let history_expanded = self.expanded_worktrees.contains(&wt_key);
                let wt_row = if has_history {
                    let glyph = if history_expanded { "\u{25BE}" } else { "\u{25B8}" };
                    let key_for_toggle = wt_key.clone();
                    wt_row.child(
                        div()
                            .id(("worktree-history-chevron", id_hash(&wt_key)))
                            .w(px(14.0))
                            .h(px(14.0))
                            .ml(px(4.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(10.0))
                            // History chevron — `Stroke="{DynamicResource Text.Faint}"`
                            // in `SidebarView.xaml`.
                            .text_color(theme::text_faint())
                            .cursor_pointer()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.toggle_worktree_expanded(&key_for_toggle, cx);
                                }),
                            )
                            .child(glyph),
                    )
                } else {
                    wt_row
                };
                project_and_worktree_rows.push(wt_row.into_any_element());

                // Closed-session history rows. Hidden unless the user
                // has expanded this worktree's chevron. Each row is dim
                // (mirrors C#'s `Opacity=0.55` data template) with a
                // hollow outline dot, the session label in mono, and a
                // right-aligned relative-time stamp. Click emits
                // `ReopenSession` so AppShell can run
                // `SessionManager::reopen` and spawn a fresh tab.
                if has_history && history_expanded {
                    let now_iso = codescope_core::session::now_iso8601();
                    // Outline dot stroke + timestamp share `Text.Faint`
                    // in the C# `SessionTabViewModel` history template
                    // (`Stroke="{DynamicResource Text.Faint}"` and
                    // `Foreground="{DynamicResource Text.Faint}"`).
                    // Label foreground is `Text.Secondary` (ink_muted).
                    let outline_color = theme::text_faint();
                    let label_color = theme::ink_muted(&theme);
                    let ts_color = theme::text_faint();
                    // History row hover — same `#141414` fill as
                    // every other row in the sidebar tree.
                    let frost_hover = theme::surface_elev(&theme);
                    let ink_hover = theme::ink(&theme);
                    for row in wt.closed_sessions.into_iter() {
                        let session_id = row.session_id.clone();
                        let id_for_click = session_id.clone();
                        let relative = codescope_core::session::format_closed_at_relative(
                            row.closed_at.as_deref(),
                            &now_iso,
                        );
                        let history_id = format!("{}/{}", wt_key, session_id);
                        let history_row = div()
                            .id(("history", id_hash(&history_id)))
                            .h(px(24.0))
                            .flex()
                            .flex_row()
                            .items_center()
                            .pl(px(46.0)) // indent under the parent worktree's branch label
                            .pr_3()
                            .gap_2()
                            .text_size(px(11.0))
                            .cursor_pointer()
                            .hover(move |s| s.bg(frost_hover).text_color(ink_hover))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |_, _, _, cx| {
                                    cx.emit(SidebarEvent::ReopenSession {
                                        session_id: id_for_click.clone(),
                                    });
                                }),
                            )
                            // Hollow outline dot — `border_1` + transparent
                            // bg matches the WPF `Stroke=Text.Faint`
                            // ellipse on the closed-row template, marking
                            // the row as inactive at a glance.
                            .child(
                                div()
                                    .w(px(6.0))
                                    .h(px(6.0))
                                    .rounded_full()
                                    .border_1()
                                    .border_color(outline_color),
                            )
                            // Dim label — `Opacity=0.55` on the C#
                            // history DataTemplate, approximated here
                            // by `text_color = ink_dim`. No
                            // strikethrough in the WPF reference, so
                            // we don't add one.
                            .child(
                                div()
                                    .flex_grow()
                                    .truncate()
                                    .font(theme::font_mono())
                                    .text_color(label_color)
                                    .child(row.label.clone()),
                            )
                            .child(
                                div()
                                    .ml(px(8.0))
                                    .text_size(px(10.0))
                                    .text_color(ts_color)
                                    .font(theme::font_mono())
                                    .child(relative),
                            );
                        project_and_worktree_rows.push(history_row.into_any_element());
                    }
                }
            }
        }

        // `flex_grow` + `min_h(0)` + `overflow_y_scroll` so the
        // project/worktree tree absorbs remaining height in the
        // sidebar's flex column and clips + scrolls instead of
        // pushing the footer off the bottom edge with a long
        // project list. Same pattern AppShell's notifications panel
        // uses; without `min_h(0)` flex children default to their
        // content's intrinsic size and the column overflows.
        let mut body = div()
            .id("sidebar-body")
            .flex()
            .flex_col()
            .flex_grow()
            .overflow_y_scroll()
            .py_1()
            .children(project_and_worktree_rows);
        body.style().min_size.height = Some(gpui::Length::Definite(px(0.0).into()));
        if let Some(es) = empty_state {
            body = body.child(es);
        }

        // ── Footer: Overview + New Project, stacked, separated from
        //    the tree by a 1 px divider. Mirrors the bottom block in
        //    `src/CodeScope.Ui/Views/SidebarView.xaml`:
        //
        //    <Border BorderThickness="0,1,0,0" Padding="8">
        //      <StackPanel>
        //        <Button (Sidebar.OverviewButton, h=36)>
        //          <Grid> Overview | ⌃⇧O keycap </Grid>
        //        </Button>
        //        <Button (Sidebar.NewProjectButton, h=40, mt=6)
        //                BorderThickness="2,0,0,0" /* accent rail */>
        //          <StackPanel> + | New Project </StackPanel>
        //        </Button>
        //      </StackPanel>
        //    </Border>
        //
        //    Overview: emits `SidebarEvent::OpenOverview`. The C#
        //    Overview view isn't ported yet, so AppShell catches the
        //    event and surfaces a "coming soon" toast — wiring stays
        //    correct for the future port.
        //    New Project: routes to `open_new_project_dialog`, the
        //    same entry point the `+` glyph in the heading uses (PR
        //    #124). The function itself enforces the
        //    `new_project_dialog().is_none() && dialog().is_none()`
        //    gate so both call sites share one source of truth and
        //    neither can stack a second modal on top of an open one.
        let footer = {
            let frost_hover = theme::frost_10(&theme);
            let ink_hover = theme::ink(&theme);
            let elev = theme::elevated(&theme);
            let divider = theme::divider(&theme);
            let ink_dim = theme::ink_dim(&theme);
            let accent = theme::accent(&theme);

            // Overview button — 36 px tall, dim text, mono ⌃⇧O keycap
            // on the right inside a 1 px outlined pill. Mirrors
            // `Sidebar.OverviewButton` in `SidebarView.xaml`. When the
            // Overview panel is on stage (`overview_visible == true`)
            // the row picks up the accent palette (accent foreground,
            // 2 px accent rail on the left edge) — same DataTrigger
            // the C# `IsOverviewVisible` binding fires for the WPF
            // button.
            let overview_active = self.overview_visible;
            let overview_btn = div()
                .id("sidebar-footer-overview")
                .h(px(36.0))
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                .rounded(px(6.0))
                .bg(elev)
                .text_size(px(12.0))
                .when(overview_active, |s| {
                    s.border_l_2()
                        .border_color(accent)
                        .text_color(accent)
                })
                .when(!overview_active, |s| s.text_color(ink_dim))
                .cursor_pointer()
                .hover(move |s| s.bg(frost_hover).text_color(ink_hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _, _, cx| {
                        cx.emit(SidebarEvent::OpenOverview);
                    }),
                )
                .child(div().flex_grow().child("Overview"))
                .child(
                    // Overview keycap — `Foreground="{DynamicResource Text.Faint}"`
                    // on the inner TextBlock in `SidebarView.xaml`,
                    // mono.
                    div()
                        .px(px(5.0))
                        .py(px(1.0))
                        .border_1()
                        .border_color(divider)
                        .rounded(px(3.0))
                        .text_size(px(10.0))
                        .text_color(theme::text_faint())
                        .font(theme::font_mono())
                        .child("⌃⇧ O"),
                );

            // New Project — 40 px tall signature CTA. Flush-left 2 px
            // accent rail, accent-coloured `+` glyph, sans-serif label.
            // Mirrors `Sidebar.NewProjectButton` in `SidebarView.xaml`.
            // `mt(px(6.0))` reproduces the StackPanel's `Margin="0,6,0,0"`
            // gap between the two buttons.
            let new_project_btn = div()
                .id("sidebar-footer-newproject")
                .mt(px(6.0))
                .h(px(40.0))
                .pl(px(12.0))
                .pr(px(12.0))
                .flex()
                .flex_row()
                .items_center()
                .border_l_2()
                .border_color(accent)
                .rounded(px(6.0))
                .bg(elev)
                .text_color(ink_dim)
                .text_size(px(13.0))
                .cursor_pointer()
                .hover(move |s| s.bg(frost_hover).text_color(ink_hover))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| {
                        this.open_new_project_dialog(window, cx);
                    }),
                )
                .child(
                    div()
                        .text_size(px(14.0))
                        .text_color(accent)
                        .font(theme::font_mono())
                        .child("+"),
                )
                .child(
                    div()
                        .ml(px(8.0))
                        .child("New Project"),
                );

            div()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(divider)
                .p_2()
                .child(overview_btn)
                .child(new_project_btn)
        };

        // Filter input — text field above the project tree that
        // hides rows whose project name / worktree branch / folder
        // leaf don't match (case-insensitive substring). Mirrors C#
        // `SidebarViewModel.FilterText`; the empty filter shows every
        // row.
        let filter_input = self.render_filter_input(&theme, window, cx);

        // Width is set by the parent wrapper in `AppShell` so the
        // sidebar can be drag-resized + collapsed at the shell level.
        // We just fill what we're given.
        let mut root = div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::elevated(&theme))
            .border_r_1()
            .border_color(theme::divider(&theme))
            // Mirror the C# build's `Fig.Font.Sans` default for the
            // sidebar — TextElement.FontFamily on `SidebarView` (see
            // `src/CodeScope.Ui/Views/SidebarView.xaml`). Branch labels
            // and status slugs override this with `theme::font_mono()`
            // on their own `div`, matching the per-element
            // `Fig.Font.Mono` overrides on those XAML nodes.
            .font(theme::font_sans())
            // Drag-a-folder onto the sidebar adds it as a project —
            // mirrors C# `SidebarView.xaml.cs::OnDrop`. gpui translates
            // the OS file-drop into an internal drag with an
            // `ExternalPaths` payload; we receive it via `on_drop` and
            // dispatch each existing directory through the same
            // `add_project` entry point the `+ New Project` button uses.
            // Files / non-existent paths are silently skipped — matches
            // C# `PayloadFolders` filtering on `Directory.Exists`.
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                for path in paths.paths() {
                    if path.is_dir() {
                        let path_str = path.to_string_lossy().into_owned();
                        this.add_project(path_str, cx);
                    }
                }
            }))
            .child(heading)
            .child(filter_input)
            .child(div().h_px().bg(theme::divider(&theme)))
            .child(body)
            .child(footer);

        // Build whichever context menu is open (project / worktree /
        // none). Snapshot indices + position up-front so the closure-
        // borrow on `self.menu` doesn't outlast the render helper
        // calls below.
        match self.menu.as_ref() {
            Some(OpenMenu::Project { project_idx, position }) => {
                if let Some(project) = self.projects.projects.get(*project_idx).cloned() {
                    let overlay = self
                        .render_project_menu(*project_idx, *position, &project, &theme, cx)
                        .into_any_element();
                    root = root.child(overlay);
                }
            }
            Some(OpenMenu::Worktree { project_idx, worktree_id, position }) => {
                if let Some(project) = self.projects.projects.get(*project_idx).cloned()
                    && let Some(wt) =
                        project.worktrees.iter().find(|w| &w.id == worktree_id).cloned()
                {
                    let overlay = self
                        .render_worktree_menu(
                            *project_idx,
                            worktree_id.clone(),
                            *position,
                            &project,
                            &wt,
                            &theme,
                            cx,
                        )
                        .into_any_element();
                    root = root.child(overlay);
                }
            }
            None => {}
        }
        if let Some(overlay) = self.render_new_worktree_dialog(window, &theme, cx) {
            root = root.child(overlay);
        }
        if let Some(overlay) = self.render_new_project_dialog(window, &theme, cx) {
            root = root.child(overlay);
        }
        root
    }
}

impl Sidebar {
    /// Render the worktree menu's "New {DisplayName} session" rows —
    /// one per agent in `agent_registry`. Default agent first
    /// (matches C# `BuildAgentChoices`'s "Default" entry pinned to the
    /// top), then the rest in registration order. The default row gets
    /// a subtle accent dot to mirror the C# `Tag = "primary"` styling.
    ///
    /// Each row emits the same `OpenSession` event the legacy single
    /// "New Claude session" row emitted, but with the agent's argv
    /// (joined by spaces) as `auto_type` and a per-agent suffix on the
    /// tab title (` · claude`, ` · codex`, …) so multiple agents on
    /// the same worktree stay distinguishable in the tab strip.
    ///
    /// `id_prefix` is used to derive stable gpui ids per row
    /// (`"{id_prefix}-{agent_id}"`); pick a prefix that's unique
    /// inside its parent menu so multiple invocations on the same
    /// frame don't collide.
    fn build_new_agent_rows(
        &self,
        worktree_path: &str,
        title_prefix: &SharedString,
        id_prefix: &'static str,
        cx: &mut Context<Self>,
    ) -> Vec<gpui::AnyElement> {
        let theme = self.theme.clone();
        let ink = theme::ink(&theme);
        let ink_dim = theme::ink_dim(&theme);
        let frost = theme::frost_10(&theme);
        let accent = theme::accent(&theme);

        // Order: default first, then the rest in registration order.
        let default_id = self.agent_registry.get_default().map(|a| a.id.clone());
        let mut profiles: Vec<&AgentProfile> = Vec::new();
        if let Some(def_id) = default_id.as_deref()
            && let Some(def) = self.agent_registry.get_by_id(def_id)
        {
            profiles.push(def);
        }
        for a in self.agent_registry.get_all() {
            if Some(a.id.as_str()) == default_id.as_deref() {
                continue;
            }
            profiles.push(a);
        }

        profiles
            .into_iter()
            .map(|profile| {
                let is_default = Some(profile.id.as_str()) == default_id.as_deref();
                let label = SharedString::from(format!("New {} session", profile.display_name));
                let row_id = SharedString::from(format!("{id_prefix}-{}", profile.id));
                let path = PathBuf::from(worktree_path);
                let title = SharedString::from(format!(
                    "{} · {}",
                    title_prefix.as_ref(),
                    profile.id,
                ));
                // Join argv with spaces — the receiver runs it through
                // the shell, fine for our built-in profiles (single
                // tokens like `claude`, `codex`, …). Custom agent argv
                // containing spaces would need quoting; the C# build
                // has the same caveat (`string.Join(" ", argv)` in
                // `AgentCommandJoiner`).
                let mut argv: Vec<String> =
                    Vec::with_capacity(1 + profile.new_session_args.len());
                argv.push(profile.command.clone());
                argv.extend(profile.new_session_args.iter().cloned());
                let cmd = argv.join(" ");

                let base_color = if is_default { ink } else { ink_dim };
                let hover_color = ink;
                let frost_hover = frost;

                let mut row = div()
                    .id(row_id)
                    .h(px(28.0))
                    .px_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .text_size(px(12.5))
                    .text_color(base_color)
                    .cursor_pointer()
                    .hover(move |s| s.bg(frost_hover).text_color(hover_color))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            cx.emit(SidebarEvent::OpenSession {
                                working_directory: path.clone(),
                                title: title.clone(),
                                auto_type: Some(cmd.clone().into()),
                                force_new: true,
                            });
                            this.close_menu(cx);
                        }),
                    )
                    .child(div().flex_grow().child(label));
                if is_default {
                    // Accent dot — subtle marker for the default agent,
                    // mirrors C# `Tag = "primary"` rendering hint.
                    row = row.child(
                        div()
                            .ml(px(6.0))
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(accent),
                    );
                }
                row.into_any_element()
            })
            .collect()
    }

    /// The filter text box that lives between the "PROJECTS" header
    /// and the project tree. Single-line, in-place editing — click to
    /// focus, then characters / backspace / escape edit the value.
    /// Esc clears the filter (mirrors the C# behaviour of the
    /// `FilterText` clear button). Empty filter renders dim
    /// placeholder text.
    fn render_filter_input(
        &self,
        theme: &Arc<Theme>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let focused = self.filter_focus.is_focused(window);
        let border_color = if focused { theme::accent(theme) } else { divider };

        let (value_text, text_color): (SharedString, _) = if self.filter.is_empty() {
            ("Filter…".into(), ink_ghost)
        } else {
            (SharedString::from(self.filter.clone()), ink)
        };

        let filter_focus = self.filter_focus.clone();
        div()
            .id("sidebar-filter")
            .mx_2()
            .my_1()
            .h(px(24.0))
            .px(px(8.0))
            .flex()
            .flex_row()
            .items_center()
            .rounded(px(4.0))
            .bg(elevated)
            .border_1()
            .border_color(border_color)
            .text_size(px(11.5))
            .text_color(text_color)
            .cursor_pointer()
            .track_focus(&self.filter_focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |_, _, window, cx| {
                    filter_focus.focus(window);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
            .on_key_down(cx.listener(handle_filter_key_down))
            .child(value_text)
    }

    /// Build the floating project context menu. Anchored to the
    /// click position and `deferred` so it paints over the rest of
    /// the chrome instead of being clipped by the sidebar's bounds.
    /// Click outside (anywhere in the window) dismisses via
    /// `on_mouse_down_out`.
    fn render_project_menu(
        &self,
        idx: usize,
        position: Point<Pixels>,
        project: &Project,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let header_label: SharedString = project.name.clone().into();
        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let frost = theme::frost_10(theme);
        let danger = theme::danger(theme);

        let item = |id: &'static str,
                    label: &'static str,
                    danger_row: bool,
                    on_click: MenuItemAction|
         -> gpui::Stateful<gpui::Div> {
            let base_color = if danger_row { danger } else { ink_dim };
            let hover_color = if danger_row { danger } else { ink };
            let frost_hover = frost;
            div()
                .id(id)
                .h(px(28.0))
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                // Context-menu items: `FontSize="12.5"` (Fig.Font.Sans)
                // per `ContextMenuStyles.xaml` default MenuItem style.
                .text_size(px(12.5))
                .text_color(base_color)
                .cursor_pointer()
                .hover(move |s| s.bg(frost_hover).text_color(hover_color))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        on_click(this, window, cx);
                    }),
                )
                .child(label)
        };

        let menu_body = div()
            .flex()
            .flex_col()
            .py_1()
            .min_w(px(220.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_md()
            .shadow_lg()
            // Default MenuItem `FontFamily="Fig.Font.Sans"` from
            // `ContextMenuStyles.xaml`. Per-row overrides (mono title
            // in the header) re-apply `font_mono` themselves.
            .font(theme::font_sans())
            // Header — non-interactive, mirrors the C# `BuildContextHeader`
            // (`ContextMenuFactory.cs`): mono title at 11 px on the
            // first line, sans subtitle at 10 px on the second.
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(10.0))
                    .text_color(ink_ghost)
                    .child(
                        div()
                            .text_color(ink)
                            .font(theme::font_mono())
                            .text_size(px(11.0))
                            .truncate()
                            .child(header_label),
                    )
                    .child(div().child("project")),
            )
            .child(div().h_px().bg(divider).my_1())
            // "New session" rows fire `OpenSession` on the project's
            // primary worktree path. Project name doubles as the tab
            // title — the user can rename later. Two flavours:
            //   • Plain "New session" — opens a shell at the project
            //     root, no auto-typed command.
            //   • "New Claude session" — opens a shell, then types
            //     `claude` after the prompt is up. Mirrors the C#
            //     build's `BuildAgentChoices` flow at the project
            //     scope; we don't have the full agent picker yet so
            //     this is the headline shortcut.
            .child({
                let project_path = project.path.clone();
                let project_title: SharedString = project.name.clone().into();
                item(
                    "menu-new-session",
                    "New session",
                    false,
                    Box::new(move |this, _window, cx| {
                        cx.emit(SidebarEvent::OpenSession {
                            working_directory: PathBuf::from(&project_path),
                            title: project_title.clone(),
                            auto_type: None,
                            force_new: true,
                        });
                        this.close_menu(cx);
                    }),
                )
            })
            .child({
                let project_path = project.path.clone();
                let project_title: SharedString = project.name.clone().into();
                item(
                    "menu-new-claude",
                    "New Claude session",
                    false,
                    Box::new(move |this, _window, cx| {
                        cx.emit(SidebarEvent::OpenSession {
                            working_directory: PathBuf::from(&project_path),
                            title: project_title.clone(),
                            auto_type: Some(claude_command().into()),
                            force_new: true,
                        });
                        this.close_menu(cx);
                    }),
                )
            })
            .child(div().h_px().bg(divider).my_1())
            .child(item(
                "menu-new-worktree",
                "New worktree from branch…",
                false,
                Box::new(move |this, window, cx| {
                    this.open_new_worktree_dialog(idx, window, cx);
                }),
            ))
            // ── Git ─────────────────────────────────────────────
            // Fetch + Open remote. Mirrors C# BuildProjectMenu
            // Git section. "Set default agent" submenu lands when
            // we have a submenu primitive.
            .child(div().h_px().bg(divider).my_1())
            .child(item(
                "menu-fetch-all",
                "Fetch all (prune)",
                false,
                Box::new(move |this, _window, cx| this.fetch_all_for_project(idx, cx)),
            ))
            .child(item(
                "menu-open-remote",
                "Open remote in browser",
                false,
                Box::new(move |this, _window, cx| {
                    this.open_project_remote_in_browser(idx, cx);
                }),
            ))
            // ── Reveal ──────────────────────────────────────────
            .child(div().h_px().bg(divider).my_1())
            .child(item(
                "menu-reveal",
                reveal_in_file_browser_label(),
                false,
                Box::new(move |this, _window, cx| this.reveal_in_explorer(idx, cx)),
            ))
            // "Open in Windows Terminal" is genuinely Windows-only —
            // `wt.exe` doesn't exist on macOS / Linux. Hide the row
            // entirely on other platforms instead of shipping a
            // misleading no-op. `.children(Option<_>)` yields 0 or 1
            // child without splitting the chain.
            .children(cfg!(target_os = "windows").then(|| {
                item(
                    "menu-wt",
                    "Open in Windows Terminal",
                    false,
                    Box::new(move |this, _window, cx| this.open_in_windows_terminal(idx, cx)),
                )
            }))
            .child(item(
                "menu-copy-path",
                "Copy path",
                false,
                Box::new(move |this, _window, cx| this.copy_path(idx, cx)),
            ))
            .child(div().h_px().bg(divider).my_1())
            .child(item(
                "menu-remove",
                "Remove project",
                true,
                Box::new(move |this, _window, cx| this.remove_project(idx, cx)),
            ))
            // Click on the menu itself shouldn't bubble out and trigger
            // the dismiss handler we install below.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)));

        // `deferred` paints the menu after the rest of the frame so it
        // overlays the tab strip / terminal area instead of being
        // clipped to the 240 px sidebar column. `anchored` snaps it
        // to a window edge if the click happens close to one.
        deferred(
            anchored()
                .position(point(position.x, position.y))
                .anchor(Corner::TopLeft)
                .snap_to_window_with_margin(px(8.0))
                .child(menu_body),
        )
    }

    /// Worktree row context menu — mirrors the C# `BuildWorktreeMenu`
    /// in scope minus the git/PR rows that depend on subsystems we
    /// haven't ported yet (rebase, pull, PR detection, dirty-state
    /// dot). Today: Open session (default), Reveal, Open in WT,
    /// Copy path, Remove worktree…
    #[allow(clippy::too_many_arguments)]
    fn render_worktree_menu(
        &self,
        project_idx: usize,
        worktree_id: String,
        position: Point<Pixels>,
        project: &Project,
        worktree: &codescope_core::projects::Worktree,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Header: branch (or folder leaf when branch is unknown)
        // dimmed, project name as the qualifier on the second line —
        // mirrors the C# header pattern (label + scope tag).
        let branch_label: SharedString = worktree
            .branch
            .clone()
            .unwrap_or_else(|| {
                std::path::Path::new(&worktree.path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| worktree.path.clone())
            })
            .into();
        let project_label: SharedString = project.name.clone().into();
        // Snapshot the open-session payload so the menu row's listener
        // can `move` the values without keeping a borrow on `project`.
        let open_session_path = PathBuf::from(&worktree.path);
        let open_session_title = SharedString::from(format!(
            "{} · {}",
            project.name, branch_label
        ));
        let is_primary = worktree.is_primary;

        // Precompute the multi-agent "New … session" rows up-front so
        // we don't try to re-borrow `cx` mutably inside the menu_body
        // chain (the `item` closure below captures `cx.listener`,
        // which holds an immutable borrow). Builds Vec<AnyElement>;
        // dropped straight into `.children(...)` later.
        let agent_rows = self.build_new_agent_rows(
            &worktree.path,
            &open_session_title,
            "wt-menu-new-agent",
            cx,
        );

        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let frost = theme::frost_10(theme);
        let danger = theme::danger(theme);

        let item = |id: &'static str,
                    label: &'static str,
                    danger_row: bool,
                    on_click: MenuItemAction|
         -> gpui::Stateful<gpui::Div> {
            let base_color = if danger_row { danger } else { ink_dim };
            let hover_color = if danger_row { danger } else { ink };
            let frost_hover = frost;
            div()
                .id(id)
                .h(px(28.0))
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                // Context-menu items: `FontSize="12.5"` (Fig.Font.Sans)
                // per `ContextMenuStyles.xaml` default MenuItem style.
                .text_size(px(12.5))
                .text_color(base_color)
                .cursor_pointer()
                .hover(move |s| s.bg(frost_hover).text_color(hover_color))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        on_click(this, window, cx);
                    }),
                )
                .child(label)
        };

        let menu_body = div()
            .flex()
            .flex_col()
            .py_1()
            .min_w(px(240.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_md()
            .shadow_lg()
            // Same default MenuItem sans family as the project menu.
            .font(theme::font_sans())
            .child(
                // Same `BuildContextHeader` layout as the project menu:
                // mono title @ 11 px (branch / leaf), 10 px subtitle.
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(10.0))
                    .text_color(ink_ghost)
                    .child(
                        div()
                            .text_color(ink)
                            .font(theme::font_mono())
                            .text_size(px(11.0))
                            .truncate()
                            .child(branch_label),
                    )
                    .child(div().truncate().child(project_label)),
            )
            .child(div().h_px().bg(divider).my_1())
            .child({
                let path_for_open = open_session_path.clone();
                let title_for_open = open_session_title.clone();
                item(
                    "wt-menu-open",
                    "Open session",
                    false,
                    Box::new(move |this, _window, cx| {
                        // The worktree menu's "Open session" row is a
                        // synonym for clicking the row itself —
                        // focus-or-open rather than always-spawn —
                        // so a user already running a session for
                        // this worktree gets routed to it instead of
                        // accidentally piling up duplicates.
                        cx.emit(SidebarEvent::OpenSession {
                            working_directory: path_for_open.clone(),
                            title: title_for_open.clone(),
                            auto_type: None,
                            force_new: false,
                        });
                        this.close_menu(cx);
                    }),
                )
            })
            // Multi-agent "New {DisplayName} session" rows — one per
            // profile in `agent_registry`. The default profile lands
            // first (matches the C# `BuildAgentChoices` "Default" entry
            // sitting at the top of the picker) and gets an accent dot.
            // Mirrors `SidebarView.xaml.cs::BuildAgentChoices` minus
            // the Shell sentinel and the global-default fallback —
            // those land when the Settings dialog grows agent-picker
            // UI.
            //
            // Title derives from `open_session_title`
            // (`{project} · {branch}`) plus a per-agent suffix so
            // multiple worktrees stay distinguishable in the tab strip
            // when the user has agents running in several of them.
            .children(agent_rows)
            // ── Git ─────────────────────────────────────────────
            // Pull / Copy branch / Open remote in browser. The
            // dirty-state aware Rebase + Discard rows from the C#
            // build land when the worktree polling infra does;
            // these three are stateless enough to ship now.
            .child(div().h_px().bg(divider).my_1())
            .child({
                let id_for_pull = worktree_id.clone();
                item(
                    "wt-menu-pull",
                    "Pull (fast-forward)",
                    false,
                    Box::new(move |this, _window, cx| {
                        this.pull_worktree(project_idx, &id_for_pull, cx);
                    }),
                )
            })
            // "Rebase onto origin/<default>" — mirrors the C#
            // `RebaseOntoDefaultCommand` row. Visible only when the
            // worktree is currently checked out on a named branch
            // that isn't the project default: detached HEAD
            // (`branch is None`) would fail the rebase outright,
            // and rebasing `main` onto `origin/main` is just a
            // fancy pull. The C# build also confirms via a prompt
            // before rebasing — we ship the simpler immediate form
            // here, so no trailing ellipsis on the label (which
            // elsewhere implies a follow-up dialog). Plumbing a
            // `Window` into the handler so we can prompt is a
            // separate follow-up.
            //
            // Built inline rather than through the `item` helper
            // because the label is dynamic (the default branch can
            // shift via `projects.json` edits) and the helper
            // takes a `&'static str` to avoid per-render
            // allocations on the more common rows.
            .children({
                let show_rebase = worktree
                    .branch
                    .as_ref()
                    .is_some_and(|b| b != &project.default_branch);
                show_rebase.then(|| {
                    let id_for_rebase = worktree_id.clone();
                    let label: SharedString = format!(
                        "Rebase onto origin/{}",
                        project.default_branch
                    )
                    .into();
                    let frost_hover = frost;
                    div()
                        .id("wt-menu-rebase-default")
                        .h(px(28.0))
                        .px_3()
                        .flex()
                        .flex_row()
                        .items_center()
                        // Same 12.5 px parity as the `item` helper above.
                        .text_size(px(12.5))
                        .text_color(ink_dim)
                        .cursor_pointer()
                        .hover(move |s| s.bg(frost_hover).text_color(ink))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.rebase_worktree_onto_default(
                                    project_idx,
                                    &id_for_rebase,
                                    cx,
                                );
                            }),
                        )
                        .child(label)
                })
            })
            .children(worktree.branch.is_some().then(|| {
                let id_for_copy_branch = worktree_id.clone();
                item(
                    "wt-menu-copy-branch",
                    "Copy branch name",
                    false,
                    Box::new(move |this, _window, cx| {
                        this.copy_worktree_branch(project_idx, &id_for_copy_branch, cx);
                    }),
                )
            }))
            // "Open PR in browser" + "Copy PR URL" — only when the
            // cached `gh pr list` lookup resolved to an open PR for
            // this worktree's *current* branch (live `git_status`
            // first, persisted `branch` fallback — same logic
            // `open_worktree_menu` uses to decide whether to refetch).
            // The fetch is kicked off in `open_worktree_menu` (lazy)
            // and warmed by the 60 s `start_pr_poll` loop; until
            // either lands the rows stay hidden so a half-loaded menu
            // doesn't flash unusable entries. A stale `Resolved` from
            // a prior branch is also hidden until the refetch lands.
            // Mirrors C#'s `wt.HasPullRequest`-gated
            // `OpenPullRequestCommand` + `CopyPullRequestUrlCommand`
            // pair.
            .children({
                let live_branch = self
                    .git_status
                    .get(&worktree.path)
                    .map(|s| s.branch.clone())
                    .or_else(|| worktree.branch.clone());
                let has_pr = live_branch
                    .as_ref()
                    .map(|live| {
                        matches!(
                            self.pr_urls.get(&worktree.path),
                            Some(PrLookup::Resolved { branch, info: Some(_) })
                                if branch == live
                        )
                    })
                    .unwrap_or(false);
                let mut rows: Vec<gpui::AnyElement> = Vec::new();
                if has_pr {
                    let id_for_open_pr = worktree_id.clone();
                    rows.push(
                        item(
                            "wt-menu-open-pr",
                            "Open PR in browser",
                            false,
                            Box::new(move |this, _window, cx| {
                                this.open_worktree_pr_in_browser(
                                    project_idx,
                                    &id_for_open_pr,
                                    cx,
                                );
                            }),
                        )
                        .into_any_element(),
                    );
                    let id_for_copy_pr = worktree_id.clone();
                    rows.push(
                        item(
                            "wt-menu-copy-pr-url",
                            "Copy PR URL",
                            false,
                            Box::new(move |this, _window, cx| {
                                this.copy_worktree_pr_url(project_idx, &id_for_copy_pr, cx);
                            }),
                        )
                        .into_any_element(),
                    );
                }
                rows
            })
            .child({
                let id_for_remote = worktree_id.clone();
                item(
                    "wt-menu-open-remote",
                    "Open remote in browser",
                    false,
                    Box::new(move |this, _window, cx| {
                        this.open_worktree_remote_in_browser(project_idx, &id_for_remote, cx);
                    }),
                )
            })
            // "Discard changes…" — only surface when the dirty
            // poller has flagged this worktree as having changes;
            // for clean / unknown worktrees the action would be a
            // no-op + scary prompt, so hide the row entirely.
            .children(
                self.dirty_state
                    .get(&worktree.path)
                    .copied()
                    .unwrap_or(false)
                    .then(|| {
                        let id_for_discard = worktree_id.clone();
                        item(
                            "wt-menu-discard",
                            "Discard changes…",
                            true,
                            Box::new(move |this, window, cx| {
                                this.discard_worktree_changes(
                                    project_idx,
                                    &id_for_discard,
                                    window,
                                    cx,
                                );
                            }),
                        )
                    }),
            )
            // ── Reveal ──────────────────────────────────────────
            .child(div().h_px().bg(divider).my_1())
            .child({
                let id_for_reveal = worktree_id.clone();
                item(
                    "wt-menu-reveal",
                    reveal_in_file_browser_label(),
                    false,
                    Box::new(move |this, _window, cx| {
                        this.reveal_worktree_in_explorer(project_idx, &id_for_reveal, cx);
                    }),
                )
            })
            .children(cfg!(target_os = "windows").then(|| {
                let id_for_wt = worktree_id.clone();
                item(
                    "wt-menu-wt",
                    "Open in Windows Terminal",
                    false,
                    Box::new(move |this, _window, cx| {
                        this.open_worktree_in_windows_terminal(project_idx, &id_for_wt, cx);
                    }),
                )
            }))
            .child({
                let id_for_copy = worktree_id.clone();
                item(
                    "wt-menu-copy-path",
                    "Copy path",
                    false,
                    Box::new(move |this, _window, cx| {
                        this.copy_worktree_path(project_idx, &id_for_copy, cx);
                    }),
                )
            })
            // Primary worktrees are tracked by the project row itself —
            // there's nothing to "remove" without removing the project
            // (which has its own destructive flow). Hide the row
            // entirely on the primary so the menu stays a list of
            // things that actually do something.
            .children((!is_primary).then(|| {
                let id_for_remove = worktree_id.clone();
                div().child(div().h_px().bg(divider).my_1()).child(item(
                    "wt-menu-remove",
                    "Remove worktree…",
                    true,
                    Box::new(move |this, window, cx| {
                        this.remove_worktree(project_idx, &id_for_remove, window, cx);
                    }),
                ))
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)));

        deferred(
            anchored()
                .position(point(position.x, position.y))
                .anchor(Corner::TopLeft)
                .snap_to_window_with_margin(px(8.0))
                .child(menu_body),
        )
    }
}

/// Snapshot of everything `remove_worktree` needs after the user
/// confirms the prompt. Captured up front so the async task that runs
/// the `git worktree remove` (and the follow-up `projects.json`
/// rewrite) doesn't have to re-borrow the sidebar — by the time it
/// resumes, indices may have shifted under it.
struct WorktreeRemoveContext {
    project_id: String,
    worktree_id: String,
    project_path: String,
    worktree_path: String,
    /// Branch (or folder leaf) shown in toasts/error messages.
    display_label: String,
}

/// Run the post-confirm `git worktree remove` flow. Lives outside the
/// `Sidebar` impl because `cx.spawn_in` hands us an `AsyncApp` and a
/// `WeakEntity<Sidebar>`, not a `&mut Sidebar` — wrapping the whole
/// thing in a free async function keeps the borrow shape obvious.
async fn run_remove_worktree_flow(
    this: gpui::WeakEntity<Sidebar>,
    ctx: WorktreeRemoveContext,
    cx: &mut gpui::AsyncWindowContext,
) {
    use codescope_core::git;

    let repo = std::path::PathBuf::from(&ctx.project_path);
    let wt_path = std::path::PathBuf::from(&ctx.worktree_path);

    // First attempt without --force, mirroring the C# flow. The
    // background_executor lets us off the UI thread while git runs.
    let first_attempt = {
        let repo = repo.clone();
        let wt_path = wt_path.clone();
        cx.background_spawn(async move { git::remove_worktree(&repo, &wt_path, false) })
            .await
    };

    let needs_force = match first_attempt {
        Ok(()) => false,
        Err(err) => {
            // Retry-with-force prompt. Same wording shape as the C#
            // dialog: show the underlying git error so the user sees
            // *why* the normal remove failed before deciding to force.
            let prompt_msg = "Couldn't remove worktree — force?".to_string();
            let detail = format!(
                "{}\n\nForce remove will discard uncommitted changes and untracked files in the worktree.",
                err
            );
            let receiver = match this.update_in(cx, |_this, window, cx| {
                window.prompt(
                    gpui::PromptLevel::Warning,
                    &prompt_msg,
                    Some(&detail),
                    &["Force remove", "Cancel"],
                    cx,
                )
            }) {
                Ok(rx) => rx,
                Err(_) => return,
            };
            match receiver.await {
                Ok(0) => true,
                _ => return,
            }
        }
    };

    if needs_force {
        let repo = repo.clone();
        let wt_path = wt_path.clone();
        let forced = cx
            .background_spawn(async move { git::remove_worktree(&repo, &wt_path, true) })
            .await;
        if let Err(err) = forced {
            eprintln!(
                "warning: force-remove of worktree '{}' failed: {err:#}",
                ctx.display_label
            );
            return;
        }
    }

    // Git is happy; rewrite `projects.json` to drop the row. Match by
    // (project id, worktree id) so a concurrent edit that reordered
    // the list still hits the right row. If the project / worktree is
    // already gone (e.g. another window beat us to it), the rewrite
    // is a no-op and we still return success.
    let _ = this.update(cx, |this, cx| {
        let mut next = this.projects.clone();
        let Some(project) = next.projects.iter_mut().find(|p| p.id == ctx.project_id) else {
            return;
        };
        let before = project.worktrees.len();
        project.worktrees.retain(|wt| wt.id != ctx.worktree_id);
        if project.worktrees.len() == before {
            // Nothing to persist — the row was already gone.
            return;
        }
        if let Err(err) = next.save(this.paths.as_ref()) {
            eprintln!("warning: failed to save projects.json after worktree removal: {err:#}");
            return;
        }
        this.projects = next;
        cx.notify();
    });

    // Note: `git worktree remove` already deletes the worktree
    // directory on success. If the directory lingers (rare, typically
    // antivirus holding a handle), it stays as a flat empty folder the
    // user can remove manually. We deliberately don't retry on the
    // Rust side — the C# build's `RemoveWorktreeResidualDirPrefix`
    // path also just surfaces the message rather than attempting
    // another delete from app code.
}

/// OS-spawn shared by project + worktree "Reveal" rows. Detached so a
/// slow shell extension can't stall the UI thread, and we ignore exit
/// status since the user sees the result on their desktop.
fn reveal_path_in_file_browser(path: &str) {
    #[cfg(target_os = "windows")]
    let result = Command::new("explorer.exe").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(path).spawn();
    if let Err(err) = result {
        eprintln!("warning: failed to reveal {path}: {err:#}");
    }
}

/// Resolve `remote.origin.url` for `repo`, normalise it to a
/// browser URL, and open it via the OS handler. Shared by the
/// project menu and the worktree menu — both ultimately read
/// `origin` from the project's primary repo (worktrees inherit
/// via their `gitdir:` pointer). No-op + log when there's no
/// origin or the URL shape isn't recognised.
async fn spawn_open_remote_in_browser(
    repo: std::path::PathBuf,
    cx: &mut gpui::AsyncApp,
) {
    let url_result = cx
        .background_spawn(async move { codescope_core::git::remote_origin_url(&repo) })
        .await;
    let url = match url_result {
        Ok(Some(u)) => u,
        Ok(None) => {
            eprintln!("info: no remote.origin.url configured");
            return;
        }
        Err(err) => {
            eprintln!("warning: failed to read remote.origin.url: {err:#}");
            return;
        }
    };
    let browser_url = match codescope_core::git::remote_url_to_browser(&url) {
        Some(u) => u,
        None => {
            eprintln!("info: remote URL not a recognised browser shape: {url}");
            return;
        }
    };
    open_url_in_browser(&browser_url);
}

/// Open an HTTP(S) URL in the user's default browser. On Windows
/// we route through `ShellExecuteW` (via `win32_titlebar::shell_open_url`)
/// rather than `cmd /C start` — the latter would let any `&` / `|`
/// in the URL be interpreted as command separators by `cmd.exe`,
/// which is a command-injection risk for a URL we got out of
/// `git config`. macOS uses `open`, Linux uses `xdg-open`; both
/// pass arguments verbatim without shell parsing.
fn open_url_in_browser(url: &str) {
    #[cfg(target_os = "windows")]
    {
        crate::win32_titlebar::shell_open_url(url);
    }
    #[cfg(target_os = "macos")]
    {
        if let Err(err) = Command::new("open").arg(url).spawn() {
            eprintln!("warning: failed to open URL in browser: {err:#}");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Err(err) = Command::new("xdg-open").arg(url).spawn() {
            eprintln!("warning: failed to open URL in browser: {err:#}");
        }
    }
}

/// `wt -d <path>` — Windows-only. Logs a friendly warning on other
/// platforms; the menu row is hidden there but the helper itself stays
/// safe to call so the call sites don't need their own `cfg!` guards.
fn open_path_in_windows_terminal(path: &str) {
    #[cfg(target_os = "windows")]
    {
        if let Err(err) = Command::new("wt").args(["-d", path]).spawn() {
            eprintln!("warning: failed to launch Windows Terminal: {err:#}");
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        eprintln!("info: 'Open in Windows Terminal' is Windows-only");
    }
}

/// Sidebar filter input keystroke handler. Mirrors the
/// `new_project_dialog::handle_key_down` pattern: backspace pops a
/// char, escape clears the field, printable characters append. No
/// "submit" key — filtering is live as the user types.
fn handle_filter_key_down(
    sidebar: &mut Sidebar,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<Sidebar>,
) {
    let key = event.keystroke.key.as_str();
    match key {
        "escape" => {
            if !sidebar.filter.is_empty() {
                sidebar.filter.clear();
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        "backspace" => {
            if sidebar.filter.pop().is_some() {
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        _ => {}
    }
    let Some(key_char) = event.keystroke.key_char.as_deref() else {
        return;
    };
    if key_char.is_empty() {
        return;
    }
    let mut changed = false;
    for ch in key_char.chars() {
        if !ch.is_control() {
            sidebar.filter.push(ch);
            changed = true;
        }
    }
    if changed {
        cx.stop_propagation();
        cx.notify();
    }
}

/// Does a worktree row match the lowercased filter needle? Matches
/// the branch (when set) and the folder leaf (always). Caller has
/// already lowercased `needle` and confirmed the project name didn't
/// match.
fn worktree_row_matches(wt: &WorktreeRowData, needle: &str) -> bool {
    if let Some(branch) = wt.branch.as_deref()
        && branch.to_ascii_lowercase().contains(needle)
    {
        return true;
    }
    let leaf = std::path::Path::new(&wt.path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    leaf.to_ascii_lowercase().contains(needle)
}

/// The command we auto-type for "New Claude session". Bare `claude`
/// — relies on the user's PATH to resolve it (npm-global on Windows,
/// homebrew/npm on macOS, /usr/local/bin or similar on Linux). When
/// it fails the user just sees a `command not found` in their shell
/// and can install it from there. Mirrors the C# build's default
/// agent invocation; the full agent picker (Codex / shell / custom)
/// lands when the settings story does.
fn claude_command() -> &'static str {
    "claude"
}

/// Platform-appropriate label for the "Reveal in <native file browser>"
/// menu row. Mirrors the underlying spawn target in
/// [`Sidebar::reveal_in_explorer`] (`explorer.exe` / `open` /
/// `xdg-open`) so the UI matches what actually happens. The C# build
/// is Windows-only and uses "Reveal in File Explorer" verbatim — we
/// keep that string on Windows and pick a native equivalent
/// elsewhere instead of shipping a Windows-centric label on macOS /
/// Linux.
fn reveal_in_file_browser_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "Reveal in File Explorer"
    } else if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Reveal in File Manager"
    }
}

/// Cheap hash for use as a gpui element id derived from a string id.
/// `id` itself isn't `Hash` for gpui's id needs but `(static_str, u64)`
/// is — so we shrink the project id to a u64.
fn id_hash(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_agent_label_resolves_built_in_ids() {
        assert_eq!(history_agent_display_name("claude"), Some("Claude Code"));
        assert_eq!(history_agent_display_name("copilot"), Some("Copilot CLI"));
        assert_eq!(history_agent_display_name("opencode"), Some("OpenCode"));
        assert_eq!(history_agent_display_name("pi"), Some("Pi"));
        assert_eq!(history_agent_display_name("codex"), Some("Codex"));
    }

    #[test]
    fn history_agent_label_is_case_insensitive() {
        assert_eq!(history_agent_display_name("Claude"), Some("Claude Code"));
        assert_eq!(history_agent_display_name("CLAUDE"), Some("Claude Code"));
        assert_eq!(history_agent_display_name("Copilot"), Some("Copilot CLI"));
    }

    #[test]
    fn history_agent_label_normalizes_legacy_aliases() {
        // `claude-code` and similar dash/underscore variants resolve
        // to the canonical first-token id used by the registry.
        assert_eq!(history_agent_display_name("claude-code"), Some("Claude Code"));
        assert_eq!(history_agent_display_name("claude_code"), Some("Claude Code"));
        assert_eq!(history_agent_display_name("copilot-cli"), Some("Copilot CLI"));
    }

    #[test]
    fn history_agent_label_unknown_returns_none() {
        assert_eq!(history_agent_display_name(""), None);
        assert_eq!(history_agent_display_name("gemini"), None);
        assert_eq!(history_agent_display_name("-"), None);
    }

    fn row(branch: Option<&str>, path: &str) -> WorktreeRowData {
        WorktreeRowData {
            id: "test".into(),
            canonical_path: path.into(),
            path: path.into(),
            branch: branch.map(|s| s.to_string()),
            closed_sessions: Vec::new(),
        }
    }

    #[test]
    fn filter_matches_branch_case_insensitively() {
        // `worktree_row_matches` expects the caller to lowercase the
        // needle (the render-side caller does — `self.filter.to_ascii_lowercase()`).
        let r = row(Some("Feat/Foo"), "/repos/bar");
        assert!(worktree_row_matches(&r, "feat"));
        assert!(worktree_row_matches(&r, "foo"));
    }

    #[test]
    fn filter_matches_folder_leaf() {
        // Cross-platform path — `/` works as a separator on Windows
        // and Unix, so `Path::file_name` yields `my-proj` on both.
        let r = row(None, "/repos/my-proj");
        assert!(worktree_row_matches(&r, "my-proj"));
        assert!(worktree_row_matches(&r, "proj"));
        // "repos" is part of the parent path, not the leaf — must
        // NOT match.
        assert!(!worktree_row_matches(&r, "repos"));
    }

    #[test]
    fn filter_no_match_returns_false() {
        let r = row(Some("main"), "/repos/bar");
        assert!(!worktree_row_matches(&r, "missing"));
    }
}
