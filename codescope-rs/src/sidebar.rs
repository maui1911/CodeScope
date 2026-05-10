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

use codescope_core::{AppPaths, LayoutState, Project, ProjectsConfig, Theme};
use codescope_core::git::GitStatus;
use gpui::{
    AppContext, ClipboardItem, Context, Corner, EventEmitter, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, PathPromptOptions, Pixels, Point, Render,
    SharedString, Styled, Window, anchored, deferred, div, point, px,
};

use crate::new_worktree_dialog::NewWorktreeDialogState;
use crate::theme;

/// How often the dirty-state poller wakes up. 5 s is well under
/// the user's reaction-to-edit window (most workflows save +
/// glance at the sidebar within 1-2 seconds), and well over the
/// per-call I/O budget of `git status --porcelain` even on
/// thousand-file repos. Mirrors the C# build's
/// `WorktreePoller.Interval`.
const DIRTY_POLL_INTERVAL: Duration = Duration::from_secs(5);

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
    /// Spawn a new tab whose terminal is pinned to `working_directory`,
    /// using `title` for the tab strip. Title is just a label — the
    /// host decides what to actually show (in practice the worktree
    /// branch name suffixed with the project name).
    OpenSession {
        working_directory: PathBuf,
        title: SharedString,
        /// Optional command to auto-type at the shell prompt once the
        /// pty has come up. Used by "New Claude session" / "New
        /// Codex session" rows to launch the agent inline; `None`
        /// just opens a plain shell. The host adds the trailing CR.
        auto_type: Option<SharedString>,
    },
    /// Surface a status notification to the user. The sidebar emits
    /// these from menu actions (pull / fetch / open remote / discard)
    /// so the AppShell's toast layer can render them; without this
    /// channel sidebar errors would only land in stderr where the
    /// user can't see them. Severity drives the toast colour stripe
    /// and lifetime.
    Toast { kind: ToastSeverity, title: SharedString, detail: Option<SharedString> },
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
    branch: Option<String>,
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
    /// Project ids the user has explicitly collapsed. Projects start
    /// expanded by default; toggling the chevron adds/removes the id
    /// here. Mirrors the C# `TreeViewItem.IsExpanded` state per
    /// project — except the C# build hangs that off WPF's tree
    /// control, while we keep an explicit `HashSet<String>` so the
    /// render loop can decide visibility without per-row state.
    /// In-memory only for now; persisting to `layout.json` is a
    /// follow-up (matches what the C# build does — TreeView state
    /// is also session-scoped over there).
    collapsed_projects: HashSet<String>,
}

impl Sidebar {
    pub fn new(
        projects: ProjectsConfig,
        layout: LayoutState,
        theme: Arc<Theme>,
        paths: Arc<AppPaths>,
    ) -> Self {
        // Restore last-opened project if it still exists. Falls back
        // to the first project when the saved id is gone (project
        // removed between sessions) or absent (first launch).
        let selected = match layout.selected_project_id.as_deref() {
            Some(id) => projects.projects.iter().position(|p| p.id == id),
            None => None,
        }
        .or_else(|| (!projects.projects.is_empty()).then_some(0));
        Self {
            projects,
            selected,
            theme,
            paths,
            layout,
            menu: None,
            dialog: None,
            dirty_state: HashMap::new(),
            git_status: HashMap::new(),
            collapsed_projects: HashSet::new(),
        }
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
        cx.notify();
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

    /// Open the platform "pick a folder" dialog. On confirm, hand the
    /// path to [`Self::add_project`] which writes `projects.json`
    /// before mutating in-memory state, so a save failure leaves both
    /// the disk and the UI in their previous (consistent) state.
    pub fn open_add_project_picker(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match rx.await {
                Ok(Ok(Some(paths))) => paths,
                // `Ok(None)` = user cancelled, `Ok(Err(...))` = picker
                // failed to open (Linux). Both end the flow silently
                // — the user already sees what happened on screen.
                Ok(Ok(None)) => return,
                Ok(Err(err)) => {
                    eprintln!("warning: file picker failed: {err:#}");
                    return;
                }
                Err(_) => return,
            };
            if let Some(path) = paths.into_iter().next() {
                let path_str = path.to_string_lossy().into_owned();
                let _ = this.update(cx, |this, cx| {
                    this.add_project(path_str, cx);
                });
            }
        })
        .detach();
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
        if !project.worktrees.iter().any(|wt| wt.id == worktree_id) {
            return;
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
        let mut next = self.projects.clone();
        next.projects.remove(idx);
        if let Err(err) = next.save(&self.paths) {
            eprintln!("warning: failed to save projects.json: {err:#}");
            return;
        }
        self.projects = next;
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
        // Only persist layout when the persisted id actually changed —
        // i.e. when the removed project was the active one. Removing
        // a row before/after the active one leaves the id intact.
        if self.layout.selected_project_id != prev_selected_id {
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
        // Snapshot project + non-primary worktree metadata up front so
        // each row's `cx.listener` closure can hold owned values
        // without overlapping the immutable borrow `iter()` would
        // otherwise extend across the rest of `render`. Primary
        // worktrees are implicit at `Project::path`; we only emit
        // child rows for the extras.
        let rows: Vec<(usize, String, SharedString, String, Vec<WorktreeRowData>)> = self
            .projects
            .projects
            .iter()
            .enumerate()
            .map(|(idx, p)| {
                let worktrees = p
                    .worktrees
                    .iter()
                    .filter(|wt| !wt.is_primary)
                    .map(|wt| WorktreeRowData {
                        id: wt.id.clone(),
                        path: wt.path.clone(),
                        branch: wt.branch.clone(),
                    })
                    .collect();
                (
                    idx,
                    p.id.clone(),
                    SharedString::from(p.name.clone()),
                    p.name.clone(),
                    worktrees,
                )
            })
            .collect();

        let heading = div()
            .h(px(40.0))
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .text_size(px(11.0))
            .text_color(theme::ink_muted(&theme))
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
                            this.open_add_project_picker(window, cx);
                        }),
                    )
                    .child("+"),
            );

        let empty_state = if self.projects.projects.is_empty() {
            Some(
                div()
                    .px_3()
                    .py_4()
                    .text_size(px(12.0))
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
            let bg = if active {
                theme::frost_10(&theme)
            } else {
                gpui::transparent_black()
            };
            let rail = if active {
                theme::accent(&theme)
            } else {
                gpui::transparent_black()
            };
            let text_color = if active {
                theme::ink(&theme)
            } else {
                theme::ink_dim(&theme)
            };
            let frost_hover = theme::frost_10(&theme);
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
                .text_color(theme::ink_ghost(&theme))
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
                let wt_label: SharedString = wt
                    .branch
                    .clone()
                    .unwrap_or_else(|| {
                        // Fallback when the worktree row in
                        // `projects.json` predates branch tracking:
                        // surface the folder leaf so the user still
                        // sees something useful.
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
                let frost_hover = theme::frost_10(&theme);
                let ink_hover = theme::ink(&theme);
                // Resolve dirty-state for this worktree. `None` (still
                // loading) gets a dim ghost dot; `Some(false)` (clean)
                // a small accent-coloured dot; `Some(true)` (dirty) a
                // warning-coloured dot.
                let dirty_dot_color = match self.dirty_state.get(&wt.path) {
                    Some(true) => theme::status_dirty(&theme),
                    Some(false) => theme::status_clean(&theme),
                    None => theme::ink_ghost(&theme),
                };
                let wt_row = div()
                    .id(("worktree", id_hash(&wt.id)))
                    .h(px(28.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .pl(px(34.0)) // align under the project text past the rail + indent
                    .pr_3()
                    .gap_2()
                    .text_color(theme::ink_ghost(&theme))
                    .text_size(px(12.0))
                    .cursor_pointer()
                    .hover(move |s| s.bg(frost_hover).text_color(ink_hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.emit(SidebarEvent::OpenSession {
                                working_directory: PathBuf::from(&wt_path_for_event),
                                title: title_label.clone(),
                                auto_type: None,
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
                    .child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded_full()
                            .bg(dirty_dot_color),
                    )
                    .child(div().flex_grow().truncate().child(wt_label));
                // Right-aligned status slug — `chg` / `↑N ↓N` / `idle`
                // computed by
                // `codescope_core::git::worktree_status_label` from
                // the cached `git_status` snapshot. Renders the same
                // information the C# `WorktreeViewModel.StatusLabel`
                // slot shows. Sized at 10 pt with the dim
                // `ink_ghost` foreground; the C# build also paints
                // this in `Fig.Font.Mono`, but the Rust sidebar
                // still draws every text element in the default
                // sans family — switching the whole sidebar over to
                // the mono / sans split is a separate follow-up
                // rather than a one-off here. The C# build also
                // surfaces `busy` (active agent) and `ci!` (failing
                // PR CI); those land alongside session and PR
                // tracking.
                let status_slug = self
                    .git_status
                    .get(&wt.path)
                    .map(codescope_core::git::worktree_status_label)
                    .unwrap_or_default();
                let wt_row = if status_slug.is_empty() {
                    wt_row
                } else {
                    wt_row.child(
                        div()
                            .ml(px(8.0))
                            .mr(px(4.0))
                            .text_size(px(10.0))
                            .text_color(theme::ink_ghost(&theme))
                            .child(status_slug),
                    )
                };
                project_and_worktree_rows.push(wt_row.into_any_element());
            }
        }

        let mut body = div()
            .flex()
            .flex_col()
            .py_1()
            .children(project_and_worktree_rows);
        if let Some(es) = empty_state {
            body = body.child(es);
        }

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
            .child(heading)
            .child(div().h_px().bg(theme::divider(&theme)))
            .child(body);

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
        root
    }
}

impl Sidebar {
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
                .text_size(px(13.0))
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
            // Header — non-interactive, mirrors the C# `BuildContextHeader`
            // (project name dimmed, "project" qualifier).
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(11.0))
                    .text_color(ink_ghost)
                    .child(div().text_color(ink).text_size(px(13.0)).truncate().child(header_label))
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
                .text_size(px(13.0))
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
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(11.0))
                    .text_color(ink_ghost)
                    .child(div().text_color(ink).text_size(px(13.0)).truncate().child(branch_label))
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
                        cx.emit(SidebarEvent::OpenSession {
                            working_directory: path_for_open.clone(),
                            title: title_for_open.clone(),
                            auto_type: None,
                        });
                        this.close_menu(cx);
                    }),
                )
            })
            // "New Claude session" — same as Open session but auto-
            // types `claude` after the shell is up. Lands above the
            // Reveal/Copy/Remove rows so the agent-launch path stays
            // close to the plain Open session row.
            //
            // Title derives from the same `open_session_title` shape
            // (`{project} · {branch}`) plus a ` · claude` suffix so
            // multiple worktrees stay distinguishable in the tab strip
            // when the user has agents running in several of them.
            .child({
                let path = PathBuf::from(&worktree.path);
                let title = SharedString::from(format!(
                    "{} · claude",
                    open_session_title.clone()
                ));
                item(
                    "wt-menu-new-claude",
                    "New Claude session",
                    false,
                    Box::new(move |this, _window, cx| {
                        cx.emit(SidebarEvent::OpenSession {
                            working_directory: path.clone(),
                            title: title.clone(),
                            auto_type: Some(claude_command().into()),
                        });
                        this.close_menu(cx);
                    }),
                )
            })
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
                        .text_size(px(13.0))
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
