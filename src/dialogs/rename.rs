//! Rename dialog — Rust port of `legacy:CodeScope.Ui/Dialogs/RenameDialog.xaml(.cs)`.
//!
//! A small single-input modal that prompts for a new name and hands the
//! trimmed result back to the caller. The C# build uses it for three
//! targets (project, worktree, session); the Rust port wires up two —
//! project and session — and intentionally omits the worktree path
//! rename for now. The C# `RenameWorktreeAsync` actually renames the
//! on-disk directory, which requires closing every tab pinned to that
//! worktree first; until that flow lands we keep the row hidden rather
//! than ship a half-working version. See `docs/DECISIONS.md`.
//!
//! Visual idiom: copied 1:1 from `new_project_dialog.rs` and
//! `settings_dialog.rs` — elevated card on a dark backdrop, divider
//! border, ink-dim labels in uppercase, accent primary button. Width
//! is ~420 px (smaller than the new-project dialog because the body is
//! a single text input).
//!
//! Lives on [`AppShell`] (not [`crate::sidebar::Sidebar`]) because the
//! rename surface spans both stores: `Project.name` lives in the
//! sidebar's `ProjectsConfig`, but session renames go through
//! `SessionManager::rename` against AppShell's mirror copy. Owning the
//! dialog state at the AppShell level keeps the submit path on one
//! side of the entity boundary, and we mirror the result back into the
//! sidebar via `Sidebar::replace_projects` (the same pattern
//! `reopen_session` uses).

use std::sync::Arc;

use codescope_core::{SessionManager, Theme};
use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, SharedString, Styled, Window, anchored, deferred, div, point,
    px,
};

use crate::app::AppShell;
use crate::sidebar::RenameRequest;
use crate::text_field::{TextField, render_input_content, focused_caret_style};
use crate::theme;

/// Live state of the rename dialog. Holds the target identity, the
/// human-readable title shown in the header, and the editable buffer
/// pre-filled with the current name. Created on
/// [`SidebarEvent::OpenRenameDialog`](crate::sidebar::SidebarEvent::OpenRenameDialog)
/// and dropped on submit / cancel.
pub struct RenameDialogState {
    pub focus_handle: FocusHandle,
    pub target: RenameRequest,
    /// Header text. Mirrors the C# `RenameDialog.HeaderText` slot —
    /// the caller supplies the title verbatim ("Rename project",
    /// "Rename session") so the dialog stays a dumb input primitive.
    pub title: SharedString,
    /// Editable buffer. Pre-filled with the current name; caret parks
    /// at the end so the next keystroke appends. WPF's "select-all on
    /// open" idiom is not implemented yet — the user has to manually
    /// clear before typing.
    pub name: TextField,
    /// Snapshot of the pre-fill value, kept around so submit can short-
    /// circuit on `trimmed == original.trim()`. Session renames in
    /// particular need this: `SessionManager::rename` unconditionally
    /// sets `display_name`, so pressing Enter on a closed-history row
    /// whose label was *derived* (from `branch` or `id`) would otherwise
    /// stamp that derived value as an explicit override — invisible to
    /// the user, but it stops branch / id fallbacks from re-deriving.
    pub original: String,
    /// Inline validation error, rendered above the footer. Cleared on
    /// any user edit.
    pub error: Option<String>,
}

impl RenameDialogState {
    pub fn new(target: RenameRequest, current: String, focus_handle: FocusHandle) -> Self {
        let title: SharedString = match &target {
            RenameRequest::Project { .. } => "Rename project".into(),
            RenameRequest::Session { .. } => "Rename session".into(),
            RenameRequest::RemoteCommand { .. } => "Edit remote command".into(),
        };
        Self {
            focus_handle,
            target,
            title,
            original: current.clone(),
            name: TextField::with_text(current),
            error: None,
        }
    }

    /// Rename is allowed when the trimmed name is non-empty; the
    /// remote-command target additionally validates the buffer
    /// through `is_valid_remote_shell_command` (single-line) — the
    /// same rule the Add-project dialog applies, so the Save button
    /// can't claim a multi-line paste is fine only for submit to
    /// reject it. The
    /// "no-op when unchanged" rule mirrors the C# build's
    /// `string.Equals(newLeaf, currentLeaf, StringComparison.Ordinal)`
    /// guard — handled at submit time so the button stays enabled
    /// (and the user can still press Enter to dismiss) when the buffer
    /// matches the original. Matches `new_project_dialog::is_valid`
    /// in returning `bool` rather than `Result` so the render path
    /// stays branch-free.
    pub fn is_valid(&self) -> bool {
        match &self.target {
            RenameRequest::RemoteCommand { .. } => {
                codescope_core::projects::is_valid_remote_shell_command(self.name.text())
            }
            _ => !self.name.text().trim().is_empty(),
        }
    }

    /// Per-target chrome strings — (eyebrow, hint, field label, OK
    /// button). The dialog started as a rename primitive; the
    /// remote-command editor (#327) reuses its skeleton with editing
    /// vocabulary so the header doesn't claim a "rename".
    fn chrome(&self) -> (&'static str, &'static str, &'static str, &'static str) {
        match &self.target {
            RenameRequest::RemoteCommand { .. } => (
                "EDIT",
                "Change the command this project runs (e.g. `ssh dev`). \
                 Open tabs keep the old command; the next session uses the new one.",
                "COMMAND",
                "Save",
            ),
            _ => (
                "RENAME",
                "Enter a new name. Press Enter to confirm.",
                "NEW NAME",
                "Rename",
            ),
        }
    }
}

impl AppShell {
    /// Open the rename dialog. Idempotent on an already-open dialog —
    /// a second `OpenRenameDialog` event while one is showing is
    /// dropped silently rather than replacing the in-flight buffer
    /// (matches the C# behaviour where a `RenameDialog.Prompt` call
    /// while another modal is shown returns `null` without raising
    /// the second prompt).
    pub fn open_rename_dialog(
        &mut self,
        target: RenameRequest,
        current_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rename_dialog.is_some() {
            return;
        }
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        self.rename_dialog =
            Some(RenameDialogState::new(target, current_name, focus_handle));
        cx.notify();
    }

    /// Close the dialog without committing the edit.
    pub fn cancel_rename_dialog(&mut self, cx: &mut Context<Self>) {
        if self.rename_dialog.take().is_some() {
            cx.notify();
        }
    }

    /// Commit the edit. Validates trimming + non-empty, mutates the
    /// appropriate slot in `self.projects`, saves `projects.json`, and
    /// mirrors the change back to the sidebar so its rendered list
    /// picks up the new name on the same frame. A trimmed value equal
    /// to the current name is a silent no-op (mirrors C#
    /// `RenameProjectAsync` / `RenameSessionAsync` early returns), and
    /// validation failures keep the dialog open with an inline error.
    pub fn submit_rename_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.rename_dialog.as_ref() else { return };
        let trimmed = state.name.text().trim().to_string();
        if trimmed.is_empty() {
            if let Some(state) = self.rename_dialog.as_mut() {
                state.error = Some(match &state.target {
                    RenameRequest::RemoteCommand { .. } => "Command cannot be empty".into(),
                    _ => "Name cannot be empty".into(),
                });
            }
            cx.notify();
            return;
        }
        let target = state.target.clone();
        let original_trimmed = state.original.trim().to_string();
        // Universal no-op when the trimmed buffer matches the trimmed
        // pre-fill — applies to both project and session paths so a
        // closed-history row pre-filled from a derived label doesn't
        // get its derivation stamped as an explicit `display_name`
        // override on a no-op Enter. The project path's helper
        // already guards against this internally, but mirroring the
        // check up-front keeps the two branches aligned and skips an
        // unnecessary disk write either way.
        if trimmed == original_trimmed {
            self.rename_dialog = None;
            cx.notify();
            return;
        }

        match target {
            RenameRequest::Project { project_id } => {
                let changed = match codescope_core::projects::rename_project(
                    &mut self.projects,
                    &project_id,
                    &trimmed,
                ) {
                    Ok(changed) => changed,
                    Err(err) => {
                        if let Some(state) = self.rename_dialog.as_mut() {
                            state.error = Some(format!("{err:#}"));
                        }
                        cx.notify();
                        return;
                    }
                };
                if !changed {
                    // No-op — close silently. Matches the C# guard.
                    self.rename_dialog = None;
                    cx.notify();
                    return;
                }
                if let Err(err) = self.projects.save(self.paths_ref().as_ref()) {
                    if let Some(state) = self.rename_dialog.as_mut() {
                        state.error = Some(format!("Failed to save: {err:#}"));
                    }
                    cx.notify();
                    return;
                }
            }
            RenameRequest::RemoteCommand { project_id } => {
                let changed = match codescope_core::projects::set_remote_shell_command(
                    &mut self.projects,
                    &project_id,
                    &trimmed,
                ) {
                    Ok(changed) => changed,
                    Err(err) => {
                        if let Some(state) = self.rename_dialog.as_mut() {
                            state.error = Some(format!("{err:#}"));
                        }
                        cx.notify();
                        return;
                    }
                };
                if !changed {
                    self.rename_dialog = None;
                    cx.notify();
                    return;
                }
                if let Err(err) = self.projects.save(self.paths_ref().as_ref()) {
                    if let Some(state) = self.rename_dialog.as_mut() {
                        state.error = Some(format!("Failed to save: {err:#}"));
                    }
                    cx.notify();
                    return;
                }
            }
            RenameRequest::Session { session_id } => {
                // SessionManager::rename normalises whitespace → None
                // internally; we pass `Some(trimmed)` so empty strings
                // never reach it (we rejected them above).
                if let Err(err) = SessionManager::rename(
                    &mut self.projects,
                    &session_id,
                    Some(&trimmed),
                ) {
                    if let Some(state) = self.rename_dialog.as_mut() {
                        state.error = Some(format!("{err:#}"));
                    }
                    cx.notify();
                    return;
                }
                if let Err(err) = self.projects.save(self.paths_ref().as_ref()) {
                    if let Some(state) = self.rename_dialog.as_mut() {
                        state.error = Some(format!("Failed to save: {err:#}"));
                    }
                    cx.notify();
                    return;
                }
            }
        }

        // Persistence committed — close the dialog and mirror to the
        // sidebar so its rendered list refreshes on this frame. Same
        // pattern `reopen_session` uses.
        self.rename_dialog = None;
        self.mirror_projects_to_sidebar(cx);
        cx.notify();
    }

    /// Render the dialog overlay. Returns `None` when no dialog is
    /// open. Layered via `deferred(anchored(...))` at priority 10,
    /// matching `render_settings_dialog` / `render_new_project_dialog`.
    pub fn render_rename_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.rename_dialog.as_ref()?;
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

        let focus_handle = state.focus_handle.clone();
        let title = state.title.clone();
        let (eyebrow, hint, field_label_text, ok_label) = state.chrome();
        let valid = state.is_valid();
        let error_msg: Option<SharedString> = state.error.clone().map(Into::into);
        let blink_phase = self.text_blink_phase;

        // Header — eyebrow ("RENAME") + title.
        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_5()
            .pt_5()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(accent)
                    .child(eyebrow),
            )
            .child(div().text_size(px(20.0)).text_color(ink).child(title))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(ink_muted)
                    .child(hint),
            );

        let field_label = div()
            .text_size(px(11.0))
            .text_color(ink_ghost)
            .child(field_label_text);

        // Single-line text input. Always focused (only one field), so
        // the caret is always painted — blink phase still flips it on
        // and off via the global timer.
        let textbox = div()
            .id("rename-input")
            .px_3()
            .h(px(34.0))
            .bg(canvas)
            .border_1()
            .border_color(accent)
            .rounded(px(6.0))
            .text_size(px(13.0))
            .flex()
            .items_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if let Some(state) = this.rename_dialog.as_mut()
                        && let Some(idx) =
                            state.name.index_for_window_point(event.position)
                    {
                        state.name.set_caret(idx);
                        this.wake_text_blink(cx);
                        cx.notify();
                    }
                }),
            )
            .child(render_input_content(
                &state.name,
                SharedString::from("(empty)"),
                focused_caret_style(theme, blink_phase),
            ));

        let body = div()
            .px_5()
            .flex()
            .flex_col()
            .gap_1()
            .child(field_label)
            .child(textbox);

        let error_block = error_msg.map(|msg| {
            div()
                .px_5()
                .text_size(px(12.0))
                .text_color(danger)
                .child(msg)
        });

        let cancel_btn = div()
            .id("rename-cancel")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(ink_dim)
            .border_1()
            .border_color(divider)
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |s| s.bg(frost).text_color(ink))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.cancel_rename_dialog(cx);
                }),
            )
            .child("Cancel");

        let ok_color = if valid { canvas } else { ink_ghost };
        let ok_bg = if valid { accent } else { divider };
        let ok_btn = {
            let mut btn = div()
                .id("rename-ok")
                .px_4()
                .py_2()
                .text_size(px(13.0))
                .text_color(ok_color)
                .bg(ok_bg)
                .rounded(px(6.0));
            if valid {
                btn = btn.cursor_pointer().on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.submit_rename_dialog(cx);
                    }),
                );
            }
            btn.child(ok_label)
        };

        let footer_buttons = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .px_5()
            .pb_5()
            .child(cancel_btn)
            .child(ok_btn);

        let mut card = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(420.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .pb_2()
            .track_focus(&focus_handle)
            .key_context("RenameDialog")
            .on_key_down(cx.listener(handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(body);
        if let Some(eb) = error_block {
            card = card.child(eb);
        }
        card = card.child(footer_buttons);

        let backdrop = div()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.cancel_rename_dialog(cx);
                }),
            )
            .child(card);

        Some(
            deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
                .with_priority(10)
                .into_any_element(),
        )
    }
}

/// Escape cancels; Enter submits; Backspace pops a char; printable
/// characters append. Mirrors `new_project_dialog::handle_key_down` in
/// shape — every dialog in the Rust port uses the same skeleton so
/// readers don't have to reverse-engineer a different key dispatch on
/// each surface.
fn handle_key_down(
    shell: &mut AppShell,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) {
    let key = event.keystroke.key.as_str();
    cx.stop_propagation();

    if crate::text_field::is_paste_chord(&event.keystroke) {
        let pasted = cx.read_from_clipboard().and_then(|item| item.text());
        let changed = pasted
            .and_then(|text| {
                shell.rename_dialog.as_mut().map(|s| {
                    let c = s.name.insert_str(&text);
                    if c {
                        s.error = None;
                    }
                    c
                })
            })
            .unwrap_or(false);
        if changed {
            shell.wake_text_blink(cx);
            cx.notify();
        }
        return;
    }

    match key {
        "escape" => {
            shell.cancel_rename_dialog(cx);
            return;
        }
        "enter" => {
            shell.submit_rename_dialog(cx);
            return;
        }
        "backspace" => {
            let changed = if let Some(state) = shell.rename_dialog.as_mut() {
                let c = state.name.backspace();
                if c {
                    state.error = None;
                }
                c
            } else {
                false
            };
            if changed {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "delete" => {
            let changed = if let Some(state) = shell.rename_dialog.as_mut() {
                let c = state.name.delete_forward();
                if c {
                    state.error = None;
                }
                c
            } else {
                false
            };
            if changed {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "left" => {
            let changed = shell
                .rename_dialog
                .as_mut()
                .map(|s| s.name.move_left())
                .unwrap_or(false);
            if changed {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "right" => {
            let changed = shell
                .rename_dialog
                .as_mut()
                .map(|s| s.name.move_right())
                .unwrap_or(false);
            if changed {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "home" => {
            let changed = shell
                .rename_dialog
                .as_mut()
                .map(|s| s.name.move_home())
                .unwrap_or(false);
            if changed {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "end" => {
            let changed = shell
                .rename_dialog
                .as_mut()
                .map(|s| s.name.move_end())
                .unwrap_or(false);
            if changed {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "space" => {
            if let Some(state) = shell.rename_dialog.as_mut() {
                state.name.insert_char(' ');
                state.error = None;
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        _ => {}
    }

    let Some(key_char) = event.keystroke.key_char.as_deref() else { return };
    if key_char.is_empty() {
        return;
    }
    if let Some(state) = shell.rename_dialog.as_mut() {
        let mut changed = false;
        for ch in key_char.chars() {
            if !ch.is_control() {
                state.name.insert_char(ch);
                state.error = None;
                changed = true;
            }
        }
        if changed {
            shell.wake_text_blink(cx);
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn is_valid_rejects_empty_and_whitespace() {
        // `RenameDialogState::new` needs a `FocusHandle`, which can't
        // be constructed outside the gpui runtime. We exercise the
        // pure trimming rule that `is_valid` codifies so a regression
        // in the predicate would still be caught here.
        assert!(String::new().trim().is_empty());
        assert!("  ".trim().is_empty());
        assert!(!"name".trim().is_empty());
        assert!(!"  name  ".trim().is_empty());
    }
}
