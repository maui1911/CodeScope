//! Command palette modal — Rust port of
//! `src/CodeScope.Ui/Dialogs/CommandPaletteDialog.xaml(.cs)` +
//! `CommandPaletteViewModel.cs` + `MainViewModel.Palette.cs`.
//!
//! Ctrl+P / Ctrl+Shift+P open the palette. Fuzzy search runs over a
//! freshly-assembled list of actions covering five kinds:
//!
//! - **Projects**  — focus / select the project in the sidebar.
//! - **Worktrees** — open or focus its session.
//! - **Agents**   — start a new session running that agent in the
//!                  active worktree.
//! - **Themes**   — live-apply (writes `settings.theme`, re-renders chrome).
//! - **Commands** — static built-ins (toggle overview / sidebar, new
//!                  project, open settings, reload theme).
//!
//! Arrow keys move selection; Enter activates the highlighted row;
//! Esc closes. The dropdown is grouped by kind so the user can
//! visually parse the result set without reading the right-side hint.
//!
//! Scoring lives in [`codescope_core::command_palette::score`] / `rank`
//! so it's testable without a window. See
//! `codescope-rs/core/src/command_palette.rs` for the algorithm spec.
//!
//! The palette state lives on `AppShell` rather than its own entity
//! because every action it dispatches already lives on `AppShell`
//! (spawn_tab, apply_settings, toggle_sidebar, sidebar selection…),
//! so an extra `Entity<Palette>` would only add a hop without sharing
//! any state.

use std::sync::Arc;

use codescope_core::Theme;
use gpui::{
    Context, FocusHandle, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, Styled, Window, anchored, deferred, div, point, px,
};

use crate::theme;

/// Kind of action shown in the palette. Drives the group label on the
/// row (uppercase eyebrow text) and decides which AppShell method runs
/// on submit. The variant data carries everything the dispatcher needs
/// — no separate lookup against the live state, so a `BuildPaletteActions`
/// snapshot survives across renders even as the underlying state shifts.
///
/// Some fields are read only by the human-facing path (debug logs,
/// toast strings the user can paste back into search), not by the
/// dispatcher — `#[allow(dead_code)]` keeps the compiler quiet without
/// us having to thread them through every match arm.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum PaletteActionKind {
    /// Focus the project at the given index in `sidebar.projects()`.
    Project { sidebar_index: usize, name: String },
    /// Open / focus a session for this worktree. `working_directory`
    /// is what gets handed to `spawn_tab_in`; `title` is the tab label.
    Worktree {
        working_directory: std::path::PathBuf,
        title: SharedString,
        branch: String,
        project_name: String,
    },
    /// Start a new session running this agent in the active worktree.
    /// `command` is the auto-typed shell command (e.g. "claude"); the
    /// dispatcher will look up the active project's path and pass it to
    /// `spawn_tab_in` as the working directory.
    Agent { id: String, display_name: String, command: String },
    /// Live-apply theme. The id matches a `codescope_core::theme::builtin`
    /// entry; the dispatcher passes it through `apply_settings` so the
    /// chrome repaints and `settings.json` picks up the new name.
    Theme { id: String, display_name: String },
    /// Built-in command. Each variant maps to an existing AppShell
    /// method so the palette never has to duplicate behaviour.
    Command(BuiltInCommand),
}

/// Static command list registered on every palette open. Mirrors the
/// rows the C# `BuildPaletteActions` appends regardless of project /
/// worktree state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltInCommand {
    /// Open the overview pane. The Rust port routes through the
    /// sidebar's `OpenOverview` event the same way the footer button
    /// does. Keymap hint: Ctrl+Shift+O.
    ToggleOverview,
    /// Show / hide the sidebar. Keymap hint: Ctrl+B.
    ToggleSidebar,
    /// Open the "Add project" dialog. Keymap hint: none (the `+` button).
    NewProject,
    /// Spawn a new shell tab. Keymap hint: Ctrl+T.
    NewSession,
    /// Reveal `settings.json` in the platform file browser. Keymap
    /// hint: none — the C# build wires this through a menu item.
    OpenSettings,
    /// Re-resolve the theme from settings and re-apply it. Useful when
    /// the user has hand-edited `settings.json` and wants to see the
    /// change without restarting. Keymap hint: none.
    ReloadTheme,
}

impl BuiltInCommand {
    /// Title shown in the palette row. Matches the C# wording where
    /// the C# build has a counterpart, and follows the same imperative
    /// "<Verb> <noun>" cadence for the new ones.
    pub fn title(self) -> &'static str {
        match self {
            BuiltInCommand::ToggleOverview => "Toggle overview",
            BuiltInCommand::ToggleSidebar => "Toggle sidebar",
            BuiltInCommand::NewProject => "New project",
            BuiltInCommand::NewSession => "New session",
            BuiltInCommand::OpenSettings => "Open settings",
            BuiltInCommand::ReloadTheme => "Reload theme",
        }
    }

    /// Right-aligned secondary hint (keymap or "menu") rendered in
    /// muted mono. Empty when the command has no visible chord.
    pub fn hint(self) -> &'static str {
        match self {
            BuiltInCommand::ToggleOverview => "Ctrl+Shift+O",
            BuiltInCommand::ToggleSidebar => "Ctrl+B",
            BuiltInCommand::NewProject => "+",
            BuiltInCommand::NewSession => "Ctrl+T",
            BuiltInCommand::OpenSettings => "menu",
            BuiltInCommand::ReloadTheme => "menu",
        }
    }
}

/// One palette row. The `kind` carries everything needed to dispatch
/// on submit. `title` is the user-facing label; `subtitle` is the
/// muted second line (path, branch, keymap hint).
///
/// `display` is `title` + `subtitle` joined with `   —   ` (matching
/// C# `PaletteAction.Display`). It's the string fed to the fuzzy
/// scorer so a user typing "main" matches both worktree titles and
/// branches in the subtitle.
#[derive(Debug, Clone)]
pub struct PaletteAction {
    pub kind: PaletteActionKind,
    pub title: SharedString,
    pub subtitle: Option<SharedString>,
    /// Group label rendered as a faint eyebrow above the title.
    pub group: PaletteGroup,
}

impl PaletteAction {
    /// Search-target string. Mirrors C# `PaletteAction.Display`.
    pub fn display(&self) -> String {
        match &self.subtitle {
            Some(sub) if !sub.trim().is_empty() => format!("{}   —   {}", self.title, sub),
            _ => self.title.to_string(),
        }
    }
}

/// Group label rendered as the row eyebrow. Drives ordering within a
/// tied score so the result list reads consistently — projects above
/// worktrees, worktrees above agents, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteGroup {
    Projects,
    Worktrees,
    Agents,
    Themes,
    Commands,
}

impl PaletteGroup {
    pub fn label(self) -> &'static str {
        match self {
            PaletteGroup::Projects => "Projects",
            PaletteGroup::Worktrees => "Worktrees",
            PaletteGroup::Agents => "Agents",
            PaletteGroup::Themes => "Themes",
            PaletteGroup::Commands => "Commands",
        }
    }
}

/// Live state of an open palette. Created by
/// [`crate::app::AppShell::open_command_palette`] and dropped on
/// confirm / cancel.
pub struct CommandPaletteState {
    pub focus_handle: FocusHandle,
    /// Full action list — built once on open. Re-runs scoring against
    /// this list whenever `query` changes; we don't re-build the list
    /// mid-session (matches C# behaviour: state captured at open).
    pub all_actions: Vec<PaletteAction>,
    /// Current search input.
    pub query: String,
    /// Indices into `all_actions` after filtering by `query`, sorted
    /// by descending score. Updated on every typed character.
    pub filtered: Vec<usize>,
    /// Index *into `filtered`* of the currently highlighted row. Arrow
    /// keys move this; Enter dispatches `all_actions[filtered[selected]]`.
    /// `0` when `filtered` is non-empty, irrelevant when empty.
    pub selected: usize,
}

impl CommandPaletteState {
    /// Construct with every action visible and the first row selected.
    /// Mirrors C# `CommandPaletteViewModel`'s constructor (which seeds
    /// `Filtered` from `_all`).
    pub fn new(actions: Vec<PaletteAction>, focus_handle: FocusHandle) -> Self {
        let filtered: Vec<usize> = (0..actions.len()).collect();
        Self {
            focus_handle,
            all_actions: actions,
            query: String::new(),
            filtered,
            selected: 0,
        }
    }

    /// Re-score against the current query and refresh `filtered`.
    /// Mirrors C# `OnQueryChanged`.
    pub fn refresh_filter(&mut self) {
        let needle = self.query.as_str();
        let displays: Vec<String> = self.all_actions.iter().map(|a| a.display()).collect();
        self.filtered = codescope_core::command_palette::rank(&displays, needle);
        self.selected = 0;
    }

    /// Resolve the highlighted action, if any.
    pub fn selected_action(&self) -> Option<&PaletteAction> {
        self.filtered.get(self.selected).map(|&i| &self.all_actions[i])
    }

    /// Move the highlight cursor by `delta` rows, clamped to the
    /// filtered range. No-op when the result list is empty.
    pub fn move_selection(&mut self, delta: i32) {
        if self.filtered.is_empty() {
            return;
        }
        let len = self.filtered.len() as i32;
        let next = ((self.selected as i32) + delta).clamp(0, len - 1);
        self.selected = next as usize;
    }

    pub fn append_char(&mut self, ch: char) {
        if ch.is_control() {
            return;
        }
        self.query.push(ch);
        self.refresh_filter();
    }

    pub fn pop_char(&mut self) {
        if self.query.pop().is_some() {
            self.refresh_filter();
        }
    }
}

// ─── Render helpers ────────────────────────────────────────────────────

/// Render the palette overlay. Returns `None` when closed. Layered via
/// `deferred(anchored(...))` so it paints on top of everything else —
/// same trick the dialog modules use.
pub(crate) fn render_palette(
    state: &CommandPaletteState,
    window: &mut Window,
    theme: &Arc<Theme>,
    cx: &mut Context<crate::app::AppShell>,
) -> gpui::AnyElement {
    let viewport = window.viewport_size();

    let elevated = theme::elevated(theme);
    let divider = theme::divider(theme);
    let ink = theme::ink(theme);
    let ink_muted = theme::ink_muted(theme);
    let ink_ghost = theme::ink_ghost(theme);
    let canvas = theme::canvas(theme);
    let surface_elev = theme::surface_elev(theme);
    let accent = theme::accent(theme);
    let faint = theme::text_faint();

    let focus_handle = state.focus_handle.clone();

    let header = div()
        .flex()
        .flex_col()
        .gap_1()
        .px_4()
        .pt_4()
        .child(
            div()
                .text_size(px(11.0))
                .text_color(accent)
                .font(theme::font_mono())
                .child("COMMAND PALETTE"),
        );

    let query_display: SharedString = if state.query.is_empty() {
        "Type to filter…".into()
    } else {
        state.query.clone().into()
    };
    let query_color: Hsla = if state.query.is_empty() { ink_ghost } else { ink };

    let query_box = div()
        .id("palette-query")
        .mx_4()
        .px_3()
        .h(px(36.0))
        .bg(canvas)
        .border_1()
        .border_color(divider)
        .rounded(px(6.0))
        .text_size(px(13.5))
        .text_color(query_color)
        .font(theme::font_sans())
        .flex()
        .items_center()
        .child(query_display);

    // Result rows. Iterate the filtered indices so the order matches
    // the ranker exactly.
    let mut rows = div()
        .id("palette-results")
        .flex()
        .flex_col()
        .px_2()
        .pb_3()
        .overflow_hidden();

    if state.filtered.is_empty() {
        rows = rows.child(
            div()
                .px_3()
                .py_3()
                .text_size(px(12.0))
                .text_color(ink_ghost)
                .font(theme::font_sans())
                .child("No matches"),
        );
    } else {
        for (row_idx, &action_idx) in state.filtered.iter().enumerate() {
            let action = &state.all_actions[action_idx];
            let is_selected = row_idx == state.selected;
            let bg = if is_selected { surface_elev } else { gpui::transparent_black() };

            // Right-aligned hint: keymap chord for built-in commands,
            // group label otherwise. Painted in `text_faint` per the
            // brief.
            let hint_text: SharedString = match &action.kind {
                PaletteActionKind::Command(cmd) => cmd.hint().to_string().into(),
                _ => action.group.label().to_string().into(),
            };

            let title_block = div()
                .flex_grow()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(faint)
                        .font(theme::font_sans())
                        .child(action.group.label()),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(ink)
                        .font(theme::font_sans())
                        .child(action.title.clone()),
                );

            let title_block = if let Some(sub) = action.subtitle.clone() {
                title_block.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(ink_muted)
                        .font(theme::font_mono())
                        .child(sub),
                )
            } else {
                title_block
            };

            let hint = div()
                .text_size(px(11.0))
                .text_color(faint)
                .font(theme::font_mono())
                .child(hint_text);

            let row = div()
                .id(("palette-row", row_idx))
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_3()
                .py_2()
                .rounded(px(4.0))
                .bg(bg)
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.activate_palette_row(row_idx, cx);
                    }),
                )
                .child(title_block)
                .child(hint);
            rows = rows.child(row);
        }
    }

    // Bottom hint strip — palette is keyboard-first; remind the user
    // of the chords. Same `text_faint` colour as the row eyebrow so it
    // reads as tertiary chrome.
    let foot = div()
        .px_4()
        .pb_3()
        .pt_2()
        .border_t_1()
        .border_color(divider)
        .text_size(px(11.0))
        .text_color(faint)
        .font(theme::font_mono())
        .child("↑↓ select · ↵ run · Esc close");

    let card = div()
        .flex()
        .flex_col()
        .gap_3()
        .w(px(480.0))
        .max_h(px(520.0))
        .bg(elevated)
        .border_1()
        .border_color(divider)
        .rounded_lg()
        .shadow_lg()
        .track_focus(&focus_handle)
        .key_context("CommandPalette")
        .on_key_down(cx.listener(handle_key_down))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, _, cx| cx.stop_propagation()),
        )
        .child(header)
        .child(query_box)
        .child(rows)
        .child(foot);

    // Backdrop dismiss + centred-card layout.
    let backdrop = div()
        .w(viewport.width)
        .h(viewport.height)
        .flex()
        .items_start()
        .justify_center()
        .pt(px(120.0))
        .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.close_command_palette(cx)),
        )
        .child(card);

    deferred(
        anchored()
            .position(point(px(0.0), px(0.0)))
            .child(backdrop),
    )
    .with_priority(10)
    .into_any_element()
}

/// Key handling for the palette card. Mirrors C# `OnQueryKeyDown` /
/// `OnResultsKeyDown` — Enter dispatches, Esc closes, arrows move,
/// backspace edits the query, printable characters are appended.
fn handle_key_down(
    shell: &mut crate::app::AppShell,
    event: &gpui::KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<crate::app::AppShell>,
) {
    let key = event.keystroke.key.as_str();
    cx.stop_propagation();

    match key {
        "escape" => {
            shell.close_command_palette(cx);
            return;
        }
        "enter" => {
            shell.submit_command_palette(window, cx);
            return;
        }
        "up" => {
            if let Some(state) = shell.command_palette_mut() {
                state.move_selection(-1);
                cx.notify();
            }
            return;
        }
        "down" => {
            if let Some(state) = shell.command_palette_mut() {
                state.move_selection(1);
                cx.notify();
            }
            return;
        }
        "backspace" => {
            if let Some(state) = shell.command_palette_mut() {
                state.pop_char();
                cx.notify();
            }
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
    if let Some(state) = shell.command_palette_mut() {
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

#[cfg(test)]
mod tests {
    use super::*;

    // `PaletteAction::display` mirrors C# `PaletteAction.Display`; this
    // is the exact string fed to the scorer, so a regression here would
    // silently change which rows the user can search.

    fn pa(title: &str, sub: Option<&str>) -> PaletteAction {
        PaletteAction {
            kind: PaletteActionKind::Command(BuiltInCommand::NewSession),
            title: title.to_string().into(),
            subtitle: sub.map(|s| s.to_string().into()),
            group: PaletteGroup::Commands,
        }
    }

    #[test]
    fn display_uses_title_only_when_subtitle_blank() {
        assert_eq!(pa("Title", None).display(), "Title");
        assert_eq!(pa("Title", Some("")).display(), "Title");
        assert_eq!(pa("Title", Some("  ")).display(), "Title");
    }

    #[test]
    fn display_joins_title_and_subtitle_with_em_dash() {
        assert_eq!(pa("Open", Some("Ctrl+O")).display(), "Open   —   Ctrl+O");
    }

    #[test]
    fn built_in_command_titles_are_stable() {
        // Lock the canonical titles — these are user-visible and the
        // C# build's wording is the parity target.
        assert_eq!(BuiltInCommand::NewSession.title(), "New session");
        assert_eq!(BuiltInCommand::ToggleSidebar.title(), "Toggle sidebar");
        assert_eq!(BuiltInCommand::ToggleOverview.title(), "Toggle overview");
        assert_eq!(BuiltInCommand::NewProject.title(), "New project");
        assert_eq!(BuiltInCommand::OpenSettings.title(), "Open settings");
        assert_eq!(BuiltInCommand::ReloadTheme.title(), "Reload theme");
    }

    #[test]
    fn palette_groups_have_distinct_labels() {
        // Reading the result list visually depends on each group
        // having an unambiguous eyebrow label.
        let labels = [
            PaletteGroup::Projects.label(),
            PaletteGroup::Worktrees.label(),
            PaletteGroup::Agents.label(),
            PaletteGroup::Themes.label(),
            PaletteGroup::Commands.label(),
        ];
        let mut sorted = labels.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }
}
