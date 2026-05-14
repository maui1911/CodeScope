//! Reusable single-line text field for in-app dialogs (rename,
//! new-project, new-worktree, settings, command palette).
//!
//! The terminal pane has a fully-featured cursor (blink, movement,
//! selection — see `terminal/src/view.rs`), but the dialog inputs are
//! plain gpui divs and used to render as a static text run with a
//! permanently-on caret pinned to the right of the text. That left
//! four regressions vs. the C# WPF TextBox:
//!
//!   1. **No blink** — the caret was a steady accent bar.
//!   2. **No cursor movement via keys** — typing only appended; arrow
//!      keys / Home / End did nothing.
//!   3. **No mouse-click-to-position** — clicking elsewhere in the
//!      field did not move the caret.
//!   4. **Visible gap** between the text and the caret — the legacy
//!      render placed the caret as a flex sibling separated by
//!      `gap_1()` (≈ 4 px), so the caret never sat against the last
//!      glyph.
//!
//! This module owns the data side (a small editable buffer with a
//! caret index, char-boundary-safe) plus a custom gpui [`Element`]
//! that shapes the rendered line, paints the caret at
//! `ShapedLine::x_for_index(caret_byte)`, and caches the shaped line
//! + the painted bounds back onto the field so the parent's
//! `on_mouse_down` listener can read them and translate a click
//! position into a byte index via `ShapedLine::closest_index_for_x`.
//!
//! Borrowed wholesale from Zed's `crates/gpui/examples/input.rs`
//! reference implementation; trimmed to single-line, no selection,
//! no IME composition. Blink phase is supplied by the caller from
//! `AppShell::text_blink_phase` so every input on screen flips in
//! lockstep with the global timer.

use std::sync::Arc;

use codescope_core::Theme;
use gpui::{
    App, Bounds, ElementId, GlobalElementId, Hsla, InspectorElementId, IntoElement, LayoutId,
    PaintQuad, ParentElement, Pixels, Point, ShapedLine, SharedString, Style, TextRun, Window,
    div, fill, point, px, relative, size,
};
use parking_lot::Mutex;

use crate::theme;

/// Snapshot of the most recent paint pass — the shaped line plus the
/// bounds we painted into. Stored on the [`TextField`] so the parent
/// dialog's `on_mouse_down` listener can translate a click in window
/// coords back into a byte index inside the buffer via
/// `ShapedLine::closest_index_for_x`. Lives behind an `Arc<Mutex<_>>`
/// because the paint pass writes from inside the custom [`Element`]
/// while the mouse listener reads from outside — and the field itself
/// is owned by the dialog entity, not the element.
#[derive(Clone)]
struct PaintSnapshot {
    line: ShapedLine,
    bounds: Bounds<Pixels>,
}

/// Single-line editable buffer with a caret index. The caret is a
/// byte offset that always sits on a UTF-8 char boundary — every
/// mutation re-aligns through char-boundary math so a non-ASCII glyph
/// never gets split mid-codepoint.
///
/// Holds an `Arc<Mutex<Option<PaintSnapshot>>>` populated by the
/// custom paint pass so click-to-position can hit-test without
/// re-shaping the line on every mouse event. Cloning the field
/// shares the snapshot Arc — that's fine because a clone only ever
/// shows up alongside the original inside the same dialog entity
/// (e.g. between `state.field` and a debug snapshot), and either
/// reader resolves to the same shaped line.
#[derive(Clone)]
pub struct TextField {
    text: String,
    caret: usize,
    snapshot: Arc<Mutex<Option<PaintSnapshot>>>,
}

impl Default for TextField {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for TextField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextField")
            .field("text", &self.text)
            .field("caret", &self.caret)
            .finish()
    }
}

impl TextField {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            caret: 0,
            snapshot: Arc::new(Mutex::new(None)),
        }
    }

    /// Build a field pre-filled with `initial`, caret parked at the
    /// end of the value. Matches the C# `TextBox.Text = …; CaretIndex
    /// = Text.Length;` idiom used on dialog open.
    pub fn with_text(initial: impl Into<String>) -> Self {
        let text = initial.into();
        let caret = text.len();
        Self {
            text,
            caret,
            snapshot: Arc::new(Mutex::new(None)),
        }
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

    /// Insert `ch` at the caret. Always changes the buffer (a char
    /// always lengthens it), so this returns `()` rather than a
    /// "changed" flag — callers should treat every insert as a
    /// redraw trigger.
    pub fn insert_char(&mut self, ch: char) {
        let caret = self.clamped_caret();
        self.text.insert(caret, ch);
        self.caret = caret + ch.len_utf8();
    }

    /// Delete the char to the left of the caret (Backspace). Returns
    /// `true` when the buffer actually shrank — `false` on a no-op
    /// (caret at start). The return drives whether the caller should
    /// wake the blink + notify; redrawing on a no-op would burn a
    /// frame for nothing.
    pub fn backspace(&mut self) -> bool {
        let caret = self.clamped_caret();
        if caret == 0 {
            return false;
        }
        let prev = prev_char_boundary(&self.text, caret);
        self.text.replace_range(prev..caret, "");
        self.caret = prev;
        true
    }

    /// Delete the char to the right of the caret (Delete). Returns
    /// `true` when the buffer actually shrank.
    pub fn delete_forward(&mut self) -> bool {
        let caret = self.clamped_caret();
        if caret >= self.text.len() {
            return false;
        }
        let next = next_char_boundary(&self.text, caret);
        self.text.replace_range(caret..next, "");
        self.caret = caret;
        true
    }

    /// Returns `true` when the caret actually moved.
    pub fn move_left(&mut self) -> bool {
        let caret = self.clamped_caret();
        if caret == 0 {
            return false;
        }
        self.caret = prev_char_boundary(&self.text, caret);
        true
    }

    pub fn move_right(&mut self) -> bool {
        let caret = self.clamped_caret();
        if caret >= self.text.len() {
            return false;
        }
        self.caret = next_char_boundary(&self.text, caret);
        true
    }

    pub fn move_home(&mut self) -> bool {
        if self.caret == 0 {
            return false;
        }
        self.caret = 0;
        true
    }

    pub fn move_end(&mut self) -> bool {
        if self.caret == self.text.len() {
            return false;
        }
        self.caret = self.text.len();
        true
    }

    /// Set the caret to a specific byte offset, snapping to the
    /// nearest char boundary so a hit-test on a non-ASCII glyph never
    /// leaves the caret in a split state.
    pub fn set_caret(&mut self, byte_offset: usize) {
        let mut idx = byte_offset.min(self.text.len());
        while idx > 0 && !self.text.is_char_boundary(idx) {
            idx -= 1;
        }
        self.caret = idx;
    }

    /// Translate a window-space click position into a caret byte
    /// index, using the cached [`ShapedLine`] + paint bounds from the
    /// most recent frame. Returns `None` when the field has not been
    /// painted yet (first frame) or the click landed clearly above
    /// the painted bounds; clicks below the bottom snap to end-of-
    /// buffer (matches the C# WPF behaviour and Zed's
    /// `index_for_mouse_position`).
    pub fn index_for_window_point(&self, position: Point<Pixels>) -> Option<usize> {
        let snapshot = self.snapshot.lock();
        let snap = snapshot.as_ref()?;
        if position.y < snap.bounds.top() {
            return Some(0);
        }
        if position.y > snap.bounds.bottom() {
            return Some(self.text.len());
        }
        let x_in = position.x - snap.bounds.left();
        Some(snap.line.closest_index_for_x(x_in))
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
#[derive(Clone)]
pub struct CaretStyle {
    /// Colour of the text.
    pub ink: Hsla,
    /// Colour shown when the buffer is empty and the placeholder
    /// replaces the real value.
    pub ink_ghost: Hsla,
    /// Colour of the caret bar.
    pub caret: Hsla,
    /// Caret width in CSS pixels.
    pub caret_width: f32,
    /// Show the caret bar this frame. Driven by the global blink
    /// phase (`AppShell::text_blink_phase`) **AND** focus — pass
    /// `false` to suppress the caret entirely on an unfocused field.
    pub show_caret: bool,
}

/// Build the standard "focused" caret style from the active theme.
pub fn focused_caret_style(theme: &Arc<Theme>, blink_phase: bool) -> CaretStyle {
    CaretStyle {
        ink: theme::ink(theme),
        ink_ghost: theme::ink_ghost(theme),
        caret: theme::accent(theme),
        caret_width: 1.5,
        show_caret: blink_phase,
    }
}

/// Render the buffer + caret as a custom gpui [`Element`]. Shapes
/// the line through `window.text_system().shape_line(...)`, paints
/// the resulting glyph run, paints a caret bar at
/// `line.x_for_index(caret_byte)`, and stashes the shaped line +
/// painted bounds back into the [`TextField`] so a follow-up
/// `on_mouse_down` can translate the click point into a byte index
/// without re-shaping.
///
/// `placeholder` is rendered (in `ink_ghost`) when the buffer is
/// empty so the field doesn't look broken. The caret bar still
/// paints at x = 0 in that case so the user sees where typing will
/// land.
pub fn render_input_content(
    field: &TextField,
    placeholder: SharedString,
    style: CaretStyle,
) -> impl IntoElement {
    div().child(TextFieldElement {
        text: field.text().to_string().into(),
        caret_byte: field.caret(),
        placeholder,
        style,
        snapshot: field.snapshot.clone(),
    })
}

/// Pixel height of the caret bar at the dialog default 13 px text.
/// Mirrors the legacy paint and the height the C# TextBox uses for
/// the same font size.
const CARET_HEIGHT: f32 = 16.0;

struct TextFieldElement {
    text: SharedString,
    caret_byte: usize,
    placeholder: SharedString,
    style: CaretStyle,
    snapshot: Arc<Mutex<Option<PaintSnapshot>>>,
}

struct TextFieldPrepaint {
    /// `None` when the buffer is empty — we still paint the caret
    /// and placeholder but skip the line.paint call to avoid
    /// shaping the empty string twice.
    line: Option<ShapedLine>,
    placeholder_line: Option<ShapedLine>,
    caret_quad: Option<PaintQuad>,
}

impl IntoElement for TextFieldElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl gpui::Element for TextFieldElement {
    type RequestLayoutState = ();
    type PrepaintState = TextFieldPrepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut s = Style::default();
        s.size.width = relative(1.).into();
        s.size.height = window.line_height().into();
        (window.request_layout(s, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        let text_style = window.text_style();
        let font = text_style.font();
        let font_size = text_style.font_size.to_pixels(window.rem_size());

        let empty = self.text.is_empty();
        let line = if empty {
            None
        } else {
            let run = TextRun {
                len: self.text.len(),
                font: font.clone(),
                color: self.style.ink,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            Some(
                window
                    .text_system()
                    .shape_line(self.text.clone(), font_size, &[run], None),
            )
        };

        let placeholder_line = if empty && !self.placeholder.is_empty() {
            let run = TextRun {
                len: self.placeholder.len(),
                font: font.clone(),
                color: self.style.ink_ghost,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            Some(window.text_system().shape_line(
                self.placeholder.clone(),
                font_size,
                &[run],
                None,
            ))
        } else {
            None
        };

        // Vertically centre the caret on the line: caret is 16 px
        // high, the line is `line_height` tall. The (h - 16) / 2
        // offset matches the legacy paint and the WPF TextBox
        // baseline.
        let line_h = window.line_height();
        // Vertically centre the caret inside the line box.
        let caret_pad = ((f32::from(line_h) - CARET_HEIGHT) / 2.0).max(0.0);

        let caret_quad = if self.style.show_caret {
            let caret_x = if let Some(line) = line.as_ref() {
                let safe = self.caret_byte.min(line.len());
                line.x_for_index(safe)
            } else {
                px(0.0)
            };
            Some(fill(
                Bounds::new(
                    point(bounds.left() + caret_x, bounds.top() + px(caret_pad)),
                    size(px(self.style.caret_width), px(CARET_HEIGHT)),
                ),
                self.style.caret,
            ))
        } else {
            None
        };

        TextFieldPrepaint {
            line,
            placeholder_line,
            caret_quad,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let line_h = window.line_height();
        if let Some(line) = prepaint.line.take() {
            // Best-effort paint — a failure here means the text
            // system couldn't lay something out, in which case the
            // user sees a blank input but no panic; the next frame
            // will retry. Same fallback the legacy renderer used.
            // Paint mutates inside, but the line is cheap to clone
            // (Arc<LineLayout> + SharedString) and we need a copy
            // for the snapshot mutex.
            let snap_line = line.clone();
            let _ = line.paint(bounds.origin, line_h, window, cx);
            *self.snapshot.lock() = Some(PaintSnapshot { line: snap_line, bounds });
        } else {
            // Empty buffer: still store an empty snapshot so a
            // first-frame click resolves to caret = 0 rather than
            // `None` (which would feel like the click was ignored).
            let run = TextRun {
                len: 0,
                font: window.text_style().font(),
                color: self.style.ink,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            let font_size = window.text_style().font_size.to_pixels(window.rem_size());
            let empty_line = window
                .text_system()
                .shape_line(SharedString::from(""), font_size, &[run], None);
            *self.snapshot.lock() = Some(PaintSnapshot {
                line: empty_line,
                bounds,
            });
        }
        if let Some(placeholder) = prepaint.placeholder_line.take() {
            let _ =
                placeholder.paint(bounds.origin, line_h, window, cx);
        }
        if let Some(quad) = prepaint.caret_quad.take() {
            window.paint_quad(quad);
        }
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

    #[test]
    fn set_caret_snaps_to_char_boundary() {
        let mut f = TextField::with_text("a\u{1F600}b"); // 1 + 4 + 1 = 6 bytes
        // Mid-emoji byte offset → snap back to before the emoji.
        f.set_caret(3);
        assert_eq!(f.caret(), 1);
        // Past the end → clamp to end.
        f.set_caret(99);
        assert_eq!(f.caret(), 6);
    }
}
