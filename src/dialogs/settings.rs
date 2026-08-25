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

use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;

use codescope_core::{AgentRegistry, Settings, Theme, theme::builtin};
use gpui::{
    Bounds, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, Pixels, SharedString, StatefulInteractiveElement, Styled,
    Window, anchored, deferred, div, point, px,
};

use crate::app::AppShell;
use crate::text_field::{TextField, focused_caret_style, render_input_content};
use crate::theme;

/// Which text-input field currently receives typed keystrokes. The
/// numeric stepper fields use plain text input so a user can paste
/// values; we validate on Save. The font family is picked from a
/// dropdown (see [`SettingsDialogState::font_popup_open`]) — while
/// that popup is open, typed keystrokes are routed to its filter
/// input instead of whichever field is nominally focused here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsField {
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
    /// Installed font families offered by the FONT FAMILY dropdown.
    /// Snapshotted once at dialog open from
    /// `cx.text_system().all_font_names()` (already sorted + deduped
    /// by gpui); the currently-configured family is spliced in when
    /// the OS doesn't report it (e.g. a hand-edited `settings.json`
    /// naming a font that was since uninstalled) so the selection
    /// never silently vanishes from the list.
    pub font_options: Vec<String>,
    /// FONT FAMILY dropdown popover open? While open, typed keys are
    /// routed to [`Self::font_query`] and Up/Down/Enter drive
    /// [`Self::font_selected_idx`] — mirrors the base-branch popup in
    /// `new_worktree.rs`.
    pub font_popup_open: bool,
    /// Filter text typed into the font popup search.
    pub font_query: TextField,
    /// Currently-highlighted row in the font popup (post-filter index).
    pub font_selected_idx: usize,
    /// Screen-space bounds of the FONT FAMILY trigger pill, recorded
    /// by a paint-phase `canvas` child each frame the dialog is up.
    /// The popup reads the previous frame's value to anchor itself
    /// directly under the pill — element construction happens before
    /// any painting, so same-frame bounds don't exist yet, but the
    /// pill paints every frame the dialog is open, so by the first
    /// popup render this is always populated. `Rc<Cell>` because the
    /// paint closure outlives the borrow of `self` that render holds.
    pub font_trigger_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    pub font_size_field: TextField,
    pub line_height_field: TextField,
    pub scrollback_field: TextField,
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
    pub fn new(
        settings: &Settings,
        installed_fonts: Vec<String>,
        focus_handle: FocusHandle,
    ) -> Self {
        // Normalise the family up front: the font-config builder in
        // `app.rs` (`push_non_empty_font_candidate`) treats a
        // whitespace-only family as "cleared", so the dialog must
        // too — otherwise a hand-edited `"  "` renders a blank-
        // looking trigger label and survives a Save round-trip.
        let mut draft = settings.clone();
        draft.font.family = draft.font.family.trim().to_string();
        Self {
            focus_handle,
            focused_field: SettingsField::FontSize,
            font_options: build_font_options(installed_fonts, &draft.font.family),
            font_popup_open: false,
            font_query: TextField::new(),
            font_selected_idx: 0,
            font_trigger_bounds: Rc::new(Cell::new(None)),
            font_size_field: TextField::with_text(format_f32(settings.font.size)),
            line_height_field: TextField::with_text(format_f32(settings.font.line_height_multiplier)),
            scrollback_field: TextField::with_text(settings.scrollback.to_string()),
            draft,
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
            self.font_size_field.text(),
            self.line_height_field.text(),
            self.scrollback_field.text(),
        )?;
        self.draft.font.size = size;
        self.draft.font.line_height_multiplier = line;
        self.draft.scrollback = scrollback;
        Ok(())
    }

    fn focused_field_mut(&mut self) -> &mut TextField {
        // An open font popup steals typed input for its filter — the
        // nominally-focused numeric field resumes when it closes.
        if self.font_popup_open {
            return &mut self.font_query;
        }
        match self.focused_field {
            SettingsField::FontSize => &mut self.font_size_field,
            SettingsField::LineHeight => &mut self.line_height_field,
            SettingsField::Scrollback => &mut self.scrollback_field,
        }
    }

    /// Font options that match the popup's current filter text.
    pub fn filtered_fonts(&self) -> Vec<&String> {
        filter_fonts(&self.font_options, self.font_query.text())
    }

    /// Insert + edit + caret-move helpers. The caret-movement /
    /// delete helpers forward the changed flag from `TextField` so a
    /// no-op (backspace at caret 0, move_left at caret 0, …) returns
    /// `false` and the caller skips the redraw.
    pub fn insert_char(&mut self, ch: char) -> bool {
        self.focused_field_mut().insert_char(ch);
        self.after_edit(true);
        true
    }

    /// Paste `s` into the focused field (control chars stripped by
    /// `TextField::insert_str`).
    pub fn insert_str(&mut self, s: &str) -> bool {
        let changed = self.focused_field_mut().insert_str(s);
        self.after_edit(changed);
        changed
    }

    pub fn backspace(&mut self) -> bool {
        let changed = self.focused_field_mut().backspace();
        self.after_edit(changed);
        changed
    }

    pub fn delete_forward(&mut self) -> bool {
        let changed = self.focused_field_mut().delete_forward();
        self.after_edit(changed);
        changed
    }

    /// Common post-edit bookkeeping: clear a stale validation error
    /// and, when the font popup's filter just changed, snap its
    /// highlight to the first *match* (visible index 1 — index 0 is
    /// the pinned "(built-in default)" row) so typing + Enter picks
    /// what the user filtered for, not the default. An emptied filter
    /// falls back to the pinned row.
    fn after_edit(&mut self, changed: bool) {
        if changed {
            self.error = None;
            if self.font_popup_open {
                let has_query = !self.font_query.text().trim().is_empty();
                self.font_selected_idx =
                    if has_query && !self.filtered_fonts().is_empty() { 1 } else { 0 };
            }
        }
    }

    pub fn move_caret_left(&mut self) -> bool {
        self.focused_field_mut().move_left()
    }

    pub fn move_caret_right(&mut self) -> bool {
        self.focused_field_mut().move_right()
    }

    pub fn move_caret_home(&mut self) -> bool {
        self.focused_field_mut().move_home()
    }

    pub fn move_caret_end(&mut self) -> bool {
        self.focused_field_mut().move_end()
    }

    /// Mutable accessor for one of the numeric text-input fields.
    /// Used by the mouse-down hit-test path so a click on an
    /// unfocused field shifts focus AND drops the caret at the click
    /// position in one step.
    pub fn field_mut_by(&mut self, field: SettingsField) -> &mut TextField {
        match field {
            SettingsField::FontSize => &mut self.font_size_field,
            SettingsField::LineHeight => &mut self.line_height_field,
            SettingsField::Scrollback => &mut self.scrollback_field,
        }
    }

    fn cycle_field(&mut self, forward: bool) {
        let order = [
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

/// Build the FONT FAMILY dropdown's option list. `installed` comes
/// from `TextSystem::all_font_names()` (sorted + deduped by gpui);
/// the currently-configured family is prepended when the OS doesn't
/// report it so the active selection is always present and pickable
/// again after browsing. An empty `current` (= "use the built-in
/// default chain") adds nothing — the popup offers the default via a
/// pinned "(built-in default)" row rendered above this list, never as
/// an empty-string entry inside it. Pulled out as a free function so
/// the unit tests can exercise it without a gpui `FocusHandle`.
fn build_font_options(installed: Vec<String>, current: &str) -> Vec<String> {
    // Whitespace-only counts as empty — mirrors
    // `app::push_non_empty_font_candidate`, which is what actually
    // consumes the family at font-config time.
    let current = current.trim();
    let mut options = installed;
    if !current.is_empty() && !options.iter().any(|n| n == current) {
        options.insert(0, current.to_string());
    }
    options
}

/// Case-insensitive substring filter over the font option list.
/// Mirrors `new_worktree::filter_branches` — an empty / whitespace
/// query matches everything. The pinned "(built-in default)" row is
/// not part of the option list; the popup renders it above these
/// matches unconditionally so the user can always clear back to the
/// default without emptying the filter first. Free function for the
/// same testability-without-FocusHandle reason as
/// [`build_font_options`].
fn filter_fonts<'a>(options: &'a [String], query: &str) -> Vec<&'a String> {
    let q = query.trim().to_lowercase();
    options
        .iter()
        .filter(|name| q.is_empty() || name.to_lowercase().contains(&q))
        .collect()
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
        let installed_fonts = cx.text_system().all_font_names();
        let state = SettingsDialogState::new(
            self.settings_ref().as_ref(),
            installed_fonts,
            focus_handle,
        );
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
        // swap atomically and the sidebar repaints. Palette and font
        // changes are pushed into running terminals in place
        // (`push_palette_to_terminals` / `push_font_to_terminals`);
        // scrollback still applies to new tabs only. Theme + cursor
        // take effect for chrome on the next frame; cursor-style
        // changes apply to freshly-spawned tabs.
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
    pub fn settings_set_cursor_shape(
        &mut self,
        shape: codescope_core::CursorShape,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.settings_dialog.as_mut() {
            state.draft.cursor.shape = shape;
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

    /// Move keyboard focus between the typed-input fields. Clicking
    /// into a numeric field also dismisses an open font popup so the
    /// two input targets can't both claim the keyboard.
    pub fn settings_focus_field(&mut self, field: SettingsField, cx: &mut Context<Self>) {
        if let Some(state) = self.settings_dialog.as_mut() {
            state.focused_field = field;
            state.font_popup_open = false;
            cx.notify();
        }
    }

    /// Open / close the FONT FAMILY dropdown. Opening clears the
    /// filter and highlights the currently-configured family so
    /// Enter with no typing is a no-op-shaped confirm — the pinned
    /// "(built-in default)" row sits at visible index 0, so option
    /// rows are offset by one. Mirrors `Sidebar::toggle_base_popup`.
    pub fn settings_toggle_font_popup(&mut self, cx: &mut Context<Self>) {
        if let Some(state) = self.settings_dialog.as_mut() {
            state.font_popup_open = !state.font_popup_open;
            if state.font_popup_open {
                state.font_query.set_text("");
                state.font_selected_idx = state
                    .font_options
                    .iter()
                    .position(|name| *name == state.draft.font.family)
                    .map(|pos| pos + 1)
                    .unwrap_or(0);
            }
            cx.notify();
        }
    }

    /// Pick a font family and close the popup. An empty string means
    /// "use the built-in default chain" (the pinned top row). The
    /// choice lands in the draft only — Save persists it, Cancel
    /// discards it.
    pub fn settings_select_font(&mut self, family: String, cx: &mut Context<Self>) {
        if let Some(state) = self.settings_dialog.as_mut() {
            state.draft.font.family = family;
            state.font_popup_open = false;
            state.font_query.set_text("");
            state.error = None;
            cx.notify();
        }
    }

    /// Move the font popup highlight up/down, clamped to the visible
    /// rows (the pinned "(built-in default)" row at index 0 plus the
    /// filtered options).
    pub fn settings_move_font_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(state) = self.settings_dialog.as_mut() else { return };
        let max = state.filtered_fonts().len() as isize; // pinned row makes len + 1 rows
        let next = (state.font_selected_idx as isize + delta).clamp(0, max);
        state.font_selected_idx = next as usize;
        cx.notify();
    }

    /// Resolve the highlighted popup row into a selection (Enter).
    /// Index 0 is the pinned "(built-in default)" row → empty family;
    /// indices 1.. point into the filtered list.
    pub fn settings_confirm_font_selection(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.settings_dialog.as_mut() else { return };
        if !state.font_popup_open {
            return;
        }
        let idx = state.font_selected_idx;
        let chosen = if idx == 0 {
            Some(String::new())
        } else {
            state.filtered_fonts().get(idx - 1).map(|name| (*name).clone())
        };
        if let Some(family) = chosen {
            self.settings_select_font(family, cx);
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
        let danger = theme::danger();

        let focus_handle = state.focus_handle.clone();
        let draft = state.draft.clone();
        let focused_field = state.focused_field;
        let font_popup_open = state.font_popup_open;
        let error_msg: Option<SharedString> = state.error.clone().map(Into::into);
        let blink_phase = self.text_blink_phase;

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
                       field: &TextField,
                       placeholder: &'static str,
                       this_field: SettingsField|
         -> gpui::Stateful<gpui::Div> {
            // The open font popup owns the keyboard, so no numeric
            // field shows a focus ring / caret while it's up.
            let is_focused = focused_field == this_field && !font_popup_open;
            let mut style = focused_caret_style(theme, blink_phase);
            style.show_caret = is_focused && blink_phase;
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
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.settings_focus_field(this_field, cx);
                        if let Some(state) = this.settings_dialog.as_mut() {
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

        // FONT FAMILY — clickable pill that toggles the dropdown
        // popup. The current selection renders inline; a chevron
        // hints that it's a dropdown. Mirrors `nw-base-trigger` in
        // `new_worktree.rs`.
        let font_trigger_label: SharedString = if draft.font.family.is_empty() {
            "(built-in default)".into()
        } else {
            draft.font.family.clone().into()
        };
        let font_trigger = div()
            .id("settings-font-family")
            .px_3()
            .h(px(32.0))
            .bg(canvas)
            .border_1()
            .border_color(if font_popup_open { accent } else { divider })
            .rounded(px(6.0))
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
                    this.settings_toggle_font_popup(cx);
                }),
            )
            .child(div().flex_grow().truncate().child(font_trigger_label))
            .child(div().text_color(ink_ghost).child("▾"))
            .child({
                // Invisible full-size overlay that records the pill's
                // screen-space bounds at paint time so the popup can
                // anchor itself under the pill next frame (see the
                // `font_trigger_bounds` field docs).
                // `gpui::canvas` written out — the `canvas` binding
                // in this scope is the theme colour.
                let bounds_cell = state.font_trigger_bounds.clone();
                gpui::canvas(
                    move |bounds, _, _| bounds_cell.set(Some(bounds)),
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            });
        let font_size_input = textbox(
            "settings-font-size",
            &state.font_size_field,
            "13",
            SettingsField::FontSize,
        );
        let line_height_input = textbox(
            "settings-line-height",
            &state.line_height_field,
            "1.0",
            SettingsField::LineHeight,
        );
        let scrollback_input = textbox(
            "settings-scrollback",
            &state.scrollback_field,
            "10000",
            SettingsField::Scrollback,
        );

        // Cap the visible fallback chain so a 10-deep Nerd-Font list
        // doesn't blow past the dialog width. The full chain is still
        // applied at runtime — this is purely a display truncation.
        let fallbacks_hint: SharedString = format_fallbacks_hint(&draft.font.fallbacks).into();

        // ─── Cursor shape radios ───────────────────────────────────
        use codescope_core::CursorShape;
        let cursor_radio = |id: &'static str, label: &'static str, value: CursorShape| {
            let active = draft.cursor.shape == value;
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
            .child(cursor_radio("settings-cursor-block", "Block", CursorShape::Block))
            .child(cursor_radio("settings-cursor-beam", "Beam", CursorShape::Beam))
            .child(cursor_radio("settings-cursor-underline", "Underline", CursorShape::Underline))
            .child(cursor_radio("settings-cursor-hollow", "Hollow", CursorShape::HollowBlock));

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
            .child(font_trigger)
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

        // Optional font popup, layered above the dialog as a separate
        // `deferred` at higher priority (the idiom of the base-branch
        // popup in `new_worktree.rs`), anchored under the trigger
        // pill via the bounds its canvas overlay recorded last frame.
        let popup = font_popup_open
            .then(|| self.render_font_popup(state, theme, viewport, cx));

        let mut layers: Vec<gpui::AnyElement> = Vec::new();
        layers.push(
            deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
                .with_priority(10)
                .into_any_element(),
        );
        if let Some(p) = popup {
            layers.push(p);
        }

        Some(div().children(layers).into_any_element())
    }

    /// Build the FONT FAMILY dropdown popover: a type-to-filter
    /// search row on top of a scrollable list of every installed
    /// font family. The keyboard-highlighted row gets a frost
    /// backdrop; the currently-configured family is tinted accent so
    /// the user can find "what do I have now" while browsing.
    fn render_font_popup(
        &self,
        state: &SettingsDialogState,
        theme: &Arc<Theme>,
        viewport: gpui::Size<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_muted = theme::ink_muted(theme);
        let frost = theme::frost_10(theme);
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);

        let filtered = state.filtered_fonts();
        let selected_idx = state.font_selected_idx;
        let current_family = state.draft.font.family.clone();

        // Search row — same caret discipline as the numeric inputs.
        let mut search_style = focused_caret_style(theme, self.text_blink_phase);
        search_style.show_caret = self.text_blink_phase;
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
                &state.font_query,
                SharedString::from("Filter fonts…"),
                search_style,
            ));

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        // Index 0: pinned "(built-in default)" row — always visible
        // regardless of filter (the C#-era `(HEAD)` pin idiom from
        // the base-branch popup) so the user can clear back to the
        // default chain without emptying the query first. Selecting
        // it stores an empty family.
        {
            let is_current = current_family.is_empty();
            let active = selected_idx == 0;
            rows.push(
                div()
                    .id("settings-font-row-default")
                    .h(px(28.0))
                    .px_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_size(px(12.0))
                    .text_color(if is_current { accent } else { ink_muted })
                    .bg(if active { frost } else { gpui::transparent_black() })
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.settings_select_font(String::new(), cx);
                        }),
                    )
                    .child(div().flex_grow().truncate().child("(built-in default)"))
                    .into_any_element(),
            );
        }
        for (idx, name) in filtered.iter().enumerate() {
            let is_current = **name == current_family;
            // Visible index is offset by the pinned row above.
            let active = idx + 1 == selected_idx;
            let bg = if active { frost } else { gpui::transparent_black() };
            let name_for_handler = (*name).clone();
            let label: SharedString = (*name).clone().into();
            rows.push(
                div()
                    .id(("settings-font-row", id_hash(name)))
                    .h(px(28.0))
                    .px_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .text_size(px(12.0))
                    .text_color(if is_current { accent } else { ink })
                    .bg(bg)
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.settings_select_font(name_for_handler.clone(), cx);
                        }),
                    )
                    .child(
                        // The name renders in its own family so the
                        // list doubles as a live preview — gpui falls
                        // back per-glyph for faces that fail to load,
                        // so a stale entry still reads fine. Shaped
                        // lines are cached across frames, so the cost
                        // is a first-open shape per visible row.
                        div()
                            .flex_grow()
                            .truncate()
                            .font_family((*name).clone())
                            .child(label),
                    )
                    .into_any_element(),
            );
        }
        if filtered.is_empty() {
            rows.push(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(12.0))
                    .text_color(ink_muted)
                    .child("No fonts match the filter.")
                    .into_any_element(),
            );
        }

        // Anchor under the trigger pill using the bounds its canvas
        // overlay recorded last frame; match the pill's width so the
        // popup reads as an extension of it. First-ever frame (cell
        // still empty) falls back to a centred position — in practice
        // unreachable, since the pill paints before the popup can be
        // opened.
        let trigger = state.font_trigger_bounds.get();
        let popup_w = trigger.map(|b| f32::from(b.size.width)).unwrap_or(360.0);
        let popup = div()
            .w(px(popup_w))
            .max_h(px(320.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_md()
            .shadow_lg()
            .flex()
            .flex_col()
            .overflow_hidden()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(search)
            .child(
                // Stateful + `overflow_y_scroll` so an OS with more
                // fonts than fit in `max_h(320)` still lets the user
                // reach every row by mouse. Stable id so gpui can
                // persist the scroll offset across renders.
                div()
                    .id("settings-font-popup-rows")
                    .flex_grow()
                    .flex()
                    .flex_col()
                    .py_1()
                    .overflow_y_scroll()
                    .children(rows),
            );

        let viewport_w: f32 = viewport.width.into();
        let position = trigger
            .map(|b| point(b.left(), b.bottom() + px(4.0)))
            .unwrap_or_else(|| point(px(((viewport_w - popup_w) / 2.0).max(8.0)), px(80.0)));

        // Transparent full-window click-catcher between the dialog
        // backdrop (priority 10) and the popup (priority 20). A click
        // outside the popup lands here and just dismisses the popup —
        // without it the click would fall through to the backdrop and
        // cancel the whole dialog, discarding the user's edits. The
        // popup's own mouse-down handler stops propagation, so clicks
        // inside it never reach this layer.
        let click_catcher = deferred(
            anchored().position(point(px(0.0), px(0.0))).child(
                div()
                    .w(viewport.width)
                    .h(viewport.height)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.settings_toggle_font_popup(cx);
                        }),
                    ),
            ),
        )
        .with_priority(15)
        .into_any_element();

        let popup_layer = deferred(
            anchored()
                .position(position)
                // Keep the popup inside the window when the dialog
                // sits low — gpui shifts (or flips) it as needed.
                .snap_to_window_with_margin(px(8.0))
                .child(popup),
        )
        .with_priority(20)
        .into_any_element();

        div()
            .children([click_catcher, popup_layer])
            .into_any_element()
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

    // Snapshot popup state up front — the font popup has its own
    // keyboard model (Up/Down/Enter/Escape) layered over the
    // dialog's, mirroring the base-branch popup in `new_worktree.rs`.
    let popup_open = shell
        .settings_dialog
        .as_ref()
        .map(|s| s.font_popup_open)
        .unwrap_or(false);

    if crate::text_field::is_paste_chord(&event.keystroke) {
        let pasted = cx.read_from_clipboard().and_then(|item| item.text());
        let changed = pasted
            .and_then(|text| shell.settings_dialog.as_mut().map(|s| s.insert_str(&text)))
            .unwrap_or(false);
        if changed {
            shell.wake_text_blink(cx);
            cx.notify();
        }
        return;
    }

    match key {
        "escape" => {
            // Escape inside an open popup just closes the popup;
            // otherwise it cancels the whole dialog.
            if popup_open {
                shell.settings_toggle_font_popup(cx);
            } else {
                shell.cancel_settings_dialog(cx);
            }
            return;
        }
        "enter" => {
            if popup_open {
                shell.settings_confirm_font_selection(cx);
            } else {
                shell.submit_settings_dialog(cx);
            }
            return;
        }
        "up" if popup_open => {
            shell.settings_move_font_selection(-1, cx);
            return;
        }
        "down" if popup_open => {
            shell.settings_move_font_selection(1, cx);
            return;
        }
        "tab" => {
            // Skip when the popup is open — it owns the keyboard.
            if !popup_open
                && let Some(state) = shell.settings_dialog.as_mut()
            {
                let forward = !event.keystroke.modifiers.shift;
                state.cycle_field(forward);
                cx.notify();
            }
            return;
        }
        "backspace" => {
            let touched = shell
                .settings_dialog
                .as_mut()
                .map(|s| s.backspace())
                .unwrap_or(false);
            if touched {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "delete" => {
            let touched = shell
                .settings_dialog
                .as_mut()
                .map(|s| s.delete_forward())
                .unwrap_or(false);
            if touched {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "left" => {
            let touched = shell
                .settings_dialog
                .as_mut()
                .map(|s| s.move_caret_left())
                .unwrap_or(false);
            if touched {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "right" => {
            let touched = shell
                .settings_dialog
                .as_mut()
                .map(|s| s.move_caret_right())
                .unwrap_or(false);
            if touched {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "home" => {
            let touched = shell
                .settings_dialog
                .as_mut()
                .map(|s| s.move_caret_home())
                .unwrap_or(false);
            if touched {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "end" => {
            let touched = shell
                .settings_dialog
                .as_mut()
                .map(|s| s.move_caret_end())
                .unwrap_or(false);
            if touched {
                shell.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "space" => {
            if let Some(state) = shell.settings_dialog.as_mut() {
                state.insert_char(' ');
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
    let mut changed = false;
    if let Some(state) = shell.settings_dialog.as_mut() {
        for ch in key_char.chars() {
            if !ch.is_control() && state.insert_char(ch) {
                changed = true;
            }
        }
    }
    if changed {
        shell.wake_text_blink(cx);
        cx.notify();
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
    fn build_font_options_keeps_installed_list_when_current_present() {
        let installed = vec!["Consolas".to_string(), "FiraCode Nerd Font".to_string()];
        let options = build_font_options(installed.clone(), "Consolas");
        assert_eq!(options, installed);
    }

    #[test]
    fn build_font_options_prepends_missing_current_family() {
        let installed = vec!["Consolas".to_string()];
        let options = build_font_options(installed, "Uninstalled Font");
        assert_eq!(options[0], "Uninstalled Font");
        assert_eq!(options.len(), 2);
    }

    #[test]
    fn build_font_options_ignores_empty_current_family() {
        // Empty family = "use the built-in default chain"; the
        // dropdown must not grow a blank row for it.
        let installed = vec!["Consolas".to_string()];
        let options = build_font_options(installed, "");
        assert_eq!(options, vec!["Consolas".to_string()]);
    }

    #[test]
    fn build_font_options_treats_whitespace_only_current_as_empty() {
        // A hand-edited `"  "` family means "cleared" to the font-
        // config builder (`push_non_empty_font_candidate` trims), so
        // it must not surface as a blank-looking option here either.
        let installed = vec!["Consolas".to_string()];
        let options = build_font_options(installed, "   ");
        assert_eq!(options, vec!["Consolas".to_string()]);
    }

    #[test]
    fn filter_fonts_is_case_insensitive_substring() {
        let options = vec![
            "Cascadia Mono".to_string(),
            "Consolas".to_string(),
            "FiraCode Nerd Font".to_string(),
        ];
        let hits = filter_fonts(&options, "cas");
        assert_eq!(hits, vec![&options[0]]);
        let hits = filter_fonts(&options, "NERD");
        assert_eq!(hits, vec![&options[2]]);
    }

    #[test]
    fn filter_fonts_blank_query_matches_everything() {
        let options = vec!["A".to_string(), "B".to_string()];
        assert_eq!(filter_fonts(&options, "").len(), 2);
        assert_eq!(filter_fonts(&options, "   ").len(), 2);
    }

    #[test]
    fn filter_fonts_no_match_yields_empty() {
        let options = vec!["Consolas".to_string()];
        assert!(filter_fonts(&options, "zzz").is_empty());
    }

    #[test]
    fn format_f32_strips_trailing_zeros() {
        assert_eq!(format_f32(13.0), "13");
        assert_eq!(format_f32(1.0), "1");
        assert_eq!(format_f32(1.25), "1.25");
        assert_eq!(format_f32(1.5), "1.5");
    }
}
