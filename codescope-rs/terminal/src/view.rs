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
    App, Bounds, Context, FocusHandle, Focusable, Font, FontFallbacks, FontFeatures, FontStyle,
    FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Render,
    SharedString, Styled, TextRun, Window, canvas, div, px,
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
        }
    }

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mode = self.backend.mode();
        if let Some(bytes) = keystroke_to_bytes(&event.keystroke, mode) {
            self.backend.write_input(bytes);
        }
        cx.stop_propagation();
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
        let weak = cx.weak_entity();
        let font = self.font.to_font();
        let font_size = self.font.size;

        // The transparent canvas overlay measures the actual cell width
        // and line height from the gpui text system, then reports the
        // laid-out bounds back to the entity so we can resize the
        // PTY/grid in step with the window. It paints nothing.
        let resize_probe = canvas(
            move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
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

