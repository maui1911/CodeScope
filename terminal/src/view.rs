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

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::backend::{Backend, TerminalSize, TerminalSnapshot};
use crate::colors::ColorPalette;
use crate::input::keystroke_to_bytes;
use crate::mouse::{self, MouseEventKind};
use crate::paint::paint_snapshot;
use gpui::{
    App, Bounds, ClipboardEntry, ClipboardItem, Context, FocusHandle, Focusable, Font, FontFallbacks,
    FontFeatures, FontStyle, FontWeight, Image, ImageFormat, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels,
    Point, Render, ScrollDelta, ScrollWheelEvent, SharedString, Styled, TextRun, Window, canvas,
    div, px,
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
    /// Directory the PTY was spawned in. Screenshot paste stores
    /// clipboard images under this root and inserts a relative path
    /// into the prompt.
    working_directory: Option<PathBuf>,
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
    /// URI of the OSC 8 hyperlink currently under the mouse, if any.
    /// Drives the pointer cursor — render reads this on every frame
    /// and applies `cursor_pointer()` when set. Updated on
    /// mouse-move; we only `cx.notify()` when the value flips so
    /// motion across non-hyperlink cells doesn't repaint the world.
    hovered_link: Option<Arc<str>>,
}

impl TerminalView {
    /// Wrap a [`Backend`] in a gpui Entity. Spawns an async drain task
    /// that re-snapshots and notifies on every backend event.
    ///
    /// `palette` should match the palette the backend was spawned
    /// with so renderer cell colours line up; `font` carries the
    /// family + size + ligatures choice. `working_directory` is used
    /// by image paste to save screenshots into the active project /
    /// worktree instead of a global temp folder; pass `None` to fall
    /// back to the temp folder.
    pub fn new(
        backend: Backend,
        palette: ColorPalette,
        font: FontConfig,
        working_directory: Option<PathBuf>,
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
                if let Some(req) = to_apply
                    && this
                        .update(cx, |view, cx| {
                            view.apply_resize(req.cols, req.rows, cx);
                        })
                        .is_err()
                    {
                        break;
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
            working_directory,
            focus_handle,
            last_size: Arc::new(Mutex::new((0, 0))),
            pending_size,
            bounds_cache: Arc::new(Mutex::new(None)),
            selecting: false,
            blink_phase,
            hovered_link: None,
        }
    }

    /// Write raw bytes to the pty as if the user had typed them.
    /// Lets app shells inject startup commands (e.g. auto-type
    /// `claude\r` after opening a tab) without going through the
    /// keyboard pipeline.
    ///
    /// I/O-safe to call right after `Backend::spawn` — the bytes
    /// queue on the slave side and the shell consumes them once it
    /// starts reading. **UX-wise** that isn't always what you want:
    /// pwsh on Windows prints a banner before its REPL starts
    /// reading, and bytes that land before the prompt can get echoed
    /// into the banner instead of executed. Callers that want the
    /// command to run *as if* typed at the prompt should give the
    /// shell a moment to settle (a short timer, or wait for the
    /// first idle event from the backend) before calling this.
    pub fn write_input<B>(&self, bytes: B)
    where
        B: Into<std::borrow::Cow<'static, [u8]>>,
    {
        self.backend.write_input(bytes);
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
        // Only claim the event when we actually emit bytes. If
        // `mouse::encode` returns `None` (e.g. X10 overflow on a
        // 300-col terminal) we'd otherwise swallow the click —
        // returning `false` here lets the View fall back to
        // selection / scrollback so the user gets *some* response
        // instead of a silent dead spot.
        if let Some(bytes) = mouse::encode(mode, kind, button, mods, col, row) {
            self.backend.write_input(bytes);
            true
        } else {
            false
        }
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
        // Ctrl/Cmd-click on a hyperlink always opens it, even when
        // a TUI has mouse mode on. Ctrl is the universal "bypass
        // mouse reporting for one click" convention (Shift handles
        // the same job for selection); without this short-circuit
        // tmux / vim would swallow Ctrl+click and the user could
        // never open a link inside them.
        let modifier = event.modifiers.control || event.modifiers.platform;
        let is_left = event.button == MouseButton::Left;
        if is_left && modifier && !event.modifiers.shift
            && let Some((row, col)) = self.visible_rc(event.position)
                && let Some(uri) = self.snapshot.hyperlink_at(row, col) {
                    let _ = open::that_detached(uri.as_ref());
                    return;
                }

        // Try mouse-reporting next. When a TUI like tmux/htop/vim
        // is in mouse mode, the click belongs to it — only fall
        // through to selection when reporting is off (or the user
        // held Shift to bypass).
        if let Some(button) = to_mouse_button(event.button)
            && self.try_report_mouse(
                MouseEventKind::Press,
                Some(button),
                event.modifiers,
                event.position,
            ) {
                return;
            }
        if !is_left {
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
        // Track hover-over-hyperlink so render can flip the cursor
        // to a pointer. Only re-notify when the URI actually changes,
        // otherwise every pointer wobble would force a repaint.
        let next_link = self
            .visible_rc(event.position)
            .and_then(|(row, col)| self.snapshot.hyperlink_at(row, col));
        if next_link.as_deref() != self.hovered_link.as_deref() {
            self.hovered_link = next_link;
            cx.notify();
        }

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
        if let Some(button) = to_mouse_button(event.button)
            && self.try_report_mouse(
                MouseEventKind::Release,
                Some(button),
                event.modifiers,
                event.position,
            ) {
                self.selecting = false;
                return;
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
        let any_clipboard_paste_chord = key == "v"
            && !mods.alt
            && ((mods.control && !mods.platform) || (mods.platform && !mods.control));
        if any_clipboard_paste_chord {
            if let Some(item) = cx.read_from_clipboard() {
                // Image paste is CodeScope-specific: terminal PTYs can
                // only receive text, so store the clipboard image in
                // the tab's worktree and paste the relative path. Do
                // this even for plain Ctrl+V when bracketed paste is
                // off; otherwise the shell would only receive ^V.
                if let Some(image) = clipboard_image(&item) {
                    match self.save_clipboard_image(image) {
                        Ok(path) => {
                            self.paste(&path);
                            self.show_cursor_now();
                            return;
                        }
                        Err(err) => {
                            // Image save failed — fall through to the
                            // legacy text / \x16 paste path so the key
                            // press is never silently swallowed.
                            eprintln!("failed to save clipboard image attachment: {err:#}");
                        }
                    }
                }

                if always_paste || smart_ctrl_v {
                    if let Some(text) = item.text() {
                        self.paste(&text);
                        self.show_cursor_now();
                    }
                    return;
                }
            } else if always_paste || smart_ctrl_v {
                return;
            }
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

    fn save_clipboard_image(&self, image: &Image) -> anyhow::Result<String> {
        let saved = codescope_core::save_attachment_bytes(
            self.working_directory.as_deref(),
            image_extension(image.format()),
            image.bytes(),
        )?;
        Ok(saved.paste_path)
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
    ///
    /// **`set_at` is preserved when the staged target is unchanged.**
    /// Canvas-layout runs on every render tick, not only when bounds
    /// shift — a blinking cursor, fresh terminal output, or any
    /// sibling-driven entity notify will re-fire layout against the
    /// (now-mismatching-with-`last_size`) bounds. The old code
    /// unconditionally reset `set_at = Instant::now()` on every call,
    /// so the polling task's `set_at.elapsed() >= RESIZE_DEBOUNCE`
    /// check never tripped while the entity was being re-rendered
    /// for any other reason: a 5.8 MB diagnostic tape captured
    /// 38 308 staged calls against just 59 successful `apply_resize`
    /// runs (650 : 1) — `last_size` stayed stale, the canvas-layout
    /// `diff` check stayed `true` forever, and `Backend::resize`
    /// never fired so the terminal grid never picked up the new
    /// window / splitter size. User-reported on rc.10
    /// (`cargo run --release`): resize the window or drag the
    /// splitter and the terminal pane stays at its pre-transition
    /// size until you tab-swap.
    ///
    /// Fix: when the currently-staged request already targets the
    /// same `(cols, rows)`, leave its `set_at` alone so the
    /// debounce window can actually elapse. A *different* target
    /// (the user is still dragging the window edge / splitter)
    /// still resets the timer — that's the original "don't apply
    /// mid-drag" invariant the debounce was built for. Once
    /// `apply_resize` runs, `last_size` updates, the canvas-layout
    /// `diff` flips to `false`, and `maybe_resize` stops being
    /// called from idle re-renders.
    fn maybe_resize(&self, cols: u16, rows: u16, _cx: &mut Context<Self>) {
        if cols == 0 || rows == 0 {
            crate::diag::log(&format!(
                "maybe_resize: skipped (zero-dim) cols={cols} rows={rows}"
            ));
            return;
        }
        let (cur_cols, cur_rows) = *self.last_size.lock();
        if cols == cur_cols && rows == cur_rows {
            // Already at the target; if a stale pending request would
            // try to undo it, drop it.
            self.pending_size.lock().take();
            crate::diag::log(&format!(
                "maybe_resize: noop (already_at_target) cols={cols} rows={rows}"
            ));
            return;
        }
        let mut pending = self.pending_size.lock();
        match pending.as_mut() {
            Some(req) if req.cols == cols && req.rows == rows => {
                // Same target already pending — let the existing
                // `set_at` age toward `RESIZE_DEBOUNCE` instead of
                // resetting it every idle re-render. See the doc
                // comment on `maybe_resize` for the full rationale.
                // No log noise here: this branch hits on every
                // idle re-render and would flood the tape.
            }
            _ => {
                *pending = Some(PendingResize {
                    cols,
                    rows,
                    set_at: Instant::now(),
                });
                crate::diag::log(&format!(
                    "maybe_resize: staged cols={cols} rows={rows} from cur=({cur_cols},{cur_rows})"
                ));
            }
        }
    }

    /// Actually call `Backend::resize` and refresh the snapshot. Driven
    /// by the debounce task spawned in `new_with_font`.
    fn apply_resize(&mut self, cols: u16, rows: u16, cx: &mut Context<Self>) {
        let (cur_cols, cur_rows) = *self.last_size.lock();
        if cols == cur_cols && rows == cur_rows {
            crate::diag::log(&format!(
                "apply_resize: noop (already_at_target) cols={cols} rows={rows}"
            ));
            return;
        }
        *self.last_size.lock() = (cols, rows);
        let cell_w_f32: f32 = self.font.cell_width.into();
        let cell_h_f32: f32 = self.font.line_height.into();
        crate::diag::log(&format!(
            "apply_resize: backend.resize cols={cols} rows={rows} \
             from cur=({cur_cols},{cur_rows}) cell=({cell_w_f32:.2},{cell_h_f32:.2})"
        ));
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

fn clipboard_image(item: &ClipboardItem) -> Option<&Image> {
    item.entries().iter().find_map(|entry| match entry {
        ClipboardEntry::Image(image) => Some(image),
        ClipboardEntry::String(_) => None,
    })
}

fn image_extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "png",
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        ImageFormat::Gif => "gif",
        ImageFormat::Svg => "svg",
        ImageFormat::Bmp => "bmp",
        ImageFormat::Tiff => "tiff",
    }
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
///
/// Mirrors the AppShell `on_key_down` chord set so a chord typed
/// while the terminal has focus bubbles up to the shell instead of
/// being eaten + forwarded to the PTY.
///
/// **All app chords require Ctrl+Shift** so they don't collide with
/// the wealth of plain-Ctrl bindings coding agents inside the
/// terminal rely on (Ctrl+W = backward-kill-word, Ctrl+P = previous
/// history, Ctrl+T = transpose, Ctrl+B = backward-char, Ctrl+1..9
/// often mapped to history selection, …). Plain Ctrl+letter falls
/// through to the PTY untouched.
///
/// The only chords still on plain Ctrl are Ctrl+Tab / Ctrl+Shift+Tab
/// (no shell binds Ctrl+Tab — shift is intrinsic to "prev" tab).
pub(crate) fn is_app_level_shortcut(key: &str, mods: &gpui::Modifiers) -> bool {
    let app_mod = mods.control || mods.platform;
    if !app_mod || mods.alt {
        return false;
    }
    // Ctrl+Tab / Ctrl+Shift+Tab — tab cycling. Shells don't bind
    // Ctrl+Tab; let both shapes bubble.
    if key == "tab" {
        return true;
    }
    // Ctrl+Shift+1..9 — focus tab N. Plain Ctrl+1..9 stays with
    // the terminal so the agent inside can use them.
    //
    // gpui's Windows keyboard adapter folds Shift+digit into the
    // shifted glyph (Shift+1 ⇒ "!", Shift+2 ⇒ "@", …) and clears
    // `mods.shift`. Accept both shapes:
    //
    //   - "1".."9" with shift still set (some non-US layouts).
    //   - The US-layout shifted glyphs "!@#$%^&*(" with shift
    //     cleared (the common Windows case).
    //
    // "0" / ")" is intentionally not matched — Ctrl+Shift+0 is
    // unbound to mirror the AppShell handler.
    if mods.shift
        && key.len() == 1
        && key
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit() && c != '0')
    {
        return true;
    }
    // The shifted-glyph form only counts as an app chord when shift
    // has already been consumed by the platform adapter — same rule
    // as `AppShell::keystroke_digit_index`. If a layout produces the
    // glyph *with* shift still set, the shell would refuse to bind
    // it; bubbling here would then drop the keystroke entirely. Keep
    // the whitelist and the shell handler in lockstep.
    if !mods.shift && matches!(key, "!" | "@" | "#" | "$" | "%" | "^" | "&" | "*" | "(") {
        return true;
    }
    // Ctrl+Shift+T (new tab) / Ctrl+Shift+W (close tab). Plain
    // Ctrl+T / Ctrl+W stay with the terminal — agents rely on
    // them (transpose / backward-kill-word in readline).
    if (key == "t" || key == "w") && mods.shift {
        return true;
    }
    // Ctrl+Shift+\ (split right). On US layouts the gpui Windows
    // adapter folds Shift+\ into "|" and clears `mods.shift`, so
    // we accept both shapes.
    if key == "\\" && mods.shift {
        return true;
    }
    if key == "|" {
        return true;
    }
    // Ctrl+Shift+P — command palette. Plain Ctrl+P stays with the
    // terminal so readline previous-history keeps working.
    if key == "p" && mods.shift {
        return true;
    }
    // Ctrl+Shift+O — overview pane.
    if key == "o" && mods.shift {
        return true;
    }
    // Ctrl+Shift+D — diff viewer. Plain Ctrl+D stays with the
    // terminal (EOF / readline delete-char).
    if key == "d" && mods.shift {
        return true;
    }
    // Ctrl+Shift+B — toggle sidebar. Plain Ctrl+B stays with the
    // terminal (readline backward-char).
    if key == "b" && mods.shift {
        return true;
    }
    // Ctrl+Shift+, — open Settings. Plain Ctrl+, stays with the
    // terminal in case the agent / shell has bound it. Accept both
    // shapes: bare `","` with shift (non-US / non-Windows) and
    // US-Windows folded `"<"` with shift cleared — gpui's Windows
    // adapter folds shifted punctuation the same way it folds
    // shifted digits. Without the second arm the chord is lost
    // because the bubble-up check never matches.
    if key == "," && mods.shift {
        return true;
    }
    if key == "<" && !mods.shift {
        return true;
    }
    // Ctrl+Shift+G — open active tab's worktree remote in browser.
    // Plain Ctrl+G stays with the terminal (readline abort).
    if key == "g" && mods.shift {
        return true;
    }
    // Ctrl+Shift+R — open active tab's PR in browser. Plain Ctrl+R
    // is readline reverse-history-search and stays with the
    // terminal.
    if key == "r" && mods.shift {
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
                    let bw: f32 = bounds.size.width.into();
                    let bh: f32 = bounds.size.height.into();
                    if cell_w_f32 > 0.0 && line_h_f32 > 0.0 {
                        // Lock acquisition scoped to the branch that
                        // actually reads `last_cols`/`last_rows` — the
                        // zero-dim branch below only logs that it
                        // skipped, so paying for the mutex on every
                        // first-frame render before font metrics
                        // resolve is wasted work on the hot path.
                        let (last_cols, last_rows) = *last_size.lock();
                        let cols = (bw / cell_w_f32).floor() as u16;
                        let rows = (bh / line_h_f32).floor() as u16;
                        crate::diag::log(&format!(
                            "canvas_layout: bounds=({bw:.1}x{bh:.1}) cell=({cell_w_f32:.2},{line_h_f32:.2}) \
                             computed=({cols},{rows}) last=({last_cols},{last_rows}) diff={}",
                            cols != last_cols || rows != last_rows
                        ));

                        if cols != last_cols || rows != last_rows {
                            weak.update(cx, |view, cx| {
                                view.font.cell_width = cell_width;
                                view.font.line_height = line_height;
                                view.maybe_resize(cols, rows, cx);
                            })
                            .ok();
                        }
                    } else {
                        crate::diag::log(&format!(
                            "canvas_layout: bounds=({bw:.1}x{bh:.1}) cell=(zero-dim, skipping resize)"
                        ));
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

        let pointer_for_link = self.hovered_link.is_some();

        let mut root = div()
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
            .size_full();
        if pointer_for_link {
            root = root.cursor_pointer();
        }
        root.child(canvas_element)
    }
}

#[cfg(test)]
mod tests {
    use super::{clipboard_image, image_extension, is_app_level_shortcut};
    use gpui::{ClipboardItem, Image, ImageFormat, Modifiers};

    fn ctrl() -> Modifiers {
        Modifiers {
            control: true,
            ..Default::default()
        }
    }

    fn ctrl_shift() -> Modifiers {
        Modifiers {
            control: true,
            shift: true,
            ..Default::default()
        }
    }

    fn ctrl_alt() -> Modifiers {
        Modifiers {
            control: true,
            alt: true,
            ..Default::default()
        }
    }

    #[test]
    fn clipboard_image_detects_image_entries() {
        let image = Image::from_bytes(ImageFormat::Png, b"png".to_vec());
        let item = ClipboardItem::new_image(&image);

        assert_eq!(clipboard_image(&item).map(Image::bytes), Some(b"png".as_slice()));
        assert!(clipboard_image(&ClipboardItem::new_string("text".to_string())).is_none());
    }

    #[test]
    fn image_extension_maps_agent_friendly_extensions() {
        assert_eq!(image_extension(ImageFormat::Png), "png");
        assert_eq!(image_extension(ImageFormat::Jpeg), "jpg");
        assert_eq!(image_extension(ImageFormat::Webp), "webp");
    }

    #[test]
    fn ctrl_shift_t_bubbles_to_app_shell() {
        // Ctrl+Shift+T is the universal new-tab chord. Plain Ctrl+T
        // stays with the terminal (readline transpose-char).
        assert!(is_app_level_shortcut("t", &ctrl_shift()));
        assert!(!is_app_level_shortcut("t", &ctrl()));
    }

    #[test]
    fn ctrl_shift_w_bubbles_to_app_shell() {
        // Ctrl+Shift+W closes the tab. Plain Ctrl+W stays with the
        // terminal — readline binds it to backward-kill-word, and
        // taking it away would break every coding agent CLI.
        assert!(is_app_level_shortcut("w", &ctrl_shift()));
        assert!(!is_app_level_shortcut("w", &ctrl()));
    }

    #[test]
    fn ctrl_tab_and_shift_tab_bubble() {
        // Ctrl+Tab / Ctrl+Shift+Tab are the only chords still on
        // plain Ctrl — no shell binds Ctrl+Tab.
        assert!(is_app_level_shortcut("tab", &ctrl()));
        assert!(is_app_level_shortcut("tab", &ctrl_shift()));
    }

    #[test]
    fn ctrl_shift_digit_bubbles_only_for_one_through_nine() {
        // Bare-digit shape (non-US layouts and most non-Windows
        // platforms): "1".."9" with shift still set.
        for d in '1'..='9' {
            let key = d.to_string();
            assert!(is_app_level_shortcut(&key, &ctrl_shift()), "digit {d}");
        }
        // US-layout shifted-glyph shape: gpui's Windows adapter
        // returns "!@#$%^&*(" with shift cleared.
        for glyph in ["!", "@", "#", "$", "%", "^", "&", "*", "("] {
            assert!(is_app_level_shortcut(glyph, &ctrl()), "glyph {glyph}");
        }
        // Ctrl+Shift+0 / Ctrl+) intentionally unbound.
        assert!(!is_app_level_shortcut("0", &ctrl_shift()));
        assert!(!is_app_level_shortcut(")", &ctrl()));
        // Glyph form with shift still set: layouts that don't fold
        // the shifted character must not bubble (the shell would
        // reject the same shape — see `keystroke_digit_index` in
        // `app.rs`). Without this guard the keystroke is lost.
        for glyph in ["!", "@", "#", "$", "%", "^", "&", "*", "("] {
            assert!(
                !is_app_level_shortcut(glyph, &ctrl_shift()),
                "glyph {glyph} with shift still set must not bubble"
            );
        }
    }

    #[test]
    fn plain_ctrl_digit_stays_with_terminal() {
        // Plain Ctrl+1..9 (no shift) is *not* an app chord any more
        // — agents inside the terminal use Ctrl+digit. This is the
        // central guarantee of the Ctrl+Shift universal remap.
        for d in '1'..='9' {
            let key = d.to_string();
            assert!(
                !is_app_level_shortcut(&key, &ctrl()),
                "plain Ctrl+{d} must stay with the terminal"
            );
        }
    }

    #[test]
    fn ctrl_shift_backslash_bubbles_for_split() {
        // Both bare Ctrl+Shift+\ and the US-layout-folded "|"
        // shape map to split-right.
        assert!(is_app_level_shortcut("\\", &ctrl_shift()));
        assert!(is_app_level_shortcut("|", &ctrl()));
        // Plain Ctrl+\ stays with the terminal.
        assert!(!is_app_level_shortcut("\\", &ctrl()));
    }

    #[test]
    fn alt_chord_stays_with_terminal() {
        // Alt+Left/Right etc. are app-level for group focus, but
        // they aren't app-level *here* — gpui delivers them at the
        // window root, not via terminal bubbling.
        assert!(!is_app_level_shortcut("left", &ctrl_alt()));
    }

    #[test]
    fn ctrl_c_v_stay_with_terminal() {
        // Copy / paste are handled inside the terminal (selection
        // semantics + bracketed paste), not bubbled to AppShell.
        assert!(!is_app_level_shortcut("c", &ctrl()));
        assert!(!is_app_level_shortcut("v", &ctrl()));
    }

    #[test]
    fn unmodified_keys_stay_with_terminal() {
        let plain = Modifiers::default();
        assert!(!is_app_level_shortcut("t", &plain));
        assert!(!is_app_level_shortcut("a", &plain));
    }

    #[test]
    fn ctrl_shift_p_bubbles_for_palette_only() {
        // Plain Ctrl+P is readline previous-history, kept with the
        // terminal. Only Ctrl+Shift+P opens the palette now.
        assert!(is_app_level_shortcut("p", &ctrl_shift()));
        assert!(!is_app_level_shortcut("p", &ctrl()));
    }

    #[test]
    fn ctrl_shift_o_bubbles_for_overview() {
        assert!(is_app_level_shortcut("o", &ctrl_shift()));
        assert!(!is_app_level_shortcut("o", &ctrl()));
    }

    #[test]
    fn ctrl_shift_b_bubbles_for_sidebar() {
        // Ctrl+Shift+B opens the sidebar. Plain Ctrl+B is readline
        // backward-char and stays with the terminal.
        assert!(is_app_level_shortcut("b", &ctrl_shift()));
        assert!(!is_app_level_shortcut("b", &ctrl()));
    }

    #[test]
    fn ctrl_shift_comma_bubbles_in_both_keystroke_shapes() {
        // Ctrl+Shift+, opens the Settings dialog. gpui surfaces the
        // chord in two shapes depending on platform / layout — both
        // must bubble or the chord silently doesn't work on US
        // Windows (the most common install):
        //
        //   - bare `","` with `mods.shift` set (non-US layouts, most
        //     non-Windows platforms)
        //   - US-Windows folded `"<"` with shift cleared (same
        //     folding the Windows adapter does for shifted digits →
        //     `!@#$%^&*(`)
        assert!(is_app_level_shortcut(",", &ctrl_shift()));
        assert!(is_app_level_shortcut("<", &ctrl()));
        // Plain Ctrl+, stays with the terminal in case the agent /
        // shell has bound it. Plain `<` with shift still set is the
        // "non-folding layout sent the shifted character anyway"
        // edge case — also stays with the terminal so we don't
        // double-fire.
        assert!(!is_app_level_shortcut(",", &ctrl()));
        assert!(!is_app_level_shortcut("<", &ctrl_shift()));
    }

    #[test]
    fn ctrl_shift_g_bubbles_for_open_remote() {
        // Ctrl+Shift+G opens the active tab's worktree remote in the
        // browser. Plain Ctrl+G is readline abort and stays with the
        // terminal so coding agents inside the PTY keep working.
        assert!(is_app_level_shortcut("g", &ctrl_shift()));
        assert!(!is_app_level_shortcut("g", &ctrl()));
    }

    #[test]
    fn ctrl_shift_r_bubbles_for_open_pr() {
        // Ctrl+Shift+R opens the active tab's PR URL in the browser.
        // Plain Ctrl+R is readline reverse-history-search and is
        // heavily used inside agent CLIs — must stay with the
        // terminal.
        assert!(is_app_level_shortcut("r", &ctrl_shift()));
        assert!(!is_app_level_shortcut("r", &ctrl()));
    }
}

