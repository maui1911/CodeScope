//! PTY + alacritty `Term` driver.
//!
//! Owns the child-process lifecycle and pumps bytes through alacritty's
//! VTE parser. Exposes a writer for keyboard input, a resize hook, and a
//! locked accessor for the live `Term` so the View layer can render
//! consistent snapshots.
//!
//! The conhost scroll-offset tracking that motivated this crate's
//! existence (see `vendor/gpui-terminal/CODESCOPE-PATCHES.md`) is *not*
//! in this revision — it lands once we have a working render path to
//! verify the offset against. For now we mirror Zed's three-layer
//! architecture and stay drop-in compatible with raw alacritty
//! coordinates.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::thread::JoinHandle;

use alacritty_terminal::event::{Notify, OnResize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg, Notifier, State};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::CursorShape;
use alacritty_terminal::tty::{self, Options as TtyOptions, Pty, Shell};
use anyhow::{Context, Result};
use gpui::Hsla;

use crate::colors::ColorPalette;
use crate::event::{BackendEvent, EventProxy};

/// Dimensions plus per-cell pixel size, packaged the way alacritty wants
/// it. We keep our own `WindowSize` re-export so callers don't need to
/// pull alacritty into their crate graph.
pub use alacritty_terminal::event::WindowSize as TerminalSize;

/// Configuration for spawning a shell inside a fresh terminal.
#[derive(Debug, Clone)]
pub struct SpawnConfig {
    /// Program + arguments. `None` lets alacritty pick the platform
    /// default (`%COMSPEC%` on Windows, `$SHELL` on Unix).
    pub shell: Option<Shell>,
    /// Initial working directory for the child.
    pub working_directory: Option<PathBuf>,
    /// Extra environment variables merged on top of the parent process's.
    pub env: HashMap<String, String>,
    /// Initial geometry. Cell metrics may be 0 if the View hasn't laid
    /// out yet — alacritty only uses them for `WindowSize` reports.
    pub size: TerminalSize,
    /// On Windows, escape command-line arguments per CRT rules. Set
    /// `false` only if you know the child does its own argv parsing.
    #[cfg(target_os = "windows")]
    pub escape_args: bool,
}

impl Default for SpawnConfig {
    fn default() -> Self {
        Self {
            shell: None,
            working_directory: None,
            env: HashMap::new(),
            size: TerminalSize {
                num_lines: 24,
                num_cols: 80,
                cell_width: 0,
                cell_height: 0,
            },
            #[cfg(target_os = "windows")]
            escape_args: true,
        }
    }
}

/// Minimal `Dimensions` impl for handing geometry to `Term::new` and
/// `Term::resize`. Alacritty exposes a `TermSize` only under its `test`
/// module, so we roll our own here to keep the production surface clean.
#[derive(Copy, Clone, Debug)]
struct GridSize {
    columns: usize,
    screen_lines: usize,
}

impl GridSize {
    fn from_window(window: TerminalSize) -> Self {
        Self {
            columns: window.num_cols as usize,
            screen_lines: window.num_lines as usize,
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Owns the PTY, alacritty `Term`, and event-loop worker thread.
///
/// Cloning is intentionally not provided: each Backend is single-owner,
/// and the View layer holds it inside a gpui Entity. Use [`Backend::events`]
/// to fan events out where multiple subscribers are needed.
pub struct Backend {
    terminal: Arc<FairMutex<Term<EventProxy>>>,
    notifier: Notifier,
    sender: EventLoopSender,
    join: Option<JoinHandle<(EventLoop<Pty, EventProxy>, State)>>,
    events: flume::Receiver<BackendEvent>,
}

impl Backend {
    /// Spawn a child process inside a fresh PTY and start the event loop.
    pub fn spawn(config: SpawnConfig) -> Result<Self> {
        let SpawnConfig {
            shell,
            working_directory,
            env,
            size,
            #[cfg(target_os = "windows")]
            escape_args,
        } = config;

        let mut tty_options = TtyOptions {
            shell,
            working_directory,
            drain_on_exit: false,
            env,
            #[cfg(target_os = "windows")]
            escape_args,
        };
        // Silence the unused-mut warning on non-Windows builds where
        // every field is set in the struct literal above.
        let _ = &mut tty_options;

        let pty = tty::new(&tty_options, size, /* window_id */ 0)
            .context("failed to spawn PTY child process")?;

        let (proxy, events) = EventProxy::new();

        let term_config = Config {
            scrolling_history: 10_000,
            ..Config::default()
        };
        let term = Term::new(term_config, &GridSize::from_window(size), proxy.clone());
        let terminal = Arc::new(FairMutex::new(term));

        let event_loop = EventLoop::new(
            terminal.clone(),
            proxy.clone(),
            pty,
            /* drain_on_exit */ false,
            /* ref_test */ false,
        )
        .context("failed to construct alacritty EventLoop")?;

        let sender = event_loop.channel();
        proxy.install_pty_writer(sender.clone());
        let notifier = Notifier(sender.clone());

        let join = event_loop.spawn();

        Ok(Self {
            terminal,
            notifier,
            sender,
            join: Some(join),
            events,
        })
    }

    /// Queue input bytes for the child. The bytes are written from the
    /// event-loop thread, so this never blocks on I/O.
    pub fn write_input<B>(&self, bytes: B)
    where
        B: Into<std::borrow::Cow<'static, [u8]>>,
    {
        self.notifier.notify(bytes);
    }

    /// Notify the child of a new window size. Updates both the PTY's
    /// `winsize`/conhost console and the alacritty `Term`'s grid.
    pub fn resize(&mut self, size: TerminalSize) {
        self.notifier.on_resize(size);
        self.terminal.lock().resize(GridSize::from_window(size));
    }

    /// Run a closure with a read-only view of the terminal. The mutex is
    /// held for the duration of the call, so keep `f` short — anything
    /// that copies grid contents into a render buffer should batch its
    /// reads inside one `with_term` rather than calling repeatedly.
    pub fn with_term<R>(&self, f: impl FnOnce(&Term<EventProxy>) -> R) -> R {
        f(&self.terminal.lock())
    }

    /// Receiver for [`BackendEvent`]s emitted by the event loop. The View
    /// layer typically owns this and re-emits into gpui's notify channel.
    pub fn events(&self) -> flume::Receiver<BackendEvent> {
        self.events.clone()
    }

    /// Capture a styled snapshot of the visible grid. Each line is a
    /// list of [`StyledRun`]s with already-resolved gpui colours, flags,
    /// and exact column positions, so the renderer can paint quads and
    /// text at pixel-accurate offsets without re-walking the grid.
    ///
    /// Cells are walked via `display_iter`, so scrollback position is
    /// respected (the View drives `display_offset`). Adjacent cells with
    /// identical fg/bg/flags merge into one run; wide-char spacers are
    /// skipped so the wide cell carries its full visual width
    /// (`len_cols == 2`).
    ///
    /// Cursor info travels separately on the snapshot — the renderer
    /// paints the cursor as a quad on top of text, so we don't bake
    /// inverted colours into the run.
    pub fn snapshot(&self, palette: &ColorPalette) -> TerminalSnapshot {
        self.with_term(|term| {
            let columns = term.columns();
            let screen_lines = term.screen_lines();
            let mut lines: Vec<Vec<StyledRun>> = vec![Vec::new(); screen_lines];

            let content = term.renderable_content();
            let display_offset = content.display_offset as i32;
            let mode = content.mode;

            let default_fg = palette.resolve(
                alacritty_terminal::vte::ansi::Color::Named(
                    alacritty_terminal::vte::ansi::NamedColor::Foreground,
                ),
                content.colors,
            );
            let default_bg = palette.resolve(
                alacritty_terminal::vte::ansi::Color::Named(
                    alacritty_terminal::vte::ansi::NamedColor::Background,
                ),
                content.colors,
            );

            // Cursor info — translate the absolute-grid line to a
            // visible-row index. TUIs like claude-code, vim, etc. hide
            // alacritty's logical cursor and draw their own; honour both
            // signals (TermMode::SHOW_CURSOR + CursorShape::Hidden).
            let cursor_visible = mode.contains(TermMode::SHOW_CURSOR)
                && content.cursor.shape != CursorShape::Hidden;
            let cursor_row = content.cursor.point.line.0 + display_offset;
            let cursor_col = content.cursor.point.column.0;
            // Track the cursor cell's character + style so the renderer
            // can paint the right glyph on top of the cursor quad.
            let mut cursor_char: char = ' ';
            let mut cursor_cell_fg = default_fg;
            let mut cursor_cell_bg = default_bg;
            let mut cursor_cell_bold = false;
            let mut cursor_cell_italic = false;
            let mut cursor_cell_underline = false;

            for indexed in content.display_iter {
                let row = indexed.point.line.0 + display_offset;
                if row < 0 || (row as usize) >= screen_lines {
                    continue;
                }
                let col = indexed.point.column.0;
                if col >= columns {
                    continue;
                }

                let cell = indexed.cell;
                let flags = cell.flags;

                // Wide chars span 2 cells: keep the leading cell, skip
                // the trailing spacer so we don't double-paint.
                if flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let len_cols = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };

                let inverse = flags.contains(Flags::INVERSE);
                let mut fg = palette.resolve(cell.fg, content.colors);
                let mut bg = palette.resolve(cell.bg, content.colors);
                if inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let bold = flags.contains(Flags::BOLD);
                let italic = flags.contains(Flags::ITALIC);
                let underline = flags.contains(Flags::UNDERLINE);

                let selected = content
                    .selection
                    .as_ref()
                    .is_some_and(|range| range.contains(indexed.point));
                // Selection inverts the styling so it's visible against
                // any background. We don't bake the cursor's inversion
                // here — it's painted on top by the renderer.
                let (fg, bg) = if selected { (bg, fg) } else { (fg, bg) };

                let is_cursor_cell = cursor_visible
                    && row == cursor_row
                    && col == cursor_col;
                if is_cursor_cell {
                    cursor_char = cell.c;
                    cursor_cell_fg = fg;
                    cursor_cell_bg = bg;
                    cursor_cell_bold = bold;
                    cursor_cell_italic = italic;
                    cursor_cell_underline = underline;
                }

                let line = &mut lines[row as usize];
                let mergeable = line.last().is_some_and(|last| {
                    last.start_col + last.len_cols == col
                        && last.fg == fg
                        && last.bg == bg
                        && last.bold == bold
                        && last.italic == italic
                        && last.underline == underline
                });
                if mergeable {
                    let last = line.last_mut().unwrap();
                    last.text.push(cell.c);
                    last.len_cols += len_cols;
                } else {
                    line.push(StyledRun {
                        text: cell.c.to_string(),
                        start_col: col,
                        len_cols,
                        fg,
                        bg,
                        bold,
                        italic,
                        underline,
                    });
                }
            }

            // Cursor info: build now that we've captured the cell under
            // it. For block-style cursors, the renderer inverts colours
            // (cell.bg → text fg, palette.cursor → cursor bg). For beam
            // / underline shapes, only the bar uses cursor colour and
            // the cell text stays unchanged.
            let cursor = if cursor_visible
                && cursor_row >= 0
                && (cursor_row as usize) < screen_lines
                && cursor_col < columns
            {
                let cursor_color = palette.resolve(
                    alacritty_terminal::vte::ansi::Color::Named(
                        alacritty_terminal::vte::ansi::NamedColor::Cursor,
                    ),
                    content.colors,
                );
                Some(CursorInfo {
                    row: cursor_row,
                    col: cursor_col,
                    shape: content.cursor.shape,
                    cursor_color,
                    cell_fg: cursor_cell_fg,
                    cell_bg: cursor_cell_bg,
                    character: cursor_char,
                    bold: cursor_cell_bold,
                    italic: cursor_cell_italic,
                    underline: cursor_cell_underline,
                })
            } else {
                None
            };

            TerminalSnapshot {
                lines,
                cursor,
                mode,
                columns,
                screen_lines,
                default_fg,
                default_bg,
            }
        })
    }

    /// Current terminal mode (used by the keystroke encoder for
    /// APP_CURSOR-aware arrow keys).
    pub fn mode(&self) -> TermMode {
        self.with_term(|term| *term.mode())
    }

    /// Scroll the visible region by `delta` lines: positive scrolls
    /// *up* into history, negative scrolls back down toward the active
    /// region. Alacritty emits a `MouseCursorDirty` event afterward
    /// which our drain loop turns into a re-snapshot, so the View
    /// repaints automatically.
    pub fn scroll(&self, delta: i32) {
        self.terminal.lock().scroll_display(Scroll::Delta(delta));
    }

    /// Snap the visible region back to the active grid (line 0 of the
    /// scrollback / `display_offset = 0`). Called whenever the user
    /// types so input stays at the cursor instead of in history.
    pub fn reset_scroll(&self) {
        self.terminal.lock().scroll_display(Scroll::Bottom);
    }

    /// Page-sized scroll, useful for `PageUp`/`PageDown` keybindings.
    pub fn scroll_page_up(&self) {
        self.terminal.lock().scroll_display(Scroll::PageUp);
    }

    pub fn scroll_page_down(&self) {
        self.terminal.lock().scroll_display(Scroll::PageDown);
    }

    /// Current `display_offset` (number of rows scrolled into history).
    /// The View needs this to translate pixel coordinates to absolute
    /// grid points when starting / extending a selection.
    pub fn display_offset(&self) -> usize {
        self.terminal.lock().grid().display_offset()
    }

    /// Begin a fresh `Simple`-mode selection anchored at `point`.
    /// Replaces any prior selection.
    pub fn start_selection(&self, line: i32, column: usize) {
        let point = Point::new(Line(line), Column(column));
        let mut term = self.terminal.lock();
        term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
    }

    /// Extend the active selection's far end to `point`. No-op if there
    /// is no selection in flight.
    pub fn extend_selection(&self, line: i32, column: usize) {
        let point = Point::new(Line(line), Column(column));
        let mut term = self.terminal.lock();
        if let Some(sel) = term.selection.as_mut() {
            sel.update(point, Side::Right);
        }
    }

    /// Clear any active selection.
    pub fn clear_selection(&self) {
        self.terminal.lock().selection = None;
    }

    /// Materialise the current selection as a `String`. Returns `None`
    /// when there is no selection or it covers no cells.
    pub fn selection_text(&self) -> Option<String> {
        self.terminal.lock().selection_to_string()
    }
}

/// One contiguous run of cells with identical styling on a single row.
/// `start_col` is the leftmost column the run occupies; `len_cols` is
/// the cell width (each char contributes 1, wide chars contribute 2).
/// The renderer uses these to position background quads and shaped text
/// at exact pixel offsets — `text.chars().count()` is *not* a substitute
/// because of wide chars.
#[derive(Debug, Clone)]
pub struct StyledRun {
    pub text: String,
    pub start_col: usize,
    pub len_cols: usize,
    pub fg: Hsla,
    pub bg: Hsla,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// Cursor location, shape, and the colour state it should be painted
/// against. The renderer paints the cursor as a quad on top of the
/// styled runs, so the run text under the cursor stays untouched.
#[derive(Debug, Clone)]
pub struct CursorInfo {
    /// Visible row (0-based, top of viewport).
    pub row: i32,
    /// Column index.
    pub col: usize,
    pub shape: CursorShape,
    /// Cursor colour from the palette — block fill, beam bar, or
    /// underline bar all use this.
    pub cursor_color: Hsla,
    /// Foreground colour the cell would otherwise paint at.
    pub cell_fg: Hsla,
    /// Background colour of the cell — used as the text colour for a
    /// block cursor so the glyph stays readable against the inverted
    /// fill.
    pub cell_bg: Hsla,
    /// Character occupying the cursor cell. Painted unchanged for beam
    /// / underline cursors; redrawn with `cell_bg` for block cursors so
    /// it remains readable on top of the inverted fill.
    pub character: char,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// One frame's worth of grid contents, prepared for the gpui View. Lines
/// are indexed by visible row (0-based, top-down).
#[derive(Debug, Clone)]
pub struct TerminalSnapshot {
    pub lines: Vec<Vec<StyledRun>>,
    pub cursor: Option<CursorInfo>,
    pub mode: TermMode,
    pub columns: usize,
    pub screen_lines: usize,
    pub default_fg: Hsla,
    pub default_bg: Hsla,
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Ask the event loop to stop; ignore the result because the
        // receiver may already be gone if the child exited on its own.
        let _ = self.sender.send(Msg::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}
