//! "New worktree from branch…" dialog — Rust port of
//! `src/CodeScope.Ui/Dialogs/NewWorktreeDialog.xaml.cs`.
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

use codescope_core::{Project, Theme, projects::Worktree};
use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, SharedString, Styled, Window, anchored, deferred, div, point, px,
};

use crate::sidebar::Sidebar;
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

/// Live state of an open dialog. Created by [`Sidebar::open_new_worktree_dialog`]
/// and dropped when the user confirms or cancels.
pub struct NewWorktreeDialogState {
    pub project_idx: usize,
    pub project_name: String,
    pub project_path: String,
    pub worktree_root: String,
    pub branch: String,
    pub folder: String,
    pub error: Option<String>,
    pub focus_handle: FocusHandle,
}

impl NewWorktreeDialogState {
    pub fn new(idx: usize, project: &Project, focus_handle: FocusHandle) -> Self {
        let worktree_root = project.worktree_root_path();
        Self {
            project_idx: idx,
            project_name: project.name.clone(),
            project_path: project.path.clone(),
            worktree_root,
            branch: String::new(),
            folder: String::new(),
            error: None,
            focus_handle,
        }
    }

    /// Mirrors the C# `RefreshValidity`: branch must be at least 2
    /// non-whitespace chars, and the derived folder must be non-empty
    /// (sanitisation can otherwise reduce a branch like `///` to "").
    pub fn is_valid(&self) -> bool {
        self.branch.trim().len() >= 2 && !self.folder.is_empty()
    }

    fn recompute_folder(&mut self) {
        self.folder = derived_folder(&self.worktree_root, &self.branch);
        // A typing change always invalidates a stale error message —
        // the user is correcting the input.
        self.error = None;
    }

    fn append_char(&mut self, ch: char) {
        self.branch.push(ch);
        self.recompute_folder();
    }

    fn pop_char(&mut self) {
        self.branch.pop();
        self.recompute_folder();
    }
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
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let state = NewWorktreeDialogState::new(idx, project, focus_handle);
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
        let branch = state.branch.trim().to_string();
        let folder = state.folder.clone();
        let project_path = state.project_path.clone();

        let result = codescope_core::git::add_worktree(
            Path::new(&project_path),
            Path::new(&folder),
            &branch,
            None,
        );
        match result {
            Ok(()) => {
                let new_wt = Worktree {
                    id: uuid::Uuid::new_v4().to_string(),
                    path: folder,
                    branch: Some(branch),
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
            }
            Err(err) => {
                let msg = err.to_string();
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
        let danger = theme::danger(theme);
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);

        let project_eyebrow: SharedString = state.project_name.to_uppercase().into();
        let branch_display: SharedString = if state.branch.is_empty() {
            SharedString::from("")
        } else {
            state.branch.clone().into()
        };
        let folder_display: SharedString = if state.folder.is_empty() {
            SharedString::from("…")
        } else {
            state.folder.clone().into()
        };
        let footer_branch: SharedString = if state.branch.is_empty() {
            SharedString::from("…")
        } else {
            state.branch.clone().into()
        };
        let footer_leaf: SharedString = if state.folder.is_empty() {
            SharedString::from("…")
        } else {
            folder_leaf(&state.folder).to_string().into()
        };
        let valid = state.is_valid();
        let error_msg: Option<SharedString> = state
            .error
            .as_ref()
            .map(|e| e.clone().into());
        let focus_handle = state.focus_handle.clone();

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

        // Branch field — div styled to look like a textbox. The thin
        // accent caret is appended only when the field is focused-and-
        // empty would be invisible otherwise; we always paint it for
        // simplicity since the dialog grabs focus on open and never
        // gives it up before close.
        let placeholder_visible = state.branch.is_empty();
        let branch_field_inner = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(
                div()
                    .flex_grow()
                    .text_color(if placeholder_visible { ink_ghost } else { ink })
                    .child(if placeholder_visible {
                        SharedString::from("e.g. feat/awesome")
                    } else {
                        branch_display
                    }),
            )
            .child(
                div()
                    .w(px(1.5))
                    .h(px(16.0))
                    .bg(accent),
            );

        let branch_field = div()
            .px_3()
            .py_2()
            .bg(canvas)
            .border_1()
            .border_color(divider)
            .rounded_md()
            .text_size(px(13.0))
            .child(branch_field_inner);

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
            .child(branch_field);

        // Read-only folder preview.
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
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(ink_dim)
                    .truncate()
                    .child(folder_display),
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
        // "git worktree add  ·  HEAD → <branch>  @  <leaf>"
        let footer_meta = div()
            .px_5()
            .text_size(px(11.0))
            .text_color(ink_muted)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child("git worktree add  ·  HEAD →")
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
            .child(folder_block);
        if let Some(eb) = error_block {
            card = card.child(eb);
        }
        card = card.child(footer_meta).child(footer_buttons);

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

        Some(
            deferred(
                anchored()
                    .position(point(px(0.0), px(0.0)))
                    .child(backdrop),
            )
            // Higher priority than the context menu so an opened
            // dialog always paints on top.
            .with_priority(10)
            .into_any_element(),
        )
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
}

/// Top-level key handler for the dialog. Mutates the active
/// `NewWorktreeDialogState` directly because gpui's listener helper
/// gives us `&mut Sidebar` — there's nowhere to hang per-state
/// listeners that are also `Send + 'static`.
fn handle_key_down(
    sidebar: &mut Sidebar,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<Sidebar>,
) {
    let key = event.keystroke.key.as_str();
    cx.stop_propagation();
    match key {
        "escape" => {
            sidebar.cancel_new_worktree_dialog(cx);
            return;
        }
        "enter" => {
            sidebar.submit_new_worktree_dialog(cx);
            return;
        }
        "backspace" => {
            if let Some(state) = sidebar.dialog_mut() {
                state.pop_char();
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
    if let Some(state) = sidebar.dialog_mut() {
        let mut changed = false;
        for ch in key_char.chars() {
            if !ch.is_control() {
                state.append_char(ch);
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }
}

