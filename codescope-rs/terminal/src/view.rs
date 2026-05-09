//! gpui View layer for the terminal.
//!
//! Renders the visible grid as one row of styled spans per line. Each
//! [`StyledRun`] from the backend snapshot becomes a div with its own
//! `text_color` / `bg`, so we get per-cell ANSI colours without needing
//! a full `Element` impl yet. The Element layer is still on the roadmap
//! — it's the only way to get a sub-cell cursor, partial-cell
//! background fills, and the kind of text-shaping batching that keeps
//! a 200×60 grid cheap. For now this is plenty to drive a real shell.
//!
//! Architecture pointers:
//!
//! * Snapshotting happens on [`BackendEvent::Wakeup`] (and friends), so
//!   we don't poll. The async drain task lives for the entity's
//!   lifetime; dropping the entity drops the receiver and the loop
//!   exits cleanly.
//! * Keyboard input is converted via [`keystroke_to_bytes`] and pushed
//!   into the backend's input queue.
//! * A `canvas` overlay on the line stack reports its laid-out bounds
//!   so we can compute the right `cols`×`rows` for the current window
//!   size and call [`Backend::resize`] when it changes.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::backend::{Backend, StyledRun, TerminalSize, TerminalSnapshot};
use crate::colors::ColorPalette;
use crate::input::keystroke_to_bytes;
use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, Font, FontFallbacks,
    FontFeatures, FontStyle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    Render, ScrollDelta, ScrollWheelEvent, SharedString, Styled, TextRun, Window, canvas, div,
    px,
};

/// User-overridable font + size knobs. The defaults target oh-my-posh
/// users on Windows: a Nerd Font variant of Cascadia, plus a stack of
/// fallbacks so the powerline glyph range is found even when the
/// primary face doesn't ship it. Set `CODESCOPE_FONT` to override the
/// primary; the fallbacks still apply.
///
/// `cell_width` and `line_height` are the *measured* values once we've
/// run a real shape on `window.text_system()`. The construction-time
/// values are placeholders used only for the very first frame.
#[derive(Debug, Clone)]
pub struct FontConfig {
    pub family: SharedString,
    pub fallbacks: Vec<SharedString>,
    pub size: Pixels,
    pub line_height: Pixels,
    pub cell_width: Pixels,
}

impl Default for FontConfig {
    fn default() -> Self {
        let family: SharedString = std::env::var("CODESCOPE_FONT")
            .unwrap_or_else(|_| "FiraCode Nerd Font".to_string())
            .into();
        // Common nerd-font names on Windows, in rough order of
        // likelihood. gpui falls back per-glyph, so installing any one
        // of these is enough to pick up missing powerline icons.
        let fallbacks = vec![
            "FiraCode Nerd Font Mono".into(),
            "FiraCodeNerdFont".into(),
            "FiraCodeNerdFontMono".into(),
            "CaskaydiaCove Nerd Font".into(),
            "CaskaydiaCove Nerd Font Mono".into(),
            "MesloLGM Nerd Font".into(),
            "JetBrainsMono Nerd Font".into(),
            "Hack Nerd Font".into(),
            "Cascadia Mono".into(),
            "Consolas".into(),
        ];
        Self {
            family,
            fallbacks,
            size: px(13.0),
            line_height: px(18.0),
            cell_width: px(7.8),
        }
    }
}

impl FontConfig {
    fn to_font(&self) -> Font {
        Font {
            family: self.family.clone(),
            features: FontFeatures::default(),
            fallbacks: Some(FontFallbacks::from_fonts(
                self.fallbacks.iter().map(|f| f.to_string()).collect(),
            )),
            weight: FontWeight::NORMAL,
            style: FontStyle::Normal,
        }
    }
}

pub struct TerminalView {
    backend: Backend,
    snapshot: TerminalSnapshot,
    palette: ColorPalette,
    font: FontConfig,
    focus_handle: FocusHandle,
    /// Last grid size we sent to the Backend, so we don't trigger a
    /// resize on every render.
    last_size: Arc<Mutex<(u16, u16)>>,
    /// Shared cache of the most recently laid-out terminal bounds.
    /// Mouse handlers read this to translate window-pixel positions to
    /// grid (line, column) coords.
    bounds_cache: Arc<Mutex<Option<Bounds<Pixels>>>>,
    /// `true` between `mouse_down` and `mouse_up` while the user is
    /// dragging out a selection. Mouse-move events are ignored unless
    /// this is set, so we don't accidentally extend the selection
    /// every time the cursor passes over the terminal.
    selecting: bool,
}

impl TerminalView {
    /// Wrap a [`Backend`] in a gpui Entity. Spawns an async drain task
    /// that re-snapshots and notifies on every backend event.
    pub fn new(backend: Backend, cx: &mut Context<Self>) -> Self {
        Self::new_with_font(backend, FontConfig::default(), cx)
    }

    pub fn new_with_font(
        backend: Backend,
        font: FontConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let palette = ColorPalette::default();
        let snapshot = backend.snapshot(&palette);
        let events = backend.events();

        cx.spawn(async move |this, cx| {
            while let Ok(_event) = events.recv_async().await {
                if this
                    .update(cx, |view: &mut Self, cx| {
                        view.snapshot = view.backend.snapshot(&view.palette);
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        Self {
            backend,
            snapshot,
            palette,
            font,
            focus_handle,
            last_size: Arc::new(Mutex::new((0, 0))),
            bounds_cache: Arc::new(Mutex::new(None)),
            selecting: false,
        }
    }

    fn point_at(&self, position: Point<Pixels>) -> Option<(i32, usize)> {
        let bounds = (*self.bounds_cache.lock())?;
        let cell_w: f32 = self.font.cell_width.into();
        let line_h: f32 = self.font.line_height.into();
        if cell_w <= 0.0 || line_h <= 0.0 {
            return None;
        }
        let local_x: f32 = (position.x - bounds.origin.x).into();
        let local_y: f32 = (position.y - bounds.origin.y).into();
        if local_x < 0.0 || local_y < 0.0 {
            return None;
        }
        let col = (local_x / cell_w).floor().max(0.0) as usize;
        let visible_row = (local_y / line_h).floor() as i32;
        let display_offset = self.backend.display_offset() as i32;
        // Convert visible-row index back to absolute grid line so the
        // selection range stays anchored to the original output even
        // when the user scrolls.
        let grid_line = visible_row - display_offset;
        Some((grid_line, col))
    }

    fn refresh_snapshot(&mut self, cx: &mut Context<Self>) {
        self.snapshot = self.backend.snapshot(&self.palette);
        cx.notify();
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.button != MouseButton::Left {
            return;
        }
        if let Some((line, col)) = self.point_at(event.position) {
            self.backend.start_selection(line, col);
            self.selecting = true;
            self.refresh_snapshot(cx);
        }
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.selecting {
            return;
        }
        if let Some((line, col)) = self.point_at(event.position) {
            self.backend.extend_selection(line, col);
            self.refresh_snapshot(cx);
        }
    }

    fn on_mouse_up(
        &mut self,
        _event: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        self.selecting = false;
    }

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();

        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        // Copy semantics, matching Windows Terminal / GNOME Terminal:
        //   * Ctrl+C with a live selection → copy, clear selection.
        //   * Ctrl+C with no selection      → fall through to SIGINT.
        //   * Ctrl+Shift+C                  → always copy (no-op if
        //                                     nothing is selected).
        //   * Cmd+C (macOS)                 → always copy.
        if key == "c" && (mods.control || mods.platform) && !mods.alt {
            let force_copy = mods.shift || mods.platform;
            if let Some(text) = self.backend.selection_text() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                self.backend.clear_selection();
                self.refresh_snapshot(cx);
                return;
            }
            if force_copy {
                return;
            }
            // No selection + plain Ctrl+C: fall through so the shell
            // gets its SIGINT byte.
        }

        // PageUp/PageDown without modifiers scroll the view's history
        // instead of being passed to the shell — same convention as
        // most terminals. Holding Shift sends them through to the PTY
        // for apps that actually use them (less, vim).
        let plain = !mods.control && !mods.alt && !mods.platform;
        if plain && !mods.shift {
            match key {
                "pageup" => {
                    self.backend.scroll_page_up();
                    return;
                }
                "pagedown" => {
                    self.backend.scroll_page_down();
                    return;
                }
                _ => {}
            }
        }

        let mode = self.backend.mode();
        if let Some(bytes) = keystroke_to_bytes(&event.keystroke, mode) {
            // Typing dismisses any active selection — both because
            // selection rendering would obscure the new prompt and
            // because the user has clearly moved on.
            self.backend.clear_selection();
            // Snap back to the active region whenever the user types
            // so the prompt doesn't disappear into history.
            self.backend.reset_scroll();
            self.backend.write_input(bytes);
        }
    }

    fn on_scroll_wheel(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        let lines = match event.delta {
            ScrollDelta::Pixels(point) => {
                let line_h: f32 = self.font.line_height.into();
                let py: f32 = point.y.into();
                if line_h > 0.0 { (py / line_h).round() as i32 } else { 0 }
            }
            ScrollDelta::Lines(point) => point.y.round() as i32,
        };
        if lines != 0 {
            // gpui reports +y for wheel-up, alacritty's Scroll::Delta
            // is +n for "scroll up into history" — same direction.
            self.backend.scroll(lines);
        }
    }

    fn maybe_resize(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let (cur_cols, cur_rows) = *self.last_size.lock();
        if cols == cur_cols && rows == cur_rows {
            return;
        }
        if cols == 0 || rows == 0 {
            return;
        }
        *self.last_size.lock() = (cols, rows);
        let cell_w_f32: f32 = self.font.cell_width.into();
        let cell_h_f32: f32 = self.font.line_height.into();
        self.backend.resize(TerminalSize {
            num_lines: rows,
            num_cols: cols,
            cell_width: cell_w_f32 as u16,
            cell_height: cell_h_f32 as u16,
        });
        self.snapshot = self.backend.snapshot(&self.palette);
        cx.notify();
    }
}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let line_height = self.font.line_height;
        let bg = self.snapshot.default_bg;
        let last_size = self.last_size.clone();
        let bounds_cache = self.bounds_cache.clone();
        let weak = cx.weak_entity();
        let font = self.font.to_font();
        let font_size = self.font.size;

        // The transparent canvas overlay measures the actual cell width
        // and line height from the gpui text system, then reports the
        // laid-out bounds back to the entity so we can resize the
        // PTY/grid in step with the window. It paints nothing.
        let resize_probe = canvas(
            move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
                // Stash the laid-out bounds so mouse handlers can map
                // window-pixel positions back to grid coordinates.
                *bounds_cache.lock() = Some(bounds);

                // Shape a single character through the same font stack
                // we render with. '│' is the conventional choice — TUI
                // fonts size it to fill the cell exactly.
                let probe_run = TextRun {
                    len: "│".len(),
                    font: font.clone(),
                    color: gpui::black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let shaped = window.text_system().shape_line(
                    "│".into(),
                    font_size,
                    &[probe_run],
                    None,
                );
                let measured_cell_w = shaped.width;
                let measured_line_h = (shaped.ascent + shaped.descent).ceil();

                let cell_w_f32: f32 = measured_cell_w.into();
                let line_h_f32: f32 = measured_line_h.into();
                let w: f32 = bounds.size.width.into();
                let h: f32 = bounds.size.height.into();
                if cell_w_f32 <= 0.0 || line_h_f32 <= 0.0 {
                    return;
                }
                let cols = (w / cell_w_f32).floor() as u16;
                let rows = (h / line_h_f32).floor() as u16;

                let (cur_cols, cur_rows) = *last_size.lock();
                if cols != cur_cols || rows != cur_rows {
                    weak
                        .update(cx, |view, cx| {
                            view.font.cell_width = measured_cell_w;
                            view.font.line_height = measured_line_h;
                            view.maybe_resize(cols, rows, cx);
                        })
                        .ok();
                }
            },
            |_bounds, _state, _window, _cx| {},
        )
        .absolute()
        .size_full();

        div()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .bg(bg)
            .font_family(self.font.family.clone())
            .text_size(self.font.size)
            .size_full()
            .relative()
            .child(resize_probe)
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .children(self.snapshot.lines.iter().map(|line| {
                        div()
                            .h(line_height)
                            .flex()
                            .flex_row()
                            .whitespace_nowrap()
                            .children(line.iter().map(render_run))
                    })),
            )
    }
}

fn render_run(run: &StyledRun) -> impl IntoElement {
    let mut element = div()
        .text_color(run.fg)
        .bg(run.bg)
        .child(run.text.clone());
    if run.bold {
        element = element.font_weight(FontWeight::BOLD);
    }
    if run.italic {
        element = element.italic();
    }
    if run.underline {
        element = element.underline();
    }
    element
}

