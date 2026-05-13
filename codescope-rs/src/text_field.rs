//! Reusable single-line text field for in-app dialogs (rename,
//! new-project, new-worktree, settings, command palette).
//!
//! The terminal pane has a fully-featured cursor (blink, movement,
//! selection — see `terminal/src/view.rs`), but the dialog inputs are
//! plain gpui divs and used to render as a static text run with a
//! permanently-on caret pinned to the right of the text. That left
//! three regressions vs. the C# WPF TextBox:
//!
//!   1. **No blink** — the caret was a steady accent bar.
//!   2. **No cursor movement** — typing only ever appended and
//!      backspace only ever popped, so arrow keys / Home / End did
//!      nothing and you couldn't insert in the middle of a value.
//!   3. **Visible gap** between the text and the caret — the legacy
//!      render placed the caret as a flex sibling separated by
//!      `gap_1()` (≈ 4 px), so the caret never sat against the last
//!      glyph.
//!
//! This module owns the data side (a small editable buffer with a
//! caret index) plus the render helper that splits the buffer at the
//! caret and paints the caret bar inline between the two halves with
//! no gap. Blink phase is supplied by the caller from
//! `AppShell::text_blink_phase` so every input on screen flips in
//! lockstep with the global timer.
//!
//! Intentionally minimal: no selection model, no IME composition, no
//! word-wise navigation. The C# build doesn't lean on those for these
//! dialogs either; selection is the next step on the parity ladder.

use std::sync::Arc;

use codescope_core::Theme;
use gpui::{Hsla, IntoElement, ParentElement, SharedString, Styled, div, px};

use crate::theme;

/// Single-line editable buffer with a caret index. The caret is a
/// byte offset that always sits on a UTF-8 char boundary — every
/// mutation re-aligns through char-boundary math so a non-ASCII glyph
/// never gets split mid-codepoint.
#[derive(Clone, Debug)]
pub struct TextField {
    text: String,
    caret: usize,
}

impl Default for TextField {
    fn default() -> Self {
        Self::new()
    }
}

impl TextField {
    pub fn new() -> Self {
        Self { text: String::new(), caret: 0 }
    }

    /// Build a field pre-filled with `initial`, caret parked at the
    /// end of the value. Matches the C# `TextBox.Text = …; CaretIndex
    /// = Text.Length;` idiom used on dialog open.
    pub fn with_text(initial: impl Into<String>) -> Self {
        let text = initial.into();
        let caret = text.len();
        Self { text, caret }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn caret(&self) -> usize {
        self.caret
    }

    /// Replace the buffer wholesale (used when an upstream derivation
    /// — e.g. `new_project_dialog::maybe_redrive_name` — recomputes
    /// the value). Caret is parked at end so the next keystroke
    /// appends to the new value rather than landing in the middle of
    /// it.
    pub fn set_text(&mut self, value: impl Into<String>) {
        self.text = value.into();
        self.caret = self.text.len();
    }

    pub fn insert_char(&mut self, ch: char) {
        let caret = self.clamped_caret();
        self.text.insert(caret, ch);
        self.caret = caret + ch.len_utf8();
    }

    /// Delete the char to the left of the caret (Backspace).
    pub fn backspace(&mut self) {
        let caret = self.clamped_caret();
        if caret == 0 {
            return;
        }
        let prev = prev_char_boundary(&self.text, caret);
        self.text.replace_range(prev..caret, "");
        self.caret = prev;
    }

    /// Delete the char to the right of the caret (Delete).
    pub fn delete_forward(&mut self) {
        let caret = self.clamped_caret();
        if caret >= self.text.len() {
            return;
        }
        let next = next_char_boundary(&self.text, caret);
        self.text.replace_range(caret..next, "");
        self.caret = caret;
    }

    pub fn move_left(&mut self) {
        let caret = self.clamped_caret();
        if caret == 0 {
            return;
        }
        self.caret = prev_char_boundary(&self.text, caret);
    }

    pub fn move_right(&mut self) {
        let caret = self.clamped_caret();
        if caret >= self.text.len() {
            return;
        }
        self.caret = next_char_boundary(&self.text, caret);
    }

    pub fn move_home(&mut self) {
        self.caret = 0;
    }

    pub fn move_end(&mut self) {
        self.caret = self.text.len();
    }

    fn clamped_caret(&self) -> usize {
        let mut c = self.caret.min(self.text.len());
        while c > 0 && !self.text.is_char_boundary(c) {
            c -= 1;
        }
        c
    }
}

fn prev_char_boundary(s: &str, mut idx: usize) -> usize {
    if idx == 0 {
        return 0;
    }
    idx -= 1;
    while idx > 0 && !s.is_char_boundary(idx) {
        idx -= 1;
    }
    idx
}

fn next_char_boundary(s: &str, mut idx: usize) -> usize {
    let len = s.len();
    if idx >= len {
        return len;
    }
    idx += 1;
    while idx < len && !s.is_char_boundary(idx) {
        idx += 1;
    }
    idx
}

/// Style knobs for the inline caret render. Each dialog passes its
/// own colours so a disabled / inactive surface can dim the caret.
pub struct CaretStyle {
    /// Colour of the text. Used for both halves of the split.
    pub ink: Hsla,
    /// Colour shown when the buffer is empty and the placeholder
    /// replaces the real value.
    pub ink_ghost: Hsla,
    /// Colour of the caret bar.
    pub caret: Hsla,
    /// Caret width in CSS pixels. 1.5 matches the legacy paint.
    pub caret_width: f32,
    /// Caret height in CSS pixels. 16 matches the legacy paint at
    /// 13 px text.
    pub caret_height: f32,
    /// Show the caret bar this frame. Driven by the global blink
    /// phase (`AppShell::text_blink_phase`) **AND** focus — pass
    /// `false` to suppress the caret entirely on an unfocused field.
    pub show_caret: bool,
}

/// Render the text content of a single-line input, with the caret
/// painted inline at `field.caret()`. When the buffer is empty,
/// `placeholder` is rendered in the ghost colour and the caret sits
/// at the very start (no gap).
///
/// The split-and-paint approach avoids the legacy "trailing caret in
/// a `gap_1()` flex sibling" idiom, which left a visible 4 px gap
/// between the last glyph and the caret bar. Here the two halves sit
/// directly against the caret.
pub fn render_input_content(
    field: &TextField,
    placeholder: SharedString,
    style: CaretStyle,
) -> impl IntoElement {
    let empty = field.is_empty();
    let (left, right) = if empty {
        (String::new(), String::new())
    } else {
        let caret = field.caret().min(field.text().len());
        let mut caret = caret;
        while caret > 0 && !field.text().is_char_boundary(caret) {
            caret -= 1;
        }
        (field.text()[..caret].to_string(), field.text()[caret..].to_string())
    };

    let mut row = div().flex().flex_row().items_center();

    if empty {
        if style.show_caret {
            row = row.child(
                div()
                    .w(px(style.caret_width))
                    .h(px(style.caret_height))
                    .bg(style.caret),
            );
        }
        row = row.child(
            div()
                .flex_grow()
                .text_color(style.ink_ghost)
                .truncate()
                .child(placeholder),
        );
        return row;
    }

    if !left.is_empty() {
        row = row.child(
            div()
                .text_color(style.ink)
                .child(SharedString::from(left)),
        );
    }
    if style.show_caret {
        row = row.child(
            div()
                .w(px(style.caret_width))
                .h(px(style.caret_height))
                .bg(style.caret),
        );
    }
    if !right.is_empty() {
        row = row.child(
            div()
                .flex_grow()
                .text_color(style.ink)
                .truncate()
                .child(SharedString::from(right)),
        );
    } else {
        // Push the caret away from the right edge by an empty grower.
        row = row.child(div().flex_grow());
    }

    row
}

/// Convenience constructor for a focused input's caret style. Resolves
/// the conventional ink / ghost / accent colours from the theme so the
/// caller only needs to thread the blink phase through.
pub fn focused_caret_style(theme: &Arc<Theme>, blink_phase: bool) -> CaretStyle {
    CaretStyle {
        ink: theme::ink(theme),
        ink_ghost: theme::ink_ghost(theme),
        caret: theme::accent(theme),
        caret_width: 1.5,
        caret_height: 16.0,
        show_caret: blink_phase,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_at_start_pushes_caret() {
        let mut f = TextField::new();
        f.insert_char('a');
        f.insert_char('b');
        assert_eq!(f.text(), "ab");
        assert_eq!(f.caret(), 2);
    }

    #[test]
    fn move_left_and_insert_inserts_mid_string() {
        let mut f = TextField::with_text("abc");
        f.move_left();
        f.insert_char('X');
        assert_eq!(f.text(), "abXc");
        assert_eq!(f.caret(), 3);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut f = TextField::with_text("ab");
        f.move_home();
        f.backspace();
        assert_eq!(f.text(), "ab");
        assert_eq!(f.caret(), 0);
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut f = TextField::with_text("ab");
        f.delete_forward();
        assert_eq!(f.text(), "ab");
        assert_eq!(f.caret(), 2);
    }

    #[test]
    fn multibyte_navigation_stays_on_boundaries() {
        let mut f = TextField::with_text("a\u{1F600}b");
        f.move_home();
        f.move_right();
        // Caret should sit after the 'a' (1 byte), before the emoji.
        assert_eq!(f.caret(), 1);
        f.move_right();
        // Past the 4-byte emoji.
        assert_eq!(f.caret(), 5);
        f.backspace();
        assert_eq!(f.text(), "ab");
    }

    #[test]
    fn set_text_parks_caret_at_end() {
        let mut f = TextField::with_text("abc");
        f.move_home();
        f.set_text("hello");
        assert_eq!(f.caret(), 5);
    }
}
