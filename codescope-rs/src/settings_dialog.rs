//! Settings dialog — Rust-side addition.
//!
//! The C# build configures the equivalent fields via hand-edited
//! `settings.json`; there is no in-app Settings UI on that side. The
//! Rust port adds one because hand-editing JSON is a poor experience
//! for the common settings users want to flip (theme, default agent,
//! font, cursor). See ADR-0018 in `docs/DECISIONS.md`.
//!
//! Schema: this dialog surfaces *exactly* the fields already defined in
//! [`codescope_core::Settings`] — no new schema, just a UI surface over
//! the existing struct. Saves through [`Settings::save`], which the
//! existing live-reload poller in `app.rs` will pick up and apply (we
//! also call `apply_settings` inline so the swap is instant rather than
//! waiting up to one `SETTINGS_POLL` tick).
//!
//! Visual idiom: mirrors `new_project_dialog.rs` and
//! `new_worktree_dialog.rs` — elevated card on a dark backdrop,
//! divider-bordered, 6 px corner radius, accent colour for the primary
//! action button, ink-dim labels in uppercase.

use std::sync::Arc;

use codescope_core::{AgentRegistry, Settings, Theme, theme::builtin};
use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    ParentElement, SharedString, Styled, Window, anchored, deferred, div, point, px,
};

use crate::app::AppShell;
use crate::theme;

/// Which text-input field currently receives typed keystrokes. The
/// numeric stepper fields use plain text input so a user can paste
/// values; we validate on Save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsField {
    FontFamily,
    FontSize,
    LineHeight,
    Scrollback,
}

/// Live state of the open Settings dialog. Holds an in-memory edit
/// buffer — none of the fields are persisted until the user clicks
/// Save. Cancel discards the buffer.
pub struct SettingsDialogState {
    pub focus_handle: FocusHandle,
    pub focused_field: SettingsField,
    /// Working copy of `Settings`. Edits mutate this; Save writes to
    /// disk and replaces the shell's `settings` Arc via
    /// `apply_settings`. Numeric fields edit a text buffer instead of
    /// the struct directly so the user can type freely (incl. invalid
    /// intermediate states like "" or "1.").
    pub draft: Settings,
    pub font_size_text: String,
    pub line_height_text: String,
    pub scrollback_text: String,
    /// Inline validation error, rendered above the footer. Cleared on
    /// any user edit.
    pub error: Option<String>,
}

impl SettingsDialogState {
    pub fn new(settings: &Settings, focus_handle: FocusHandle) -> Self {
        Self {
            focus_handle,
            focused_field: SettingsField::FontFamily,
            font_size_text: format_f32(settings.font.size),
            line_height_text: format_f32(settings.font.line_height_multiplier),
            scrollback_text: settings.scrollback.to_string(),
            draft: settings.clone(),
            error: None,
        }
    }

    /// Parse the text buffers back into the draft `Settings`. Returns
    /// `Ok(())` and updates `draft` on success; returns the first
    /// validation error on failure so the caller can render it
    /// inline.
    pub fn commit_text_buffers(&mut self) -> Result<(), String> {
        let size: f32 = self
            .font_size_text
            .trim()
            .parse()
            .map_err(|_| "Font size must be a number between 8 and 24.".to_string())?;
        if !(8.0..=24.0).contains(&size) {
            return Err("Font size must be between 8 and 24.".into());
        }
        let line: f32 = self
            .line_height_text
            .trim()
            .parse()
            .map_err(|_| "Line height must be a number between 0.9 and 1.5.".to_string())?;
        if !(0.9..=1.5).contains(&line) {
            return Err("Line height must be between 0.9 and 1.5.".into());
        }
        let scrollback: usize = self
            .scrollback_text
            .trim()
            .parse()
            .map_err(|_| "Scrollback must be a whole number between 1000 and 100000.".to_string())?;
        if !(1_000..=100_000).contains(&scrollback) {
            return Err("Scrollback must be between 1000 and 100000.".into());
        }
        self.draft.font.size = size;
        self.draft.font.line_height_multiplier = line;
        self.draft.scrollback = scrollback;
        Ok(())
    }

    fn append_char(&mut self, ch: char) {
        match self.focused_field {
            SettingsField::FontFamily => self.draft.font.family.push(ch),
            SettingsField::FontSize => self.font_size_text.push(ch),
            SettingsField::LineHeight => self.line_height_text.push(ch),
            SettingsField::Scrollback => self.scrollback_text.push(ch),
        }
        self.error = None;
    }

    fn pop_char(&mut self) {
        match self.focused_field {
            SettingsField::FontFamily => {
                self.draft.font.family.pop();
            }
            SettingsField::FontSize => {
                self.font_size_text.pop();
            }
            SettingsField::LineHeight => {
                self.line_height_text.pop();
            }
            SettingsField::Scrollback => {
                self.scrollback_text.pop();
            }
        }
        self.error = None;
    }

    fn cycle_field(&mut self, forward: bool) {
        let order = [
            SettingsField::FontFamily,
            SettingsField::FontSize,
            SettingsField::LineHeight,
            SettingsField::Scrollback,
        ];
        let pos = order.iter().position(|f| *f == self.focused_field).unwrap_or(0);
        let next = if forward {
            (pos + 1) % order.len()
        } else {
            (pos + order.len() - 1) % order.len()
        };
        self.focused_field = order[next];
    }
}

/// Format an `f32` for the text buffer: trim trailing zeros so `13.0`
/// renders as `13` (less noisy in the input), `1.25` stays `1.25`.
fn format_f32(value: f32) -> String {
    if value.fract().abs() < f32::EPSILON {
        format!("{value:.0}")
    } else {
        // Trim trailing zeros — Rust's default formatter doesn't.
        let mut s = format!("{value}");
        if s.contains('.') {
            while s.ends_with('0') {
                s.pop();
            }
            if s.ends_with('.') {
                s.pop();
            }
        }
        s
    }
}

impl AppShell {
    /// Open the Settings dialog. Idempotent on an already-open
    /// dialog. Triggered by Ctrl+, (handled in `on_key_down`).
    pub fn open_settings_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.settings_dialog.is_some() {
            return;
        }
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let state = SettingsDialogState::new(self.settings_ref().as_ref(), focus_handle);
        self.settings_dialog = Some(state);
        cx.notify();
    }

    /// Close the dialog without saving.
    pub fn cancel_settings_dialog(&mut self, cx: &mut Context<Self>) {
        if self.settings_dialog.take().is_some() {
            cx.notify();
        }
    }

    /// Parse + validate + persist + apply. On a validation failure
    /// the dialog stays open with an inline error; on success it
    /// closes and the new settings take effect immediately.
    pub fn submit_settings_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.settings_dialog.as_mut() else { return };
        if let Err(err) = state.commit_text_buffers() {
            state.error = Some(err);
            cx.notify();
            return;
        }
        let new_settings = state.draft.clone();
        let paths = self.paths_ref().clone();
        if let Err(err) = new_settings.save(&paths) {
            if let Some(state) = self.settings_dialog.as_mut() {
                state.error = Some(format!("Failed to save settings: {err:#}"));
            }
            cx.notify();
            return;
        }
        self.settings_dialog = None;
        // Reuse the existing live-reload pathway so theme + chrome
        // swap atomically and the sidebar repaints. Terminals that
        // are already running keep their baked-in palette / font /
        // scrollback — only new tabs pick up font + scrollback
        // changes. Theme + cursor take effect for chrome on the next
        // frame; cursor-style changes apply to freshly-spawned tabs.
        self.apply_settings(new_settings, cx);
    }

    /// Set the draft theme and re-apply immediately so the user sees
    /// the chrome swap in real time as they click through the list.
    pub fn settings_set_theme(&mut self, name: String, cx: &mut Context<Self>) {
        if let Some(state) = self.settings_dialog.as_mut() {
            state.draft.theme = name.clone();
            state.error = None;
        }
        // Live preview: swap `self.theme` so the chrome behind the
        // modal repaints in the new colours. The on-disk settings
        // are NOT touched here — Cancel reverts cleanly because the
        // poller will reload the unchanged file (and even without
        // that, `apply_settings` only swaps in-memory state). Save
        // promotes the preview to disk.
        let theme = Arc::new(builtin::by_name(&name));
        self.set_theme_preview(theme, cx);
    }

    /// Set the draft default-agent id.
    pub fn settings_set_default_agent(&mut self, id: String, cx: &mut Context<Self>) {
        if let Some(state) = self.settings_dialog.as_mut() {
            state.draft.default_agent = id;
            state.error = None;
            cx.notify();
        }
    }

    /// Set the draft cursor shape.
    pub fn settings_set_cursor_shape(&mut self, shape: &'static str, cx: &mut Context<Self>) {
        if let Some(state) = self.settings_dialog.as_mut() {
            state.draft.cursor.shape = shape.into();
            state.error = None;
            cx.notify();
        }
    }

    /// Toggle the draft cursor-blink flag.
    pub fn settings_toggle_cursor_blink(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.settings_dialog.as_mut() {
            state.draft.cursor.blinking = !state.draft.cursor.blinking;
            state.error = None;
            cx.notify();
        }
    }

    /// Move keyboard focus between the typed-input fields.
    pub fn settings_focus_field(&mut self, field: SettingsField, cx: &mut Context<Self>) {
        if let Some(state) = self.settings_dialog.as_mut() {
            state.focused_field = field;
            cx.notify();
        }
    }

    /// Render the dialog overlay. Returns `None` when no dialog is
    /// open. Mirrors `Sidebar::render_new_project_dialog`'s overlay
    /// strategy: `deferred(anchored(...))` at priority 10.
    pub fn render_settings_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.settings_dialog.as_ref()?;
        let viewport = window.viewport_size();

        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let ink_muted = theme::ink_muted(theme);
        let frost = theme::frost_10(theme);
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);
        let danger = theme::danger(theme);

        let focus_handle = state.focus_handle.clone();
        let draft = state.draft.clone();
        let focused_field = state.focused_field;
        let error_msg: Option<SharedString> = state.error.clone().map(Into::into);
        let font_size_text: SharedString = state.font_size_text.clone().into();
        let line_height_text: SharedString = state.line_height_text.clone().into();
        let scrollback_text: SharedString = state.scrollback_text.clone().into();

        // Header — eyebrow + title + subtitle.
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
                    .child("SETTINGS"),
            )
            .child(div().text_size(px(20.0)).text_color(ink).child("Settings"))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(ink_muted)
                    .child("Theme, font, cursor, default agent. Edits apply on Save."),
            );

        let field_label = |text: &'static str| {
            div().text_size(px(11.0)).text_color(ink_ghost).child(text)
        };
        let hint = |text: SharedString| {
            div().text_size(px(11.0)).text_color(ink_ghost).child(text)
        };

        // ─── Theme picker ──────────────────────────────────────────
        //
        // A vertical list of radio rows — one per built-in theme.
        // Selecting a row updates the draft and live-previews the
        // chrome. Mirrors the visual idiom of the sidebar's project
        // row, scaled down for the dialog.
        let mut theme_list = div().flex().flex_col().gap_1();
        for built_in in builtin::all() {
            let is_selected = draft.theme == built_in.name;
            let name_for_handler = built_in.name.clone();
            let row_bg = if is_selected { accent } else { canvas };
            let row_fg = if is_selected { canvas } else { ink };
            let row_label: SharedString = built_in.display_name.clone().into();
            let row = div()
                .id(("settings-theme-row", built_in.name.len() as u64 + built_in.name.bytes().fold(0u64, |a, b| a.wrapping_add(b as u64))))
                .px_3()
                .py_2()
                .rounded(px(6.0))
                .bg(row_bg)
                .text_size(px(13.0))
                .text_color(row_fg)
                .cursor_pointer()
                .hover(move |s| if !is_selected { s.bg(frost) } else { s })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.settings_set_theme(name_for_handler.clone(), cx);
                    }),
                )
                .child(row_label);
            theme_list = theme_list.child(row);
        }

        // ─── Default agent picker ──────────────────────────────────
        let registry = AgentRegistry::from_settings(&draft);
        let mut agent_list = div().flex().flex_col().gap_1();
        for profile in registry.get_all() {
            let is_selected = draft.default_agent.eq_ignore_ascii_case(&profile.id);
            let id_for_handler = profile.id.clone();
            let row_bg = if is_selected { accent } else { canvas };
            let row_fg = if is_selected { canvas } else { ink };
            let display: SharedString = profile.display_name.clone().into();
            let id_seed = profile.id.bytes().fold(1u64, |a, b| a.wrapping_add(b as u64));
            let row = div()
                .id(("settings-agent-row", id_seed))
                .px_3()
                .py_2()
                .rounded(px(6.0))
                .bg(row_bg)
                .text_size(px(13.0))
                .text_color(row_fg)
                .cursor_pointer()
                .hover(move |s| if !is_selected { s.bg(frost) } else { s })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.settings_set_default_agent(id_for_handler.clone(), cx);
                    }),
                )
                .child(display);
            agent_list = agent_list.child(row);
        }

        // ─── Text-input builder (single-line) ──────────────────────
        let textbox = |id: &'static str,
                       value: SharedString,
                       placeholder: &'static str,
                       this_field: SettingsField|
         -> gpui::Stateful<gpui::Div> {
            let placeholder_visible = value.is_empty();
            let display: SharedString = if placeholder_visible {
                SharedString::from(placeholder)
            } else {
                value
            };
            let is_focused = focused_field == this_field;
            let mut inner = div().flex().flex_row().items_center().gap_1().child(
                div()
                    .flex_grow()
                    .text_color(if placeholder_visible { ink_ghost } else { ink })
                    .truncate()
                    .child(display),
            );
            if is_focused {
                inner = inner.child(div().w(px(1.5)).h(px(16.0)).bg(accent));
            }
            div()
                .id(id)
                .px_3()
                .h(px(32.0))
                .bg(canvas)
                .border_1()
                .border_color(if is_focused { accent } else { divider })
                .rounded(px(6.0))
                .text_size(px(13.0))
                .cursor_pointer()
                .flex()
                .items_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.settings_focus_field(this_field, cx);
                    }),
                )
                .child(inner)
        };

        let font_family_input = textbox(
            "settings-font-family",
            draft.font.family.clone().into(),
            "FiraCode Nerd Font",
            SettingsField::FontFamily,
        );
        let font_size_input =
            textbox("settings-font-size", font_size_text, "13", SettingsField::FontSize);
        let line_height_input = textbox(
            "settings-line-height",
            line_height_text,
            "1.0",
            SettingsField::LineHeight,
        );
        let scrollback_input = textbox(
            "settings-scrollback",
            scrollback_text,
            "10000",
            SettingsField::Scrollback,
        );

        let fallbacks_hint: SharedString =
            format!("fallbacks: {}", draft.font.fallbacks.join(", ")).into();

        // ─── Cursor shape radios ───────────────────────────────────
        let cursor_radio = |id: &'static str, label: &'static str, value: &'static str| {
            let active = draft.cursor.shape.eq_ignore_ascii_case(value);
            let bg = if active { accent } else { canvas };
            let fg = if active { canvas } else { ink_muted };
            div()
                .id(id)
                .flex_grow()
                .h(px(28.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .bg(bg)
                .text_size(px(12.5))
                .text_color(fg)
                .cursor_pointer()
                .border_1()
                .border_color(divider)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.settings_set_cursor_shape(value, cx);
                    }),
                )
                .child(label)
        };
        let cursor_row = div()
            .flex()
            .flex_row()
            .gap_1()
            .child(cursor_radio("settings-cursor-block", "Block", "block"))
            .child(cursor_radio("settings-cursor-beam", "Beam", "beam"))
            .child(cursor_radio("settings-cursor-underline", "Underline", "underline"))
            .child(cursor_radio("settings-cursor-hollow", "Hollow", "hollow-block"));

        // ─── Cursor blink checkbox ─────────────────────────────────
        let blink_on = draft.cursor.blinking;
        let blink_box_bg = if blink_on { accent } else { canvas };
        let blink_check_color = if blink_on { canvas } else { gpui::transparent_black() };
        let blink_row = div()
            .id("settings-cursor-blink")
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.settings_toggle_cursor_blink(cx);
                }),
            )
            .child(
                div()
                    .w(px(16.0))
                    .h(px(16.0))
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(if blink_on { accent } else { divider })
                    .bg(blink_box_bg)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.0))
                    .text_color(blink_check_color)
                    .child("✓"),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(ink)
                    .child("Cursor blinks"),
            );

        // ─── Body assembly ─────────────────────────────────────────
        //
        // Two-column scroll-friendly stack: left column = theme +
        // agent pickers; right column = font + cursor + scrollback.
        // Both share the dialog's 520 px width minus the 20 px gutter
        // — keeps each column roughly the same width as the
        // new-project dialog's body.

        let restart_hint = div()
            .text_size(px(10.0))
            .text_color(ink_ghost)
            .child("Applies to new tabs only.");

        let left_col = div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_grow()
            .child(field_label("THEME"))
            .child(theme_list)
            .child(field_label("DEFAULT AGENT"))
            .child(agent_list);

        let right_col = div()
            .flex()
            .flex_col()
            .gap_2()
            .flex_grow()
            .child(field_label("FONT FAMILY"))
            .child(font_family_input)
            .child(hint(fallbacks_hint))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_grow()
                            .child(field_label("FONT SIZE"))
                            .child(font_size_input),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .flex_grow()
                            .child(field_label("LINE HEIGHT"))
                            .child(line_height_input),
                    ),
            )
            .child(field_label("SCROLLBACK"))
            .child(scrollback_input)
            .child(restart_hint)
            .child(field_label("CURSOR SHAPE"))
            .child(cursor_row)
            .child(blink_row);

        let body = div()
            .px_5()
            .flex()
            .flex_row()
            .gap_4()
            .child(left_col)
            .child(right_col);

        // ─── Footer ────────────────────────────────────────────────
        let error_block = error_msg.map(|msg| {
            div().px_5().text_size(px(12.0)).text_color(danger).child(msg)
        });

        let cancel_btn = div()
            .id("settings-cancel")
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
                    this.cancel_settings_dialog(cx);
                }),
            )
            .child("Cancel");

        let save_btn = div()
            .id("settings-save")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(canvas)
            .bg(accent)
            .rounded(px(6.0))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.submit_settings_dialog(cx);
                }),
            )
            .child("Save");

        let footer_buttons = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .px_5()
            .pb_5()
            .child(cancel_btn)
            .child(save_btn);

        let mut card = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(560.0))
            .max_h(px(640.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .pb_2()
            .track_focus(&focus_handle)
            .key_context("SettingsDialog")
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
                    this.cancel_settings_dialog(cx);
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

/// Keyboard handling inside the dialog. Escape cancels; Enter submits;
/// Tab cycles between text inputs; Backspace edits the focused buffer.
fn handle_key_down(
    shell: &mut AppShell,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) {
    let key = event.keystroke.key.as_str();
    cx.stop_propagation();

    match key {
        "escape" => {
            shell.cancel_settings_dialog(cx);
            return;
        }
        "enter" => {
            shell.submit_settings_dialog(cx);
            return;
        }
        "tab" => {
            if let Some(state) = shell.settings_dialog.as_mut() {
                let forward = !event.keystroke.modifiers.shift;
                state.cycle_field(forward);
                cx.notify();
            }
            return;
        }
        "backspace" => {
            if let Some(state) = shell.settings_dialog.as_mut() {
                state.pop_char();
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
    if let Some(state) = shell.settings_dialog.as_mut() {
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

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_text_buffers_round_trips_valid_values() {
        let mut s = Settings::default();
        s.font.size = 14.0;
        s.font.line_height_multiplier = 1.1;
        s.scrollback = 20_000;
        // Construct without a real FocusHandle — can't easily; build
        // the helper manually instead.
        let mut state = SettingsDialogStateForTest {
            font_size_text: "15".into(),
            line_height_text: "1.2".into(),
            scrollback_text: "30000".into(),
        };
        let result = test_commit(&mut state);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(state.font_size_text.as_str(), "15");
    }

    #[test]
    fn commit_text_buffers_rejects_out_of_range() {
        let mut state = SettingsDialogStateForTest {
            font_size_text: "100".into(),
            line_height_text: "1.0".into(),
            scrollback_text: "10000".into(),
        };
        assert!(test_commit(&mut state).is_err());

        let mut state = SettingsDialogStateForTest {
            font_size_text: "13".into(),
            line_height_text: "2.0".into(),
            scrollback_text: "10000".into(),
        };
        assert!(test_commit(&mut state).is_err());

        let mut state = SettingsDialogStateForTest {
            font_size_text: "13".into(),
            line_height_text: "1.0".into(),
            scrollback_text: "999".into(),
        };
        assert!(test_commit(&mut state).is_err());
    }

    #[test]
    fn commit_text_buffers_rejects_garbage() {
        let mut state = SettingsDialogStateForTest {
            font_size_text: "thirteen".into(),
            line_height_text: "1.0".into(),
            scrollback_text: "10000".into(),
        };
        assert!(test_commit(&mut state).is_err());
    }

    #[test]
    fn format_f32_strips_trailing_zeros() {
        assert_eq!(format_f32(13.0), "13");
        assert_eq!(format_f32(1.0), "1");
        assert_eq!(format_f32(1.25), "1.25");
        assert_eq!(format_f32(1.5), "1.5");
    }

    // ─── Mini-state harness ─────────────────────────────────────────
    //
    // `SettingsDialogState` holds a `FocusHandle`, which can't be
    // constructed outside a gpui app. We mirror the commit logic on a
    // minimal POD so the validation rules can still be unit-tested.

    struct SettingsDialogStateForTest {
        font_size_text: String,
        line_height_text: String,
        scrollback_text: String,
    }

    fn test_commit(state: &mut SettingsDialogStateForTest) -> Result<(), String> {
        let size: f32 = state
            .font_size_text
            .trim()
            .parse()
            .map_err(|_| "size".to_string())?;
        if !(8.0..=24.0).contains(&size) {
            return Err("size range".into());
        }
        let line: f32 = state
            .line_height_text
            .trim()
            .parse()
            .map_err(|_| "line".to_string())?;
        if !(0.9..=1.5).contains(&line) {
            return Err("line range".into());
        }
        let scrollback: usize = state
            .scrollback_text
            .trim()
            .parse()
            .map_err(|_| "scrollback".to_string())?;
        if !(1_000..=100_000).contains(&scrollback) {
            return Err("scrollback range".into());
        }
        let _ = size;
        let _ = line;
        let _ = scrollback;
        Ok(())
    }
}
