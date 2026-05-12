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
    /// Snapshot of the on-disk settings + theme captured at the moment
    /// the dialog opened. The dialog mutates [`AppShell::theme`]
    /// directly for live preview as the user clicks through the theme
    /// list; on Cancel we hand `original_settings` back to
    /// `apply_settings` so the in-memory state matches disk again
    /// (the file-watch poller would not catch this because the file
    /// itself is never touched during a preview).
    pub original_settings: Settings,
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
            original_settings: settings.clone(),
            error: None,
        }
    }

    /// Parse the text buffers back into the draft `Settings`. Returns
    /// `Ok(())` and updates `draft` on success; returns the first
    /// validation error on failure so the caller can render it
    /// inline. Delegates to [`parse_numeric_fields`] so both the
    /// runtime path and the unit tests share the exact same
    /// validation rules.
    pub fn commit_text_buffers(&mut self) -> Result<(), String> {
        let (size, line, scrollback) = parse_numeric_fields(
            &self.font_size_text,
            &self.line_height_text,
            &self.scrollback_text,
        )?;
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

/// Pure validation helper for the three numeric input buffers
/// (font size, line-height multiplier, scrollback). Returns the
/// parsed triple on success, or the first user-facing error message
/// on failure. Pulled out as a free function so [`commit_text_buffers`]
/// and the unit tests exercise the exact same code path — the
/// previous arrangement had a parallel `test_commit` shim that could
/// silently drift from production.
fn parse_numeric_fields(
    font_size_text: &str,
    line_height_text: &str,
    scrollback_text: &str,
) -> Result<(f32, f32, usize), String> {
    let size: f32 = font_size_text
        .trim()
        .parse()
        .map_err(|_| "Font size must be a number between 8 and 24.".to_string())?;
    if !(8.0..=24.0).contains(&size) {
        return Err("Font size must be between 8 and 24.".into());
    }
    let line: f32 = line_height_text
        .trim()
        .parse()
        .map_err(|_| "Line height must be a number between 0.9 and 1.5.".to_string())?;
    if !(0.9..=1.5).contains(&line) {
        return Err("Line height must be between 0.9 and 1.5.".into());
    }
    let scrollback: usize = scrollback_text
        .trim()
        .parse()
        .map_err(|_| "Scrollback must be a whole number between 1000 and 100000.".to_string())?;
    if !(1_000..=100_000).contains(&scrollback) {
        return Err("Scrollback must be between 1000 and 100000.".into());
    }
    Ok((size, line, scrollback))
}

/// Hash a string id seed to a `u64` for use in gpui's `(static_str,
/// u64)` element ids. Mirrors the helper in `sidebar.rs` — keeping
/// each dialog file's id seeds collision-resistant prevents gpui
/// from confusing distinct rows that happen to share a byte-sum.
fn id_hash(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}

/// Render the font-fallback chain as a one-line read-only hint.
/// Truncates to the first 3 names + `…` when the chain is longer so
/// the line never overflows the dialog width regardless of how many
/// fallbacks the user has configured. The full list is applied at
/// runtime — this is purely a UI truncation.
const FALLBACKS_HINT_VISIBLE: usize = 3;
fn format_fallbacks_hint(fallbacks: &[String]) -> String {
    if fallbacks.is_empty() {
        return "fallbacks: (none)".to_string();
    }
    if fallbacks.len() <= FALLBACKS_HINT_VISIBLE {
        return format!("fallbacks: {}", fallbacks.join(", "));
    }
    let visible = fallbacks[..FALLBACKS_HINT_VISIBLE].join(", ");
    let hidden = fallbacks.len() - FALLBACKS_HINT_VISIBLE;
    format!("fallbacks: {visible}, … (+{hidden} more)")
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
    /// dialog. Triggered by Ctrl+Shift+, (handled in `on_key_down`).
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

    /// Close the dialog without saving. If a live theme preview is
    /// in-flight (i.e. the user clicked through the theme list before
    /// hitting Cancel), restore the chrome to the on-disk settings —
    /// `settings.json` is never touched during a preview, so the
    /// file-watch poller would otherwise leave the preview-theme
    /// pinned until the next real edit.
    pub fn cancel_settings_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.settings_dialog.take() else { return };
        // Reapply the original on-disk settings to revert any live
        // theme preview cleanly. `apply_settings` only swaps
        // in-memory state, so this is cheap and side-effect free.
        if state.original_settings.theme != self.settings_ref().theme {
            self.apply_settings(state.original_settings, cx);
        } else {
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
            div()
                .w_full()
                .text_size(px(11.0))
                .text_color(ink_ghost)
                .truncate()
                .child(text)
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
                .id(("settings-theme-row", id_hash(&built_in.name)))
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
            let row = div()
                .id(("settings-agent-row", id_hash(&profile.id)))
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

        // Cap the visible fallback chain so a 10-deep Nerd-Font list
        // doesn't blow past the dialog width. The full chain is still
        // applied at runtime — this is purely a display truncation.
        let fallbacks_hint: SharedString = format_fallbacks_hint(&draft.font.fallbacks).into();

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
        // Both share the dialog's 640 px width minus the 40 px side
        // padding + 16 px gutter — leaves ~290 px per column which
        // is enough for the font-family input plus the truncated
        // fallback hint without wrapping.

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
            .w(px(720.0))
            .max_h(px(760.0))
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

    // `SettingsDialogState` holds a `FocusHandle` and so can't be
    // constructed outside a gpui app, but the validation rules live
    // in the free function `parse_numeric_fields` — the runtime
    // `commit_text_buffers` and the tests below both call it, so a
    // change to the rules can never silently desync the two.

    #[test]
    fn parse_numeric_fields_accepts_valid_values() {
        let (size, line, scrollback) =
            parse_numeric_fields("15", "1.2", "30000").expect("valid");
        assert_eq!(size, 15.0);
        assert_eq!(line, 1.2);
        assert_eq!(scrollback, 30_000);
    }

    #[test]
    fn parse_numeric_fields_accepts_decimal_font_size_within_range() {
        let (size, _, _) =
            parse_numeric_fields("12.5", "1.0", "10000").expect("12.5 is in [8,24]");
        assert_eq!(size, 12.5);
    }

    #[test]
    fn parse_numeric_fields_rejects_font_size_out_of_range() {
        assert!(parse_numeric_fields("100", "1.0", "10000").is_err());
        assert!(parse_numeric_fields("7", "1.0", "10000").is_err());
    }

    #[test]
    fn parse_numeric_fields_rejects_line_height_out_of_range() {
        assert!(parse_numeric_fields("13", "2.0", "10000").is_err());
        assert!(parse_numeric_fields("13", "0.5", "10000").is_err());
    }

    #[test]
    fn parse_numeric_fields_rejects_scrollback_out_of_range() {
        assert!(parse_numeric_fields("13", "1.0", "999").is_err());
        assert!(parse_numeric_fields("13", "1.0", "100001").is_err());
    }

    #[test]
    fn parse_numeric_fields_rejects_garbage() {
        assert!(parse_numeric_fields("thirteen", "1.0", "10000").is_err());
        assert!(parse_numeric_fields("13", "abc", "10000").is_err());
        assert!(parse_numeric_fields("13", "1.0", "lots").is_err());
    }

    #[test]
    fn parse_numeric_fields_messages_are_user_facing() {
        // Spot-check that the error strings are the ones the dialog
        // actually shows — if these drift, the dialog's inline error
        // would show a stale message.
        let err = parse_numeric_fields("100", "1.0", "10000").unwrap_err();
        assert!(err.contains("Font size"), "{err}");
        let err = parse_numeric_fields("13", "2.0", "10000").unwrap_err();
        assert!(err.contains("Line height"), "{err}");
        let err = parse_numeric_fields("13", "1.0", "999").unwrap_err();
        assert!(err.contains("Scrollback"), "{err}");
    }

    #[test]
    fn id_hash_distinguishes_different_strings() {
        // Byte-sum hashing would collide on "ab" / "ba"; the real
        // `DefaultHasher` does not. This guards the dialog from row-
        // confusion glitches if a future theme / agent id happens to
        // share characters with an existing one.
        assert_ne!(id_hash("ab"), id_hash("ba"));
        assert_ne!(id_hash("claude"), id_hash("codex"));
        assert_ne!(id_hash("one-dark"), id_hash("tokyo-night"));
    }

    #[test]
    fn id_hash_is_stable_for_same_input() {
        // Two calls with the same input must return the same hash in
        // the same process — gpui ties element identity to it across
        // frames.
        assert_eq!(id_hash("codescope-default"), id_hash("codescope-default"));
    }

    #[test]
    fn format_fallbacks_hint_truncates_long_chains() {
        // Empty list reads as "(none)" rather than a dangling colon.
        assert_eq!(format_fallbacks_hint(&[]), "fallbacks: (none)");
        // Short chains render in full.
        let three = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        assert_eq!(format_fallbacks_hint(&three), "fallbacks: A, B, C");
        // Long chains keep the first three and indicate the rest.
        let ten: Vec<String> = (0..10).map(|i| format!("F{i}")).collect();
        let hint = format_fallbacks_hint(&ten);
        assert!(hint.starts_with("fallbacks: F0, F1, F2"), "{hint}");
        assert!(hint.contains("(+7 more)"), "{hint}");
    }

    #[test]
    fn format_fallbacks_hint_keeps_exactly_three_inline() {
        // Exactly the visible cap — no "+N more" suffix.
        let three = vec!["A".to_string(), "B".to_string(), "C".to_string()];
        let hint = format_fallbacks_hint(&three);
        assert!(!hint.contains("more"), "{hint}");
    }

    #[test]
    fn format_f32_strips_trailing_zeros() {
        assert_eq!(format_f32(13.0), "13");
        assert_eq!(format_f32(1.0), "1");
        assert_eq!(format_f32(1.25), "1.25");
        assert_eq!(format_f32(1.5), "1.5");
    }
}
