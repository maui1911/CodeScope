//! Themed confirm dialog — Rust port of
//! `legacy:CodeScope.Ui/Dialogs/ConfirmDialog.xaml(.cs)`.
//!
//! Single in-app modal primitive for confirm / destructive prompts.
//! Replaces the OS-native `window.prompt(...)` calls that were
//! previously used for destructive sidebar actions (discard, remove
//! worktree, force-retry). The native dialog is functional but the
//! visual chrome doesn't match the rest of the in-app modals
//! (settings_dialog, rename_dialog, new_project_dialog).
//!
//! Visual idiom: mirrors `rename_dialog.rs` / `new_project_dialog.rs`
//! — elevated card on a dim backdrop, divider border, ink-dim labels,
//! accent-or-danger primary button. Width 440 px.
//!
//! API: returns a `oneshot::Receiver<bool>` (mirrors `window.prompt`'s
//! return type — a future that resolves to the chosen button). The
//! caller awaits it inside a `cx.spawn` task, exactly like the
//! `window.prompt` code path it replaces. `true` means confirm, `false`
//! means cancel / Esc / scrim click / dialog dropped.
//!
//! Two flavors mirror the C# `Flavor.Confirm` / `Flavor.Destructive`:
//!
//! * `Confirm` — neutral, accent primary, Esc cancels, Enter confirms.
//! * `Destructive` — danger primary (red), Esc cancels, Enter does NOT
//!   auto-confirm (the C# build hides the close glyph + disables Enter
//!   to force an explicit click on the destructive button — we follow
//!   suit). Scrim click also cancels rather than committing.
//!
//! Lives on [`AppShell`] (not [`crate::sidebar::Sidebar`]) following
//! the established dialog ownership pattern — `rename_dialog.rs`,
//! `settings_dialog.rs`, `new_project_dialog.rs` all sit at the shell
//! level so the deferred-anchored overlay paints over the full window
//! rather than being clipped to the sidebar column.

use std::sync::Arc;

use codescope_core::Theme;
use futures_channel::oneshot;
use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, SharedString, Styled, Window, anchored, deferred, div, point, px,
};

use crate::app::AppShell;
use crate::theme;

/// Confirm vs Destructive — mirrors the relevant subset of the C#
/// `ConfirmDialog.Flavor` enum (we omit `Info` for now; it's used by
/// startup notices on the C# side that the Rust port doesn't have a
/// parity hook for yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    /// Reversible action. Accent-blue primary button, Enter auto-
    /// commits, Esc / scrim click cancels. Eyebrow reads "CONFIRM".
    Confirm,
    /// Irreversible destructive action. Red primary button, Enter does
    /// NOT auto-commit (the user has to click the button to confirm).
    /// Esc still cancels. Eyebrow reads "DESTRUCTIVE".
    Destructive,
}

/// Caller-supplied content + flavor for one open dialog. Owned by
/// `ConfirmDialogState` while the dialog is showing. Strings are owned
/// (not `&str`) so the dialog outlives the call site after the spawned
/// async task awaits the result.
#[derive(Debug, Clone)]
pub struct ConfirmSpec {
    pub kind: ConfirmKind,
    pub title: SharedString,
    pub message: SharedString,
    /// Optional secondary line (file path, technical detail).
    /// Rendered slightly smaller and in `text_faint`. `None` hides
    /// the row entirely. Mirrors the C# `hint` slot's positioning;
    /// we use a body-level detail rather than the footer hint so the
    /// caller's typical "Path: …" line reads cleanly above the
    /// buttons.
    pub detail: Option<SharedString>,
    pub confirm_label: SharedString,
    pub cancel_label: SharedString,
}

#[allow(dead_code)] // `confirm` + `with_cancel_label` exist on the API surface
                    // for future callers; today every destructive site is a
                    // `Destructive`-flavor dialog with the default "Cancel" label.
impl ConfirmSpec {
    /// Shorthand for a reversible "Confirm" prompt.
    pub fn confirm(
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
    ) -> Self {
        Self {
            kind: ConfirmKind::Confirm,
            title: title.into(),
            message: message.into(),
            detail: None,
            confirm_label: SharedString::from("OK"),
            cancel_label: SharedString::from("Cancel"),
        }
    }

    /// Shorthand for an irreversible "Destructive" prompt.
    pub fn destructive(
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
    ) -> Self {
        Self {
            kind: ConfirmKind::Destructive,
            title: title.into(),
            message: message.into(),
            detail: None,
            confirm_label: SharedString::from("Delete"),
            cancel_label: SharedString::from("Cancel"),
        }
    }

    pub fn with_detail(mut self, detail: impl Into<SharedString>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_confirm_label(mut self, label: impl Into<SharedString>) -> Self {
        self.confirm_label = label.into();
        self
    }

    pub fn with_cancel_label(mut self, label: impl Into<SharedString>) -> Self {
        self.cancel_label = label.into();
        self
    }

    /// Trim whitespace-only labels back to a non-empty default. The
    /// dialog always renders both buttons; an empty label would leave
    /// a clickable but invisible button — that's a UX bug, not a
    /// feature. Pure helper so we can unit-test it.
    pub fn normalised_confirm_label(&self) -> SharedString {
        normalise_button_label(&self.confirm_label, "OK")
    }

    pub fn normalised_cancel_label(&self) -> SharedString {
        normalise_button_label(&self.cancel_label, "Cancel")
    }
}

/// Live state for one open dialog. The `responder` is taken on commit
/// / cancel and the result is sent back to the caller's spawned task.
/// Dropping the state without committing also drops the sender, which
/// causes the receiver to resolve to `Err(_)` — we treat that as a
/// cancel on the caller's side (`rx.await.unwrap_or(false)`).
pub struct ConfirmDialogState {
    pub focus_handle: FocusHandle,
    pub spec: ConfirmSpec,
    /// Sender half of the result oneshot. `Some` while the dialog is
    /// open; `take()` flips it to `None` on the first commit/cancel so
    /// double-clicks can't fire twice.
    responder: Option<oneshot::Sender<bool>>,
}

impl ConfirmDialogState {
    pub fn new(
        spec: ConfirmSpec,
        responder: oneshot::Sender<bool>,
        focus_handle: FocusHandle,
    ) -> Self {
        Self { focus_handle, spec, responder: Some(responder) }
    }
}

impl AppShell {
    /// Open the confirm dialog. Returns a `oneshot::Receiver<bool>`
    /// the caller awaits to learn the outcome. `true` = confirm,
    /// `false` = cancel / Esc / scrim. If a dialog is already open,
    /// the receiver immediately resolves to `false` (we don't queue
    /// dialogs).
    ///
    /// Mirrors the call-shape of `window.prompt(...)` it replaces —
    /// the caller spawns an async task and awaits the receiver.
    pub fn open_confirm_dialog(
        &mut self,
        spec: ConfirmSpec,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> oneshot::Receiver<bool> {
        let (tx, rx) = oneshot::channel();
        if self.confirm_dialog.is_some() {
            // Surface "another modal is already up" as a clean cancel
            // — same outcome the user would get by Esc-ing the in-
            // flight dialog. Mirrors the C# build's `ShowDialog()`
            // returning `false` when re-entrant prompting is rejected.
            let _ = tx.send(false);
            return rx;
        }
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        self.confirm_dialog = Some(ConfirmDialogState::new(spec, tx, focus_handle));
        cx.notify();
        rx
    }

    /// Commit the dialog — the user clicked the primary button or
    /// pressed Enter on a Confirm-flavor dialog.
    pub fn submit_confirm_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(mut state) = self.confirm_dialog.take() else { return };
        if let Some(tx) = state.responder.take() {
            let _ = tx.send(true);
        }
        cx.notify();
    }

    /// Cancel the dialog — Esc, scrim click, Cancel button, or
    /// close-glyph.
    pub fn cancel_confirm_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(mut state) = self.confirm_dialog.take() else { return };
        if let Some(tx) = state.responder.take() {
            let _ = tx.send(false);
        }
        cx.notify();
    }

    /// Render the dialog overlay. `None` when no dialog is open.
    /// Layered via `deferred(anchored(...))` at priority 10 so it
    /// paints over the rest of the frame (matching the other
    /// `render_*_dialog` helpers).
    pub fn render_confirm_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.confirm_dialog.as_ref()?;
        let viewport = window.viewport_size();

        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_muted = theme::ink_muted(theme);
        let frost = theme::frost_10(theme);
        let danger = theme::danger();
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);
        let text_faint = theme::text_faint();
        // `ink_ghost` is intentionally not bound — the C# spec reserves
        // a footer hint slot in that colour but the Rust port doesn't
        // render hints yet. Add it back if/when we wire that row in.

        let focus_handle = state.focus_handle.clone();
        let kind = state.spec.kind;
        let title = state.spec.title.clone();
        let message = state.spec.message.clone();
        let detail = state.spec.detail.clone();
        let confirm_label = state.spec.normalised_confirm_label();
        let cancel_label = state.spec.normalised_cancel_label();

        // Eyebrow text + colour: ACCENT for Confirm, DANGER for
        // Destructive. Matches the C# eyebrow rule.
        let (eyebrow_text, eyebrow_color) = match kind {
            ConfirmKind::Confirm => ("CONFIRM", accent),
            ConfirmKind::Destructive => ("DESTRUCTIVE", danger),
        };

        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_5()
            .pt_5()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(eyebrow_color)
                    .child(eyebrow_text),
            )
            .child(
                div()
                    .text_size(px(14.0))
                    .text_color(ink)
                    .font(theme::font_sans())
                    .child(title),
            );

        let body_text = div()
            .text_size(px(13.0))
            .text_color(ink_muted)
            .font(theme::font_sans())
            .child(message);

        let detail_block = detail.map(|d| {
            div()
                .text_size(px(11.5))
                .text_color(text_faint)
                .font(theme::font_sans())
                .child(d)
        });

        let mut body = div()
            .px_5()
            .pb_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(body_text);
        if let Some(d) = detail_block {
            body = body.child(d);
        }

        let cancel_btn = div()
            .id("confirm-cancel")
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
                    this.cancel_confirm_dialog(cx);
                }),
            )
            .child(cancel_label);

        let (primary_bg, primary_fg) = match kind {
            ConfirmKind::Confirm => (accent, canvas),
            ConfirmKind::Destructive => (danger, canvas),
        };
        let primary_btn = div()
            .id("confirm-ok")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(primary_fg)
            .bg(primary_bg)
            .rounded(px(6.0))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.submit_confirm_dialog(cx);
                }),
            )
            .child(confirm_label);

        let footer_buttons = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .px_5()
            .pb_5()
            .pt_2()
            .child(cancel_btn)
            .child(primary_btn);

        let card = div()
            .flex()
            .flex_col()
            .gap_2()
            .w(px(440.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .track_focus(&focus_handle)
            .key_context("ConfirmDialog")
            .on_key_down(cx.listener(handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(body)
            .child(footer_buttons);

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
                    // Scrim click cancels for both flavors. The C#
                    // build leaves scrim click as a no-op on
                    // destructive dialogs to force an explicit choice;
                    // we deliberately diverge here because the
                    // accidental-click risk is low in our flow (the
                    // sidebar is the only entry point) and treating
                    // scrim like Cancel matches every other dialog in
                    // the Rust port. Document if we revert.
                    this.cancel_confirm_dialog(cx);
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

/// Whitespace-only or empty labels collapse to the default. Pure
/// helper so we can exercise it without standing up a gpui runtime.
fn normalise_button_label(raw: &SharedString, default: &'static str) -> SharedString {
    if raw.trim().is_empty() {
        SharedString::from(default)
    } else {
        raw.clone()
    }
}

/// Escape cancels for both flavors. Enter commits only on Confirm —
/// Destructive requires an explicit button click (mirrors C#'s
/// `IsDefault=false` rule for the destructive flavor). All other keys
/// bubble.
fn handle_key_down(
    shell: &mut AppShell,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) {
    let key = event.keystroke.key.as_str();
    match key {
        "escape" => {
            cx.stop_propagation();
            shell.cancel_confirm_dialog(cx);
        }
        "enter" => {
            // Only Confirm-flavor dialogs auto-commit on Enter. The
            // user has to click the primary button on Destructive
            // — same friction the C# build adds for irreversible
            // actions.
            let is_confirm = shell
                .confirm_dialog
                .as_ref()
                .map(|s| s.spec.kind == ConfirmKind::Confirm)
                .unwrap_or(false);
            if is_confirm {
                cx.stop_propagation();
                shell.submit_confirm_dialog(cx);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalised_labels_fall_back_to_default_on_empty() {
        let spec = ConfirmSpec::destructive("t", "m").with_confirm_label("");
        assert_eq!(spec.normalised_confirm_label().as_ref(), "OK");
        let spec = ConfirmSpec::destructive("t", "m").with_cancel_label("   ");
        assert_eq!(spec.normalised_cancel_label().as_ref(), "Cancel");
    }

    #[test]
    fn normalised_labels_pass_through_real_text() {
        let spec = ConfirmSpec::destructive("t", "m")
            .with_confirm_label("Force remove")
            .with_cancel_label("Keep");
        assert_eq!(spec.normalised_confirm_label().as_ref(), "Force remove");
        assert_eq!(spec.normalised_cancel_label().as_ref(), "Keep");
    }

    #[test]
    fn with_detail_sets_the_detail_slot() {
        let spec = ConfirmSpec::confirm("t", "m").with_detail("Path: /foo");
        assert_eq!(spec.detail.as_ref().map(|s| s.as_ref()), Some("Path: /foo"));
    }

    #[test]
    fn confirm_and_destructive_kinds_are_distinct() {
        let a = ConfirmSpec::confirm("t", "m");
        let b = ConfirmSpec::destructive("t", "m");
        assert_eq!(a.kind, ConfirmKind::Confirm);
        assert_eq!(b.kind, ConfirmKind::Destructive);
    }
}
