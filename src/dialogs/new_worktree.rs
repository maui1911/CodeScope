//! "New worktree from branch…" dialog — Rust port of
//! `legacy:CodeScope.Ui/Dialogs/NewWorktreeDialog.xaml.cs`.
//!
//! Trimmed first cut versus the C# build: only the branch-name input
//! is interactive. The folder path is auto-derived from the sanitised
//! branch under `<project>.worktrees/`, the base branch is implicitly
//! HEAD (no dropdown), and we don't auto-spawn a session — the user
//! clicks the new worktree row in the sidebar after creation. The
//! C# spec drives the wording (button labels, footer caption, the
//! sanitisation rule), so the dialog already feels like the WPF one.
//!
//! Lives next to [`crate::sidebar::Sidebar`] because the dialog state
//! and the sidebar's project list move together: opening, validating,
//! and creating all happen against the same `ProjectsConfig`. We
//! expose only the data + render + key handlers; the sidebar owns
//! when to show / hide and what to do with the resulting `Worktree`.

use std::path::Path;
use std::sync::Arc;

use codescope_core::{Project, Theme, git::BranchInfo, projects::Worktree};
use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window,
    anchored, deferred, div, point, px,
};

use crate::sidebar::Sidebar;
use crate::text_field::{TextField, focused_caret_style, render_input_content};
use crate::theme;

/// The set of characters Windows refuses inside a filename. The C#
/// build pulls these from `Path.GetInvalidFileNameChars()` plus the
/// path separators it replaces explicitly. We hard-code the union so
/// the sanitiser behaves the same on every host (a Linux dev build
/// would otherwise let `?` through and break when the worktree gets
/// checked out on Windows).
const INVALID_FILENAME_CHARS: &[char] = &[
    '<', '>', ':', '"', '|', '?', '*', '/', '\\', '\0',
];

/// Mirror of C#'s `NewWorktreeDialog.Sanitize`. Replace path
/// separators and reserved chars with `-`, then trim leading/trailing
/// dashes so a branch like `feat/foo` becomes `feat-foo` (not
/// `-feat-foo` or similar).
pub fn sanitize_branch_to_folder(branch: &str) -> String {
    let mut out = String::with_capacity(branch.len());
    for ch in branch.chars() {
        if INVALID_FILENAME_CHARS.contains(&ch) || ch.is_control() {
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    out.trim_matches('-').to_string()
}

/// Derive the auto-folder path under `worktree_root` for the given
/// branch. Empty branch → empty path so the validity check has
/// something to fail against.
fn derived_folder(worktree_root: &str, branch: &str) -> String {
    let safe = sanitize_branch_to_folder(branch);
    if safe.is_empty() {
        return String::new();
    }
    let sep = if worktree_root.contains('\\') { '\\' } else { '/' };
    format!("{worktree_root}{sep}{safe}")
}

/// The "leaf" component (folder name) of the path, for the footer
/// caption. We don't use `Path::file_name` because it returns `None`
/// for paths ending in `\` and we want a forgiving last-segment
/// extractor that works on whatever the user has typed so far.
fn folder_leaf(path: &str) -> &str {
    path.rsplit_once(['\\', '/']).map(|(_, leaf)| leaf).unwrap_or(path)
}

/// Which input field currently receives typed characters from the
/// dialog's `on_key_down`. `Branch` is the open-dialog default;
/// `Folder` activates when the user clicks into the folder row to
/// override the auto-derived path; `BasePopupSearch` activates when
/// the base-branch dropdown is open and steals focus for type-to-
/// filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogField {
    Branch,
    Folder,
    BasePopupSearch,
}

/// Live state of an open dialog. Created by [`Sidebar::open_new_worktree_dialog`]
/// and dropped when the user confirms or cancels.
pub struct NewWorktreeDialogState {
    pub project_idx: usize,
    pub project_name: String,
    pub project_path: String,
    pub worktree_root: String,
    pub branch: TextField,
    pub folder: TextField,
    pub error: Option<String>,
    pub focus_handle: FocusHandle,
    /// Mirrors C#'s `SpawnSession` — defaults to `true` so confirming
    /// the dialog drops the user inside the new worktree. Toggle in
    /// the dialog footer flips it; `submit_new_worktree_dialog` gates
    /// the `OpenSession` event on this flag.
    pub spawn_session: bool,
    /// `false` until the user clicks into the FOLDER row and types.
    /// While `false`, every BRANCH change re-derives the folder; once
    /// `true`, the user is in control and we stop overwriting their
    /// edits. Mirrors the C# `BranchBox.TextChanged` heuristic
    /// ("auto-sync only when the path is empty or still under the
    /// worktree root").
    pub folder_overridden: bool,
    /// Where typed characters land. Click handlers on the BRANCH /
    /// FOLDER / base-popup search rows flip this.
    pub focused_field: DialogField,
    /// Available branches for the base-branch picker. Loaded once at
    /// dialog open via `git::list_branches`; an Err there yields an
    /// empty list and a populated [`Self::branch_load_error`] which
    /// the popup renders in place of the empty row list. The dialog
    /// still works in that state — the user can pick "(HEAD)" and
    /// hit Create.
    pub branches: Vec<BranchInfo>,
    /// Error message from the failed `list_branches` call at dialog
    /// open, surfaced inside the base-branch popup so the user knows
    /// *why* it's empty. Kept separate from [`Self::error`] (which is
    /// reserved for submit / validation failures) so a transient
    /// branch-list failure doesn't clobber a later, more relevant
    /// `git worktree add` error message.
    pub branch_load_error: Option<String>,
    /// Selected base branch by name. `None` = `(HEAD)` (the current
    /// HEAD of the project's primary worktree). The C# build's
    /// `(HEAD)` row maps to a null `BaseBranch` for the same reason.
    pub base_branch: Option<String>,
    /// Dropdown popover open?
    pub base_popup_open: bool,
    /// Filter text typed into the popup search.
    pub base_query: TextField,
    /// Currently-highlighted row in the popup (post-filter index).
    /// `0` always points at `(HEAD)` since we pin it on top.
    pub base_selected_idx: usize,
}

impl NewWorktreeDialogState {
    pub fn new(
        idx: usize,
        project: &Project,
        branches: Vec<BranchInfo>,
        focus_handle: FocusHandle,
    ) -> Self {
        let worktree_root = project.worktree_root_path();
        let base_branch = resolve_default_base(&branches, &project.default_branch);
        Self {
            project_idx: idx,
            project_name: project.name.clone(),
            project_path: project.path.clone(),
            worktree_root,
            branch: TextField::new(),
            folder: TextField::new(),
            error: None,
            focus_handle,
            spawn_session: true,
            folder_overridden: false,
            focused_field: DialogField::Branch,
            branches,
            branch_load_error: None,
            base_branch,
            base_popup_open: false,
            base_query: TextField::new(),
            base_selected_idx: 0,
        }
    }

    /// Mirrors the C# `RefreshValidity`: branch must be at least 2
    /// non-whitespace chars, and the (trimmed) folder must be
    /// non-empty. Trimming matters now that the FOLDER row is
    /// editable — without it the user could whitespace their way to
    /// an enabled Create button and then watch git fail on the
    /// resulting nonsense path.
    pub fn is_valid(&self) -> bool {
        self.branch.text().trim().len() >= 2 && !self.folder.text().trim().is_empty()
    }

    fn recompute_folder(&mut self) {
        if !self.folder_overridden {
            self.folder
                .set_text(derived_folder(&self.worktree_root, self.branch.text()));
        }
        // A typing change always invalidates a stale error message —
        // the user is correcting the input.
        self.error = None;
    }

    /// Apply `op` (insert / delete / move) to whichever input is in
    /// focus. Returns whatever `op` reports — `true` when the
    /// underlying buffer was touched, `false` on a no-op (e.g.
    /// backspace at caret 0). Side effects (recompute_folder,
    /// folder_overridden, base_selected_idx reset, error clearing)
    /// only fire when `op` actually changed something so a no-op
    /// keystroke doesn't trigger a redraw.
    fn with_focused_field<F: FnOnce(&mut TextField) -> bool>(
        &mut self,
        op: F,
    ) -> bool {
        match self.focused_field {
            DialogField::Branch => {
                let changed = op(&mut self.branch);
                if changed {
                    self.recompute_folder();
                }
                changed
            }
            DialogField::Folder => {
                let changed = op(&mut self.folder);
                if changed {
                    self.folder_overridden = true;
                    self.error = None;
                }
                changed
            }
            DialogField::BasePopupSearch => {
                let changed = op(&mut self.base_query);
                if changed {
                    self.base_selected_idx = 0;
                }
                changed
            }
        }
    }

    pub fn insert_char(&mut self, ch: char) -> bool {
        // insert_char on a TextField always changes the buffer.
        self.with_focused_field(|f| {
            f.insert_char(ch);
            true
        })
    }
    pub fn backspace(&mut self) -> bool {
        self.with_focused_field(|f| f.backspace())
    }
    pub fn delete_forward(&mut self) -> bool {
        self.with_focused_field(|f| f.delete_forward())
    }
    pub fn move_caret_left(&mut self) -> bool {
        self.with_focused_field(|f| f.move_left())
    }
    pub fn move_caret_right(&mut self) -> bool {
        self.with_focused_field(|f| f.move_right())
    }
    pub fn move_caret_home(&mut self) -> bool {
        self.with_focused_field(|f| f.move_home())
    }
    pub fn move_caret_end(&mut self) -> bool {
        self.with_focused_field(|f| f.move_end())
    }

    /// Mutable accessor for one of the editable fields by name.
    /// Used by the mouse-down hit-test path so a click can shift
    /// focus AND drop the caret at the click position in one step.
    pub fn field_mut_by(&mut self, field: DialogField) -> &mut TextField {
        match field {
            DialogField::Branch => &mut self.branch,
            DialogField::Folder => &mut self.folder,
            DialogField::BasePopupSearch => &mut self.base_query,
        }
    }

    /// Filtered branch list for the base-branch popup. `(HEAD)` is
    /// never in this vec — the renderer pins it as a sentinel row at
    /// the top of the popup. LOCAL group first, then REMOTE — within
    /// each group the original alphabetic sort from `list_branches`
    /// is preserved.
    pub fn filtered_branches(&self) -> Vec<&BranchInfo> {
        filter_branches(&self.branches, self.base_query.text())
    }
}

/// Pure helper used by [`NewWorktreeDialogState::filtered_branches`].
/// Extracted so the filter logic can be unit-tested without
/// constructing a `NewWorktreeDialogState` (which carries a real
/// `FocusHandle` we don't have in test scope).
pub fn filter_branches<'a>(
    branches: &'a [BranchInfo],
    query: &str,
) -> Vec<&'a BranchInfo> {
    let q = query.trim().to_lowercase();
    let matches = |b: &&BranchInfo| q.is_empty() || b.name.to_lowercase().contains(&q);
    let mut out: Vec<&BranchInfo> =
        branches.iter().filter(|b| !b.is_remote).filter(matches).collect();
    out.extend(branches.iter().filter(|b| b.is_remote).filter(matches));
    out
}

/// Pure helper used by [`NewWorktreeDialogState::new`] to pick the
/// default base. Mirrors C#'s
/// `req.DefaultBase ?? first-local-match ?? (HEAD)`:
///
/// 1. exact match on the project's `default_branch` (local ref), or
/// 2. the first local branch in the loaded list, or
/// 3. `None` — which the dialog renders as `(HEAD)`.
///
/// The fallback step matters for repos where the configured default
/// branch was renamed (e.g. `master` → `main`) but the rename hasn't
/// been pulled locally yet, or for fresh clones where only one local
/// branch exists. Without it the dialog would default to `(HEAD)`
/// even when there's an obvious local choice.
pub fn resolve_default_base(
    branches: &[BranchInfo],
    default_branch: &str,
) -> Option<String> {
    if let Some(b) = branches
        .iter()
        .find(|b| !b.is_remote && b.name == default_branch)
    {
        return Some(b.name.clone());
    }
    branches
        .iter()
        .find(|b| !b.is_remote)
        .map(|b| b.name.clone())
}

impl Sidebar {
    /// Slot the disabled "New worktree from branch…" row hands off
    /// to. Pulls the project, builds the dialog state, focuses the
    /// branch input, closes the open context menu (so the dialog
    /// doesn't render on top of it), and triggers a redraw.
    pub fn open_new_worktree_dialog(
        &mut self,
        idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(project) = self.projects().projects.get(idx) else {
            return;
        };
        // Load branches once at open. An I/O error here doesn't stop
        // the dialog — the user can still pick `(HEAD)` and create —
        // but we stash the message in `branch_load_error` so the
        // popup can surface it inline instead of showing an empty
        // row list. Kept separate from `state.error` (submit / git
        // failures) so a transient list failure doesn't clobber a
        // later, more relevant message. Cloning the project path so
        // we don't hold a borrow across the `cx.focus_handle()` call.
        let project_path = project.path.clone();
        let project_clone = project.clone();
        let (branches, branch_load_err) =
            match codescope_core::git::list_branches(Path::new(&project_path)) {
                Ok(bs) => (bs, None),
                Err(err) => (Vec::new(), Some(format!("branch list failed: {err:#}"))),
            };
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let mut state =
            NewWorktreeDialogState::new(idx, &project_clone, branches, focus_handle);
        state.branch_load_error = branch_load_err;
        self.set_dialog(Some(state));
        self.close_menu_no_notify();
        cx.notify();
    }

    /// Close without creating. Bound to Escape, the Cancel button,
    /// and clicks on the dim backdrop.
    pub fn cancel_new_worktree_dialog(&mut self, cx: &mut Context<Self>) {
        if self.take_dialog().is_some() {
            cx.notify();
        }
    }

    /// Switch which dialog field receives typed characters. Click
    /// handlers on BRANCH / FOLDER call this. Closes the base popup
    /// as a side effect — they're mutually exclusive focus targets.
    pub fn focus_dialog_field(
        &mut self,
        field: DialogField,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.dialog_mut() {
            state.focused_field = field;
            state.base_popup_open = false;
            state.base_query.set_text("");
            state.base_selected_idx = 0;
            cx.notify();
        }
    }

    /// Flip the spawn-session toggle. Mirrors clicking the C#
    /// `SpawnTrack` pill.
    pub fn toggle_spawn_session(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.dialog_mut() {
            state.spawn_session = !state.spawn_session;
            cx.notify();
        }
    }

    /// Open / close the base-branch dropdown. Opening also moves
    /// keyboard focus to the popup's filter input so the user can
    /// type-to-filter without an extra click.
    pub fn toggle_base_popup(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.dialog_mut() {
            state.base_popup_open = !state.base_popup_open;
            if state.base_popup_open {
                state.focused_field = DialogField::BasePopupSearch;
                state.base_query.set_text("");
                state.base_selected_idx = 0;
            } else if state.focused_field == DialogField::BasePopupSearch {
                // Closing without picking returns focus to BRANCH —
                // matches the C# `BasePopup.IsOpen = false; BranchBox.Focus();`.
                state.focused_field = DialogField::Branch;
            }
            cx.notify();
        }
    }

    /// Pick a base branch by name (or `None` for `(HEAD)`) and close
    /// the popup. Returns focus to BRANCH so the user can keep
    /// editing the branch field. Mirrors C#'s `SetSelectedBase` +
    /// `BasePopup.IsOpen = false`.
    pub fn select_base_branch(
        &mut self,
        name: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.dialog_mut() {
            state.base_branch = name;
            state.base_popup_open = false;
            state.focused_field = DialogField::Branch;
            state.base_query.set_text("");
            state.base_selected_idx = 0;
            cx.notify();
        }
    }

    /// Move the popup highlight up/down. `delta = 1` moves down,
    /// `-1` up. Clamped to the filtered list length (including the
    /// pinned `(HEAD)` row at index 0).
    pub fn move_base_popup_selection(
        &mut self,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self.dialog_mut() else { return };
        let len = state.filtered_branches().len() + 1; // +1 for (HEAD)
        if len == 0 {
            return;
        }
        let max = len - 1;
        let cur = state.base_selected_idx as isize;
        let next = (cur + delta).clamp(0, max as isize);
        state.base_selected_idx = next as usize;
        cx.notify();
    }

    /// Resolve the current popup selection. Index 0 = `(HEAD)`;
    /// indices 1.. point into the filtered list (locals first, then
    /// remotes — same ordering as the rendered rows).
    pub fn confirm_base_popup_selection(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.dialog_mut() else { return };
        if !state.base_popup_open {
            return;
        }
        let idx = state.base_selected_idx;
        let name = if idx == 0 {
            None
        } else {
            // Borrow filtered branches transiently to extract the
            // chosen name, then drop the borrow before calling
            // `select_base_branch` (which mutates state).
            state
                .filtered_branches()
                .get(idx - 1)
                .map(|b| b.name.clone())
        };
        // Selection apply path is shared with click → reuse it so
        // the post-conditions stay consistent (focus returns to
        // BRANCH, query clears, popup closes).
        self.select_base_branch(name, cx);
    }

    /// Confirm. Validates, runs `git worktree add`, appends a
    /// `Worktree` row, persists `projects.json`, and closes the
    /// dialog. On `git` failure we leave the dialog open with the
    /// trimmed stderr stashed in `state.error` so the user can see
    /// what went wrong without losing what they typed.
    pub fn submit_new_worktree_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.dialog_mut() else { return };
        if !state.is_valid() {
            return;
        }
        let idx = state.project_idx;
        let branch = state.branch.text().trim().to_string();
        // Trim the folder too — the field is user-editable now, so a
        // stray leading/trailing space would otherwise sneak into
        // both the on-disk path and the persisted `Worktree.path`,
        // causing a confusing mismatch later.
        let folder = state.folder.text().trim().to_string();
        let project_path = state.project_path.clone();
        let base_branch = state.base_branch.clone();
        let spawn_session = state.spawn_session;

        let project_name = state.project_name.clone();
        let result = codescope_core::git::add_worktree(
            Path::new(&project_path),
            Path::new(&folder),
            &branch,
            base_branch.as_deref(),
        );
        match result {
            Ok(()) => {
                let new_wt = Worktree {
                    id: uuid::Uuid::new_v4().to_string(),
                    path: folder.clone(),
                    branch: Some(branch.clone()),
                    is_primary: false,
                };
                // Clone-then-save: a write failure leaves the in-
                // memory model untouched, matching the rest of
                // `Sidebar`'s mutators.
                let mut next = self.projects().clone();
                if let Some(project) = next.projects.get_mut(idx) {
                    project.worktrees.push(new_wt);
                } else {
                    self.cancel_new_worktree_dialog(cx);
                    return;
                }
                if let Err(err) = next.save(self.paths_ref()) {
                    if let Some(state) = self.dialog_mut() {
                        state.error = Some(format!("save failed: {err:#}"));
                    }
                    cx.notify();
                    return;
                }
                self.replace_projects(next);
                self.cancel_new_worktree_dialog(cx);
                // Spawn a session pinned to the new worktree only when
                // the toggle is on. Mirrors the C# dialog's
                // `SpawnSession` flag — defaults to `true` so the
                // common path drops the user inside the new worktree,
                // but flipping it off lets them create the worktree
                // without entering it. Single spaces around `·` to
                // match the C# build's
                // `$"{project.Name} · {branch}"` convention in
                // `MainViewModel.RefreshTabTitlesForWorktree`.
                if spawn_session {
                    // Just-created worktree, just-created folder —
                    // the user explicitly asked for a session here,
                    // so always spawn one. `force_new: true` skips
                    // the focus-or-open path entirely (matters in
                    // the unlikely-but-possible case where another
                    // tab already happens to point at the new
                    // worktree's folder, e.g. because the user
                    // rebuilt over a path a previous session was
                    // sitting in).
                    cx.emit(crate::sidebar::SidebarEvent::OpenSession {
                        working_directory: std::path::PathBuf::from(&folder),
                        title: format!("{project_name} · {branch}").into(),
                        auto_type: None,
                        agent_id: None,
                        force_new: true,
                    });
                }
            }
            Err(err) => {
                let msg = format!("{err:#}");
                if let Some(state) = self.dialog_mut() {
                    state.error = Some(msg);
                }
                cx.notify();
            }
        }
    }

    /// Render the modal. Returns `None` when no dialog is open.
    /// Rendered via `deferred(anchored(...))` so it paints over the
    /// entire window — including the tab strip and terminal — and
    /// captures clicks on the dim backdrop without bubbling to the
    /// chrome below.
    pub fn render_new_worktree_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.dialog()?;
        let viewport = window.viewport_size();

        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let ink_muted = theme::ink_muted(theme);
        let frost = theme::frost_10(theme);
        let danger = theme::danger();
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);

        let project_eyebrow: SharedString = state.project_name.to_uppercase().into();
        let footer_branch: SharedString = if state.branch.is_empty() {
            SharedString::from("…")
        } else {
            state.branch.text().to_string().into()
        };
        let footer_leaf: SharedString = if state.folder.is_empty() {
            SharedString::from("…")
        } else {
            folder_leaf(state.folder.text()).to_string().into()
        };
        let footer_base: SharedString = state
            .base_branch
            .clone()
            .unwrap_or_else(|| "HEAD".to_string())
            .into();
        let valid = state.is_valid();
        let error_msg: Option<SharedString> = state
            .error
            .as_ref()
            .map(|e| e.clone().into());
        let focus_handle = state.focus_handle.clone();
        let focused = state.focused_field;
        let spawn_session = state.spawn_session;
        let base_popup_open = state.base_popup_open;
        let base_label: SharedString = state
            .base_branch
            .clone()
            .unwrap_or_else(|| "(HEAD)".to_string())
            .into();

        // Header — eyebrow (project name) + title.
        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_5()
            .pt_5()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(ink_ghost)
                    .child(if project_eyebrow.is_empty() {
                        SharedString::from("PROJECT")
                    } else {
                        project_eyebrow
                    }),
            )
            .child(
                div()
                    .text_size(px(18.0))
                    .text_color(ink)
                    .child("New worktree"),
            );

        // Reusable textbox builder — lays out a single-line input as a
        // div styled like a WPF TextBox. The caret is painted inline
        // at the field's caret position via `render_input_content` and
        // only shown when this field has focus *and* the global blink
        // phase is on. The branch / folder split-point is rendered with
        // no gap between the text and the caret bar.
        let blink_phase = self.text_blink_phase;
        let textbox = |id: &'static str,
                       field: &TextField,
                       placeholder: &'static str,
                       this_field: DialogField|
         -> gpui::Stateful<gpui::Div> {
            let is_focused = focused == this_field && !base_popup_open;
            let mut style = focused_caret_style(theme, blink_phase);
            style.show_caret = is_focused && blink_phase;
            div()
                .id(id)
                .px_3()
                .py_2()
                .bg(canvas)
                .border_1()
                .border_color(if is_focused { accent } else { divider })
                .rounded_md()
                .text_size(px(13.0))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.focus_dialog_field(this_field, cx);
                        if let Some(state) = this.dialog_mut() {
                            let idx = state
                                .field_mut_by(this_field)
                                .index_for_window_point(event.position);
                            if let Some(idx) = idx {
                                state.field_mut_by(this_field).set_caret(idx);
                                cx.notify();
                            }
                        }
                    }),
                )
                .child(render_input_content(
                    field,
                    SharedString::from(placeholder),
                    style,
                ))
        };

        let branch_block = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_5()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(ink_ghost)
                    .child("BRANCH"),
            )
            .child(textbox(
                "nw-branch",
                &state.branch,
                "e.g. feat/awesome",
                DialogField::Branch,
            ));

        // FOLDER row — editable, but the empty state still shows the
        // auto-derived path (greyed out so it reads as a hint, not an
        // edit). We pass the live `state.folder` whether or not the
        // user has overridden — auto-derive keeps it in sync with the
        // branch until the first folder keystroke.
        let folder_block = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_5()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(ink_ghost)
                    .child("FOLDER"),
            )
            .child(textbox(
                "nw-folder",
                &state.folder,
                "<derived from branch>",
                DialogField::Folder,
            ));

        // BASE row — clickable pill that toggles the dropdown popup.
        // The current selection is rendered inline; a chevron hints
        // that it's a dropdown. Mirrors the C# `BaseTrigger`.
        let base_block = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_5()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(ink_ghost)
                    .child("BASE"),
            )
            .child(
                div()
                    .id("nw-base-trigger")
                    .px_3()
                    .py_2()
                    .bg(canvas)
                    .border_1()
                    .border_color(if base_popup_open { accent } else { divider })
                    .rounded_md()
                    .text_size(px(13.0))
                    .text_color(ink)
                    .cursor_pointer()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_base_popup(cx);
                        }),
                    )
                    .child(div().flex_grow().truncate().child(base_label))
                    .child(div().text_color(ink_ghost).child("▾")),
            );

        // SPAWN toggle — pill switch matching the C# `SpawnTrack` /
        // `SpawnThumb`. Click anywhere flips `spawn_session`. Track
        // colour reflects state; the label sits next to the pill.
        let track_bg = if spawn_session { accent } else { divider };
        let thumb_align = if spawn_session { "right" } else { "left" };
        let mut track = div()
            .id("nw-spawn-track")
            .w(px(34.0))
            .h(px(18.0))
            .bg(track_bg)
            .rounded_full()
            .flex()
            .items_center()
            .px(px(2.0))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_spawn_session(cx);
                }),
            );
        let thumb = div()
            .w(px(14.0))
            .h(px(14.0))
            .rounded_full()
            .bg(canvas);
        // Push thumb to the right edge when on; left edge when off.
        // `flex_grow` on a sibling spacer is the simplest way to align
        // without measuring the track's own bounds.
        if thumb_align == "right" {
            track = track.child(div().flex_grow()).child(thumb);
        } else {
            track = track.child(thumb).child(div().flex_grow());
        }
        let spawn_block = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_5()
            .child(track)
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(if spawn_session { ink } else { ink_dim })
                    .child("Open a session in the new worktree"),
            );

        // Optional error row.
        let error_block: Option<gpui::Div> = error_msg.map(|msg| {
            div()
                .px_5()
                .text_size(px(12.0))
                .text_color(danger)
                .child(msg)
        });

        // Footer caption — mirrors the C# `FootMeta`:
        // "git worktree add · <base> → <branch> @ <leaf>"
        let footer_meta = div()
            .px_5()
            .text_size(px(11.0))
            .text_color(ink_muted)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child("git worktree add ·")
                    .child(div().text_color(ink_dim).child(footer_base))
                    .child("→")
                    .child(div().text_color(ink_dim).child(footer_branch))
                    .child("@")
                    .child(div().text_color(ink_dim).child(footer_leaf)),
            );

        // Buttons.
        let cancel_btn = div()
            .id("nw-cancel")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(ink_dim)
            .border_1()
            .border_color(divider)
            .rounded_md()
            .cursor_pointer()
            .hover(move |s| s.bg(frost).text_color(ink))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.cancel_new_worktree_dialog(cx);
                }),
            )
            .child("Cancel");

        let create_color = if valid { ink } else { ink_ghost };
        let create_bg = if valid { accent } else { divider };
        let create_btn = {
            let mut btn = div()
                .id("nw-create")
                .px_4()
                .py_2()
                .text_size(px(13.0))
                .text_color(create_color)
                .bg(create_bg)
                .rounded_md();
            if valid {
                btn = btn
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.submit_new_worktree_dialog(cx);
                        }),
                    );
            }
            btn.child("Create")
        };

        let footer_buttons = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .px_5()
            .pb_5()
            .child(cancel_btn)
            .child(create_btn);

        // Assemble the card. Fixed width so a long error message
        // wraps inside the card instead of stretching it across the
        // window.
        let mut card = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(440.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .pb_2()
            .track_focus(&focus_handle)
            .key_context("NewWorktreeDialog")
            .on_key_down(cx.listener(handle_key_down))
            // Clicking inside the card must not bubble out and trigger
            // the backdrop's dismiss handler.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(branch_block)
            .child(folder_block)
            .child(base_block);
        if let Some(eb) = error_block {
            card = card.child(eb);
        }
        card = card.child(footer_meta).child(spawn_block).child(footer_buttons);

        // Backdrop covers the entire window. Sized exactly to the
        // viewport so a click anywhere outside the card lands here.
        let backdrop = div()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.cancel_new_worktree_dialog(cx)),
            )
            .child(card);

        // Optional base-branch popup. Rendered as a separate
        // `deferred` element with higher priority than the dialog
        // backdrop so it overlays the card. We don't anchor it to
        // the BASE trigger's screen-space rect (we don't have it
        // here without a hitbox round-trip), so we centre it under
        // the dialog instead — simpler and matches the C# popup's
        // visual placement closely enough.
        let popup = base_popup_open
            .then(|| self.render_base_popup(state, theme, viewport, cx));

        let mut layers: Vec<gpui::AnyElement> = Vec::new();
        layers.push(
            deferred(
                anchored()
                    .position(point(px(0.0), px(0.0)))
                    .child(backdrop),
            )
            // Higher priority than the context menu so an opened
            // dialog always paints on top.
            .with_priority(10)
            .into_any_element(),
        );
        if let Some(p) = popup {
            layers.push(p);
        }

        Some(
            div().children(layers).into_any_element(),
        )
    }

    /// Build the base-branch dropdown popover. Pinned to the dialog's
    /// vertical centre so the search row + first few rows are visible
    /// even on a small window. The `(HEAD)` row is always at the top;
    /// LOCAL group label + rows; REMOTE group label + rows.
    fn render_base_popup(
        &self,
        state: &NewWorktreeDialogState,
        theme: &Arc<Theme>,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let ink_muted = theme::ink_muted(theme);
        let frost = theme::frost_10(theme);
        let canvas = theme::canvas(theme);

        let filtered = state.filtered_branches();
        let selected_idx = state.base_selected_idx;

        // Search row at the top — the type-to-filter input. Same
        // caret discipline as the BRANCH / FOLDER inputs.
        let mut search_style = focused_caret_style(theme, self.text_blink_phase);
        search_style.show_caret = self.text_blink_phase
            && state.focused_field == DialogField::BasePopupSearch;
        let search = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .h(px(32.0))
            .border_b_1()
            .border_color(divider)
            .bg(canvas)
            .text_size(px(12.0))
            .child(render_input_content(
                &state.base_query,
                SharedString::from("Filter branches…"),
                search_style,
            ));

        // Build rows. `(HEAD)` is always at index 0 in the visible
        // list; locals start after, remotes after that. The selected
        // index drives the highlight + Enter resolution in the key
        // handler.
        let row_for = |idx: usize, label: SharedString, meta: SharedString, is_head: bool, name: Option<String>| {
            let active = idx == selected_idx;
            let bg = if active { frost } else { gpui::transparent_black() };
            div()
                .id(("nw-base-row", idx as u64))
                .h(px(28.0))
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_size(px(12.0))
                .text_color(if is_head { ink_dim } else { ink })
                .bg(bg)
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.select_base_branch(name.clone(), cx);
                    }),
                )
                .child(div().flex_grow().truncate().child(label))
                .child(div().text_color(ink_ghost).text_size(px(11.0)).child(meta))
        };

        // Index 0: `(HEAD)` sentinel. Always visible regardless of
        // filter — matching C#'s `s_headRow` pin.
        let head_row = row_for(
            0,
            SharedString::from("(HEAD)"),
            SharedString::from("current"),
            true,
            None,
        );

        // Build local + remote rows with group headers when each
        // group is non-empty in the filtered list. Visible-index
        // tracker keeps the click highlight in sync with the
        // keyboard navigation.
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        rows.push(head_row.into_any_element());

        let local_count = filtered.iter().filter(|b| !b.is_remote).count();
        let remote_count = filtered.len() - local_count;
        let mut visible_idx = 1usize;
        if local_count > 0 {
            rows.push(
                div()
                    .px_3()
                    .pt_2()
                    .text_size(px(10.0))
                    .text_color(ink_muted)
                    .child("LOCAL")
                    .into_any_element(),
            );
            for b in filtered.iter().filter(|b| !b.is_remote) {
                let meta = format!("{} · {}", b.short_sha, b.relative_date);
                rows.push(
                    row_for(
                        visible_idx,
                        b.name.clone().into(),
                        meta.into(),
                        false,
                        Some(b.name.clone()),
                    )
                    .into_any_element(),
                );
                visible_idx += 1;
            }
        }
        if remote_count > 0 {
            rows.push(
                div()
                    .px_3()
                    .pt_2()
                    .text_size(px(10.0))
                    .text_color(ink_muted)
                    .child("REMOTE")
                    .into_any_element(),
            );
            for b in filtered.iter().filter(|b| b.is_remote) {
                let meta = format!("{} · {}", b.short_sha, b.relative_date);
                rows.push(
                    row_for(
                        visible_idx,
                        b.name.clone().into(),
                        meta.into(),
                        false,
                        Some(b.name.clone()),
                    )
                    .into_any_element(),
                );
                visible_idx += 1;
            }
        }

        // Branch-list-load error banner. Shown above the rows when
        // `git::list_branches` failed at dialog open — the popup is
        // empty in that case (no branches loaded), so we surface the
        // reason inline instead of leaving the user staring at a
        // blank popover.
        let load_error_banner = state.branch_load_error.as_ref().map(|msg| {
            div()
                .px_3()
                .py_2()
                .text_size(px(11.0))
                .text_color(theme::danger())
                .child(SharedString::from(msg.clone()))
        });

        let popup = div()
            .w(px(360.0))
            .max_h(px(320.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_md()
            .shadow_lg()
            .flex()
            .flex_col()
            .overflow_hidden()
            // Click on the popup itself shouldn't bubble out and
            // trigger the dialog's mouse-down stopper from above.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(search)
            .children(load_error_banner)
            .child(
                // Stateful + `overflow_y_scroll` so a repo with more
                // branches than fit in `max_h(320)` still lets the
                // user reach every row by mouse. Without this the
                // popup container clips the overflowing rows and
                // they're unreachable. Stable id so gpui can persist
                // the scroll offset across renders.
                div()
                    .id("nw-base-popup-rows")
                    .flex_grow()
                    .flex()
                    .flex_col()
                    .py_1()
                    .overflow_y_scroll()
                    .children(rows),
            );

        // Centre the popup horizontally; place it ~80 px below the
        // top of the viewport so it always has room above the
        // dialog's BASE trigger no matter the window height.
        let viewport_w: f32 = viewport.width.into();
        let popup_x = (viewport_w - 360.0) / 2.0;
        deferred(
            anchored()
                .position(point(px(popup_x.max(8.0)), px(80.0)))
                .child(popup),
        )
        .with_priority(20)
        .into_any_element()
    }
}

/// Top-level key handler for the dialog. Mutates the active
/// `NewWorktreeDialogState` directly because gpui's listener helper
/// gives us `&mut Sidebar` — there's nowhere to hang per-state
/// listeners that are also `Send + 'static`.
///
/// Behaviour fans out by `state.focused_field` and `base_popup_open`:
/// - Branch / Folder field → typing appends to that field, Enter
///   submits, Escape cancels, Tab cycles focus.
/// - Base popup search → typing filters, Up/Down moves selection,
///   Enter confirms, Escape closes the popup (without cancelling
///   the dialog).
fn handle_key_down(
    sidebar: &mut Sidebar,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<Sidebar>,
) {
    let key = event.keystroke.key.as_str();
    cx.stop_propagation();

    // Snapshot popup state up front since we need it across multiple
    // match arms and the borrow rules don't allow holding a `&state`
    // across `sidebar.method()` calls.
    let popup_open = sidebar
        .dialog()
        .map(|s| s.base_popup_open)
        .unwrap_or(false);

    match key {
        "escape" => {
            // Escape inside an open popup just closes the popup —
            // matches the C# `OnBaseSearchKeyDown`. Otherwise it
            // cancels the whole dialog.
            if popup_open {
                sidebar.toggle_base_popup(cx);
            } else {
                sidebar.cancel_new_worktree_dialog(cx);
            }
            return;
        }
        "enter" => {
            if popup_open {
                sidebar.confirm_base_popup_selection(cx);
            } else {
                sidebar.submit_new_worktree_dialog(cx);
            }
            return;
        }
        "tab" => {
            // Cycle focus BRANCH → FOLDER → BRANCH. Skip when the
            // popup is open (it has its own keyboard model).
            if !popup_open
                && let Some(state) = sidebar.dialog_mut()
            {
                state.focused_field = match state.focused_field {
                    DialogField::Branch => DialogField::Folder,
                    DialogField::Folder => DialogField::Branch,
                    DialogField::BasePopupSearch => DialogField::Branch,
                };
                cx.notify();
            }
            return;
        }
        "up" if popup_open => {
            sidebar.move_base_popup_selection(-1, cx);
            return;
        }
        "down" if popup_open => {
            sidebar.move_base_popup_selection(1, cx);
            return;
        }
        "backspace" => {
            let touched =
                sidebar.dialog_mut().map(|s| s.backspace()).unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "delete" => {
            let touched =
                sidebar.dialog_mut().map(|s| s.delete_forward()).unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "left" => {
            let touched =
                sidebar.dialog_mut().map(|s| s.move_caret_left()).unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "right" => {
            let touched =
                sidebar.dialog_mut().map(|s| s.move_caret_right()).unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "home" => {
            let touched =
                sidebar.dialog_mut().map(|s| s.move_caret_home()).unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "end" => {
            let touched =
                sidebar.dialog_mut().map(|s| s.move_caret_end()).unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "space" => {
            let changed =
                sidebar.dialog_mut().map(|s| s.insert_char(' ')).unwrap_or(false);
            if changed {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        _ => {}
    }

    // Anything else: insert the typed character if gpui surfaced one
    // and it isn't a control char (Enter/Tab/etc would otherwise
    // sneak in here on platforms that report them as `key_char`).
    let Some(key_char) = event.keystroke.key_char.as_deref() else {
        return;
    };
    if key_char.is_empty() {
        return;
    }
    let mut changed = false;
    if let Some(state) = sidebar.dialog_mut() {
        for ch in key_char.chars() {
            if !ch.is_control() && state.insert_char(ch) {
                changed = true;
            }
        }
    }
    if changed {
        sidebar.wake_text_blink(cx);
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_replaces_path_separators() {
        assert_eq!(sanitize_branch_to_folder("feat/foo"), "feat-foo");
        assert_eq!(sanitize_branch_to_folder(r"feat\bar"), "feat-bar");
    }

    #[test]
    fn sanitize_replaces_windows_invalid_chars() {
        assert_eq!(sanitize_branch_to_folder("a?b*c|d"), "a-b-c-d");
        assert_eq!(sanitize_branch_to_folder(r#"a:b"c<d>e"#), "a-b-c-d-e");
    }

    #[test]
    fn sanitize_trims_leading_trailing_dashes() {
        // C# behaviour: `///` collapses to `-` triples then trims to "".
        assert_eq!(sanitize_branch_to_folder("///"), "");
        assert_eq!(sanitize_branch_to_folder("/feat/"), "feat");
    }

    #[test]
    fn derived_folder_uses_backslash_when_root_is_windows_style() {
        let path = derived_folder(r"C:\repos\proj.worktrees", "feat/x");
        assert_eq!(path, r"C:\repos\proj.worktrees\feat-x");
    }

    #[test]
    fn derived_folder_uses_slash_for_unix_style_root() {
        let path = derived_folder("/home/me/proj.worktrees", "feat/x");
        assert_eq!(path, "/home/me/proj.worktrees/feat-x");
    }

    #[test]
    fn derived_folder_empty_when_branch_sanitises_to_empty() {
        assert_eq!(derived_folder("/root", "///"), "");
    }

    #[test]
    fn folder_leaf_extracts_last_segment() {
        assert_eq!(folder_leaf(r"C:\a\b\feat-x"), "feat-x");
        assert_eq!(folder_leaf("/a/b/feat-x"), "feat-x");
        assert_eq!(folder_leaf("no-separators"), "no-separators");
    }

    // ─── State / filtering tests ─────────────────────────────────
    //
    // The dialog's pure helpers (`filter_branches`,
    // `resolve_default_base`) are extracted as free functions so we
    // can test them without constructing a `NewWorktreeDialogState`
    // (which carries a `FocusHandle` we can't forge outside a gpui
    // context).

    fn branch(name: &str, is_remote: bool) -> BranchInfo {
        BranchInfo {
            name: name.into(),
            is_remote,
            short_sha: "abcdef0".into(),
            relative_date: "2 days ago".into(),
        }
    }

    #[test]
    fn filter_branches_orders_locals_before_remotes() {
        let bs = vec![
            branch("origin/main", true),
            branch("main", false),
            branch("feat/x", false),
            branch("origin/feat/x", true),
        ];
        let names: Vec<&str> = filter_branches(&bs, "")
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        // Locals come first (in their input order), remotes after.
        assert_eq!(names, vec!["main", "feat/x", "origin/main", "origin/feat/x"]);
    }

    #[test]
    fn filter_branches_query_is_case_insensitive_substring() {
        let bs = vec![
            branch("Main", false),
            branch("origin/main", true),
            branch("feat/csv", false),
        ];
        let names: Vec<&str> = filter_branches(&bs, "MAIN")
            .iter()
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(names, vec!["Main", "origin/main"]);
    }

    #[test]
    fn filter_branches_empty_query_returns_all() {
        let bs = vec![branch("main", false), branch("origin/main", true)];
        assert_eq!(filter_branches(&bs, "").len(), 2);
        assert_eq!(filter_branches(&bs, "   ").len(), 2, "whitespace trims to empty");
    }

    #[test]
    fn resolve_default_base_picks_local_match() {
        let bs = vec![
            branch("main", false),
            branch("dev", false),
            branch("origin/dev", true),
        ];
        assert_eq!(resolve_default_base(&bs, "dev").as_deref(), Some("dev"));
    }

    #[test]
    fn resolve_default_base_ignores_remotes() {
        // Only `origin/dev` exists; we don't fall back to it because
        // the C# build's default-base lookup is "first local match"
        // — and there are no locals here, so the result is `None`.
        let bs = vec![branch("origin/dev", true)];
        assert!(resolve_default_base(&bs, "dev").is_none());
    }

    #[test]
    fn resolve_default_base_is_none_when_list_empty() {
        let bs: Vec<BranchInfo> = vec![];
        assert!(resolve_default_base(&bs, "main").is_none());
    }

    #[test]
    fn resolve_default_base_falls_back_to_first_local_when_default_missing() {
        // `default_branch = "master"` doesn't exist locally; the
        // C# fallback picks the first local branch. Without this
        // step the dialog would default to `(HEAD)` even when there
        // was an obvious local choice — common after a `master` →
        // `main` rename that hasn't been pulled.
        let bs = vec![
            branch("main", false),
            branch("feat/x", false),
            branch("origin/main", true),
        ];
        assert_eq!(
            resolve_default_base(&bs, "master").as_deref(),
            Some("main"),
            "first local branch wins when default_branch is missing"
        );
    }

    #[test]
    fn resolve_default_base_fallback_skips_remotes() {
        // Even when there's a remote that matches default_branch
        // and no exact local match, fallback must stay on locals.
        let bs = vec![
            branch("origin/master", true),
            branch("dev", false),
        ];
        assert_eq!(
            resolve_default_base(&bs, "master").as_deref(),
            Some("dev")
        );
    }
}

