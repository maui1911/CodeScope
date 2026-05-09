//! gpui View layer for the terminal.
//!
//! Renders the visible grid through a single `canvas` element that
//! handles both measurement (cell metrics, grid resize) and painting
//! (default-bg quad, per-row non-default bg quads, batched text runs,
//! cursor on top). See [`crate::paint::paint_snapshot`] for the actual
//! pixel work.
//!
//! Architecture pointers:
//!
//! * Snapshotting happens on [`BackendEvent::Wakeup`] (and friends), so
//!   we don't poll. The async drain task lives for the entity's
//!   lifetime; dropping the entity drops the receiver and the loop
//!   exits cleanly.
//! * Keyboard input is converted via [`keystroke_to_bytes`] and pushed
//!   into the backend's input queue.
//! * The canvas measure phase reports its laid-out bounds back so the
//!   grid resize logic and mouse-coordinate translation can use them.

use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::backend::{Backend, TerminalSize, TerminalSnapshot};
use crate::colors::ColorPalette;
use crate::input::keystroke_to_bytes;
use crate::mouse::{self, MouseEventKind};
use crate::paint::paint_snapshot;
use gpui::{
    App, Bounds, ClipboardItem, Context, FocusHandle, Focusable, Font, FontFallbacks,
    FontFeatures, FontStyle, FontWeight, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point,
    Render, ScrollDelta, ScrollWheelEvent, SharedString, Styled, TextRun, Window, canvas, div, px,
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
    /// Pending resize request, waiting for the user to stop dragging.
    /// On Windows ConPTY, every resize causes conhost to dump the
    /// current viewport into scrollback — apply too often and the
    /// scrollback fills with duplicates. We hold off until the
    /// requested size has been stable for `RESIZE_DEBOUNCE` and only
    /// then call `Backend::resize`.
    pending_size: Arc<Mutex<Option<PendingResize>>>,
    /// Shared cache of the most recently laid-out terminal bounds.
    /// Mouse handlers read this to translate window-pixel positions to
    /// grid (line, column) coords.
    bounds_cache: Arc<Mutex<Option<Bounds<Pixels>>>>,
    /// `true` between `mouse_down` and `mouse_up` while the user is
    /// dragging out a selection. Mouse-move events are ignored unless
    /// this is set, so we don't accidentally extend the selection
    /// every time the cursor passes over the terminal.
    selecting: bool,
    /// Blink phase for blinking cursors. The renderer reads this in
    /// the canvas paint phase and skips the cursor when it's `false`.
    /// A background timer toggles it every 530 ms; input handlers set
    /// it back to `true` so the cursor doesn't disappear under typing.
    blink_phase: Arc<Mutex<bool>>,
}

impl TerminalView {
    /// Wrap a [`Backend`] in a gpui Entity. Spawns an async drain task
    /// that re-snapshots and notifies on every backend event.
    pub fn new(backend: Backend, cx: &mut Context<Self>) -> Self {
        Self::new_full(backend, ColorPalette::default(), FontConfig::default(), cx)
    }

    pub fn new_with_font(
        backend: Backend,
        font: FontConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        Self::new_full(backend, ColorPalette::default(), font, cx)
    }

    /// Full constructor — the binary uses this so the View's render
    /// palette matches whatever theme the [`Backend`] was spawned
    /// with. Callers that don't care about themes can keep using
    /// [`Self::new`] / [`Self::new_with_font`].
    pub fn new_full(
        backend: Backend,
        palette: ColorPalette,
        font: FontConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let snapshot = backend.snapshot(&palette);
        let events = backend.events();
        let blink_phase = Arc::new(Mutex::new(true));
        let pending_size: Arc<Mutex<Option<PendingResize>>> = Arc::new(Mutex::new(None));

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

        // Resize debounce task. Polls the pending request every
        // `RESIZE_POLL`; applies when the pending request has been
        // stable for `RESIZE_DEBOUNCE`. Same lifetime story as the
        // event drain task — dropping the entity ends the loop.
        let pending_for_timer = pending_size.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(RESIZE_POLL).await;
                let to_apply = {
                    let mut guard = pending_for_timer.lock();
                    match guard.as_ref() {
                        Some(req) if req.set_at.elapsed() >= RESIZE_DEBOUNCE => guard.take(),
                        _ => None,
                    }
                };
                if let Some(req) = to_apply {
                    if this
                        .update(cx, |view, cx| {
                            view.apply_resize(req.cols, req.rows, cx);
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            }
        })
        .detach();

        // Cursor blink timer. 530 ms is the conventional period (it's
        // what xterm and friends use). The async task lives for the
        // entity's lifetime; dropping the entity makes the next
        // `this.update` fail and the loop exits.
        let blink_phase_for_timer = blink_phase.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(530))
                    .await;
                {
                    let mut phase = blink_phase_for_timer.lock();
                    *phase = !*phase;
                }
                if this.update(cx, |_, cx| cx.notify()).is_err() {
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
            pending_size,
            bounds_cache: Arc::new(Mutex::new(None)),
            selecting: false,
            blink_phase,
        }
    }

    /// Snap the cursor to its visible phase. Called whenever the user
    /// types so the cursor never disappears mid-keystroke.
    fn show_cursor_now(&self) {
        *self.blink_phase.lock() = true;
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

    /// Pixel position → (visible_row, col) where both are 0-based and
    /// clamped to the visible viewport. Used for mouse-reporting
    /// encoding because TUIs expect viewport-relative coordinates,
    /// not the absolute scrollback grid.
    fn visible_rc(&self, position: Point<Pixels>) -> Option<(usize, usize)> {
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
        let row = (local_y / line_h).floor().max(0.0) as usize;
        Some((row, col))
    }

    /// Try to encode a mouse event for the running TUI. Returns
    /// `true` when reporting is on and the event was handled —
    /// caller should stop and *not* fall through to selection or
    /// scrollback.
    ///
    /// Shift bypasses mouse reporting (xterm convention) so the
    /// user can still drag-select inside `tmux`, `htop`, `vim`,
    /// etc. without disabling their mouse mode.
    fn try_report_mouse(
        &self,
        kind: MouseEventKind,
        button: Option<mouse::MouseButton>,
        modifiers: gpui::Modifiers,
        position: Point<Pixels>,
    ) -> bool {
        if modifiers.shift {
            return false;
        }
        let mode = self.backend.mode();
        if !mouse::mouse_reporting_enabled(mode) {
            return false;
        }
        // Motion events are noisy — only emit when the TUI asked
        // for motion (?1002 / ?1003).
        if matches!(kind, MouseEventKind::Motion) && !mouse::drag_reporting_enabled(mode) {
            return false;
        }
        let Some((row, col)) = self.visible_rc(position) else {
            return false;
        };
        let mods = mouse::Modifiers {
            shift: modifiers.shift,
            alt: modifiers.alt,
            control: modifiers.control,
        };
        if let Some(bytes) = mouse::encode(mode, kind, button, mods, col, row) {
            self.backend.write_input(bytes);
        }
        true
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
        // Try mouse-reporting first. When a TUI like tmux/htop/vim
        // is in mouse mode, the click belongs to it — only fall
        // through to selection when reporting is off (or the user
        // held Shift to bypass).
        if let Some(button) = to_mouse_button(event.button) {
            if self.try_report_mouse(
                MouseEventKind::Press,
                Some(button),
                event.modifiers,
                event.position,
            ) {
                return;
            }
        }
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
        // Forward motion events to the TUI when it asked for them.
        // We only emit motion-with-button-held; pointer motion with
        // no button is rare and noisy, and `?1003h` apps that want
        // it can re-enable later.
        let pressed = event.pressed_button.and_then(to_mouse_button);
        if pressed.is_some()
            && self.try_report_mouse(
                MouseEventKind::Motion,
                pressed,
                event.modifiers,
                event.position,
            )
        {
            return;
        }
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
        event: &MouseUpEvent,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if let Some(button) = to_mouse_button(event.button) {
            if self.try_report_mouse(
                MouseEventKind::Release,
                Some(button),
                event.modifiers,
                event.position,
            ) {
                self.selecting = false;
                return;
            }
        }
        self.selecting = false;
    }

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let mods = &event.keystroke.modifiers;

        // Let app-level shortcuts (Ctrl+T new tab, Ctrl+W close,
        // Ctrl+Tab cycle, Ctrl+1-9 select) bubble up to whichever
        // ancestor is listening for them. Without this skip the
        // `stop_propagation` below would swallow them and the only
        // way to switch tabs would be a mouse click — exactly the
        // bug the user reported. Done here, in the terminal, instead
        // of by keymap registration up in `AppShell` so we don't have
        // to teach the terminal about every embedder's bindings.
        if is_app_level_shortcut(key, mods) {
            return;
        }

        cx.stop_propagation();

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

        // Paste:
        //   * Ctrl+Shift+V (Windows / Linux convention) — always paste.
        //   * Cmd+V       (macOS convention)            — always paste.
        //   * Ctrl+V                                    — paste when
        //     the TUI has enabled bracketed-paste mode (claude-code,
        //     vim, modern shells), so it gets a proper paste event
        //     instead of `\x16`. When bracketed-paste is OFF (cmd.exe,
        //     bare bash) we fall through to sending `\x16` so
        //     readline's quoted-insert and PSReadLine's own paste
        //     binding still work.
        let always_paste = (mods.control && mods.shift && key == "v")
            || (mods.platform && key == "v" && !mods.control && !mods.alt);
        let smart_ctrl_v = mods.control
            && !mods.shift
            && !mods.alt
            && !mods.platform
            && key == "v"
            && self
                .backend
                .mode()
                .contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE);
        if always_paste || smart_ctrl_v {
            if let Some(item) = cx.read_from_clipboard()
                && let Some(text) = item.text()
            {
                self.paste(&text);
                self.show_cursor_now();
            }
            return;
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
            self.show_cursor_now();
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
        if lines == 0 {
            return;
        }
        // Forward wheel events to the TUI when it asked for mouse
        // input. tmux / less / vim use this for paging. Each cell-
        // worth of scroll emits one wheel event so a fast spin
        // doesn't lose ticks.
        let mode = self.backend.mode();
        if mouse::mouse_reporting_enabled(mode) && !event.modifiers.shift {
            let kind = if lines > 0 {
                MouseEventKind::WheelUp
            } else {
                MouseEventKind::WheelDown
            };
            let mods = mouse::Modifiers {
                shift: event.modifiers.shift,
                alt: event.modifiers.alt,
                control: event.modifiers.control,
            };
            let (row, col) = self.visible_rc(event.position).unwrap_or((0, 0));
            for _ in 0..lines.unsigned_abs() {
                if let Some(bytes) = mouse::encode(mode, kind, None, mods, col, row) {
                    self.backend.write_input(bytes);
                }
            }
            return;
        }
        // gpui reports +y for wheel-up, alacritty's Scroll::Delta
        // is +n for "scroll up into history" — same direction.
        self.backend.scroll(lines);
    }

    /// Write clipboard text into the PTY. Honours bracketed-paste mode
    /// (`ESC[?2004h`) so TUIs like vim, claude-code, and bash >= 4.1
    /// can distinguish paste from typed input — without it, multi-line
    /// pastes get interpreted as ENTER key sequences and run commands
    /// the user didn't intend.
    ///
    /// CR/CRLF are normalised to LF: Windows clipboards typically hand
    /// us CRLF, but a PTY treats CR as ENTER, so a multi-line paste
    /// would otherwise execute every line.
    fn paste(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let normalised = text.replace("\r\n", "\n").replace('\r', "\n");
        let mode = self.backend.mode();
        if mode.contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE) {
            let mut bytes = Vec::with_capacity(normalised.len() + 12);
            bytes.extend_from_slice(b"\x1b[200~");
            bytes.extend_from_slice(normalised.as_bytes());
            bytes.extend_from_slice(b"\x1b[201~");
            self.backend.write_input(bytes);
        } else {
            self.backend.write_input(normalised.into_bytes());
        }
        self.backend.reset_scroll();
    }

    /// Stage a resize request. Cheap — the actual call into the
    /// backend happens later, in [`Self::apply_resize`], when the
    /// request has been stable for [`RESIZE_DEBOUNCE`].
    fn maybe_resize(&self, cols: u16, rows: u16, _cx: &mut Context<Self>) {
        if cols == 0 || rows == 0 {
            return;
        }
        let (cur_cols, cur_rows) = *self.last_size.lock();
        if cols == cur_cols && rows == cur_rows {
            // Already at the target; if a stale pending request would
            // try to undo it, drop it.
            self.pending_size.lock().take();
            return;
        }
        *self.pending_size.lock() = Some(PendingResize {
            cols,
            rows,
            set_at: Instant::now(),
        });
    }

    /// Actually call `Backend::resize` and refresh the snapshot. Driven
    /// by the debounce task spawned in `new_with_font`.
    fn apply_resize(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let (cur_cols, cur_rows) = *self.last_size.lock();
        if cols == cur_cols && rows == cur_rows {
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

/// Debounce window for grid resize. ConPTY duplicates viewport
/// content into scrollback on every resize on Windows; resizing 60×
/// per second during a drag fills the buffer with garbage. 120 ms is
/// short enough to feel snappy when the user finishes dragging,
/// long enough that intermediate sizes during a drag are skipped.
const RESIZE_DEBOUNCE: Duration = Duration::from_millis(120);

/// Polling interval for the debounce timer. Independent of
/// `RESIZE_DEBOUNCE`: shorter than the debounce so we react quickly
/// once the user stops dragging.
const RESIZE_POLL: Duration = Duration::from_millis(40);

#[derive(Copy, Clone)]
struct PendingResize {
    cols: u16,
    rows: u16,
    set_at: Instant,
}

/// Map gpui's `MouseButton` enum to the wire-encoded button this
/// crate uses for mouse-reporting. Returns `None` for buttons we
/// don't translate (gpui has Navigate variants for back/forward
/// thumb buttons that no in-the-wild TUI cares about).
fn to_mouse_button(button: MouseButton) -> Option<mouse::MouseButton> {
    match button {
        MouseButton::Left => Some(mouse::MouseButton::Left),
        MouseButton::Middle => Some(mouse::MouseButton::Middle),
        MouseButton::Right => Some(mouse::MouseButton::Right),
        _ => None,
    }
}

/// Keystrokes the terminal should *not* swallow. A parent shell
/// (tab strip, command palette, …) is expected to handle them.
/// Kept conservative — we only opt out of bindings nothing in a
/// shell prompt would meaningfully use, so power-users running vim
/// or readline still get every key they need.
fn is_app_level_shortcut(key: &str, mods: &gpui::Modifiers) -> bool {
    let app_mod = mods.control || mods.platform;
    if !app_mod || mods.alt {
        return false;
    }
    // Ctrl+Tab / Ctrl+Shift+Tab (tab cycling).
    if key == "tab" {
        return true;
    }
    // Ctrl+1 .. Ctrl+9 (direct tab select).
    if !mods.shift
        && key.len() == 1
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() && c != '0')
    {
        return true;
    }
    // Ctrl+Shift+T / Ctrl+Shift+W — explicit "always" bindings that
    // never conflict with anything readline-shaped. Plain Ctrl+T /
    // Ctrl+W would clash with the shell's word/transpose, so the
    // app-shell uses the shifted variants by convention.
    if mods.shift && (key == "t" || key == "w") {
        return true;
    }
    false
}

/// State handed from the canvas measure phase to the paint phase.
struct CanvasLayout {
    bounds: Bounds<Pixels>,
    cell_width: Pixels,
    line_height: Pixels,
}

impl Render for TerminalView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bg = self.snapshot.default_bg;
        let last_size = self.last_size.clone();
        let bounds_cache = self.bounds_cache.clone();
        let weak = cx.weak_entity();
        let font = self.font.to_font();
        let font_size = self.font.size;
        let snapshot = self.snapshot.clone();
        let blink_visible = *self.blink_phase.lock();

        let canvas_element = canvas(
            // Layout phase: measure the cell, stash bounds for mouse
            // coords, and trigger a grid resize if dimensions changed.
            // The returned `CanvasLayout` is forwarded to the paint
            // phase so we don't re-shape the probe glyph.
            {
                let bounds_cache = bounds_cache.clone();
                let last_size = last_size.clone();
                let font = font.clone();
                move |bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
                    *bounds_cache.lock() = Some(bounds);

                    // '│' (BOX DRAWINGS LIGHT VERTICAL) — terminal
                    // fonts size it to fill the cell exactly, so it's
                    // the canonical measurement glyph.
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
                    let cell_width = shaped.width;
                    let line_height = (shaped.ascent + shaped.descent).ceil();

                    let cell_w_f32: f32 = cell_width.into();
                    let line_h_f32: f32 = line_height.into();
                    if cell_w_f32 > 0.0 && line_h_f32 > 0.0 {
                        let w: f32 = bounds.size.width.into();
                        let h: f32 = bounds.size.height.into();
                        let cols = (w / cell_w_f32).floor() as u16;
                        let rows = (h / line_h_f32).floor() as u16;

                        let (cur_cols, cur_rows) = *last_size.lock();
                        if cols != cur_cols || rows != cur_rows {
                            weak.update(cx, |view, cx| {
                                view.font.cell_width = cell_width;
                                view.font.line_height = line_height;
                                view.maybe_resize(cols, rows, cx);
                            })
                            .ok();
                        }
                    }

                    CanvasLayout {
                        bounds,
                        cell_width,
                        line_height,
                    }
                }
            },
            // Paint phase: emit quads + shaped text from the cloned
            // snapshot.
            move |_bounds, layout: CanvasLayout, window: &mut Window, cx: &mut App| {
                let cell_w_f32: f32 = layout.cell_width.into();
                let line_h_f32: f32 = layout.line_height.into();
                if cell_w_f32 <= 0.0 || line_h_f32 <= 0.0 {
                    return;
                }
                paint_snapshot(
                    layout.bounds,
                    &snapshot,
                    &font,
                    font_size,
                    layout.cell_width,
                    layout.line_height,
                    blink_visible,
                    window,
                    cx,
                );
            },
        )
        .size_full();

        div()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_scroll_wheel(cx.listener(Self::on_scroll_wheel))
            // Listen for *every* button, not just Left — TUIs in
            // mouse mode want right-click context menus and
            // middle-click paste / scroll events too. The handler
            // routes each event through `try_report_mouse` first,
            // so non-reporting selections still see Left only.
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Middle, cx.listener(Self::on_mouse_down))
            .on_mouse_down(MouseButton::Right, cx.listener(Self::on_mouse_down))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Middle, cx.listener(Self::on_mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::on_mouse_up))
            .bg(bg)
            .font_family(self.font.family.clone())
            .text_size(self.font.size)
            .size_full()
            .child(canvas_element)
    }
}

