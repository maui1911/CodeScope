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
    /// list of [`StyledRun`]s with already-resolved gpui colours and
    /// flags so the View layer doesn't need to know anything about
    /// alacritty's `Color` / `NamedColor` enums.
    ///
    /// Cells are walked via `display_iter`, so this respects scrollback
    /// scroll position the moment we wire the View up to drive
    /// `display_offset`. Adjacent cells with identical fg/bg/flags get
    /// merged into one run to keep the gpui element count down.
    ///
    /// The cursor cell is rendered with fg/bg swapped — good enough for
    /// the first iteration; a real block cursor with blink will land
    /// once we have an Element-level paint path.
    pub fn snapshot(&self, palette: &ColorPalette) -> TerminalSnapshot {
        self.with_term(|term| {
            let columns = term.columns();
            let screen_lines = term.screen_lines();
            let mut lines: Vec<Vec<StyledRun>> = vec![Vec::new(); screen_lines];

            let content = term.renderable_content();
            let display_offset = content.display_offset as i32;
            // Cursor position is reported in absolute grid coordinates;
            // translate to the visible row so the cursor cell lines up
            // with what the View actually paints.
            let cursor_line = content.cursor.point.line.0 + display_offset;
            let cursor_column = content.cursor.point.column.0;
            let mode = content.mode;
            // TUIs like claude-code, vim, etc. hide alacritty's logical
            // cursor and draw their own block character. Honour both
            // signals (TermMode::SHOW_CURSOR + CursorShape::Hidden) so
            // we don't double-render.
            let cursor_visible = mode.contains(TermMode::SHOW_CURSOR)
                && content.cursor.shape
                    != alacritty_terminal::vte::ansi::CursorShape::Hidden;
            let default_bg = palette.resolve(
                alacritty_terminal::vte::ansi::Color::Named(
                    alacritty_terminal::vte::ansi::NamedColor::Background,
                ),
                content.colors,
            );

            for indexed in content.display_iter {
                // Display iterator emits cells in absolute-grid line
                // coords (negative for scrollback). Translate to a
                // visible-row index 0..screen_lines.
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
                let is_cursor =
                    cursor_visible && row == cursor_line && col == cursor_column;
                let (fg, bg) = if is_cursor || selected { (bg, fg) } else { (fg, bg) };

                let line = &mut lines[row as usize];
                let next_col = line.iter().map(|r| r.text.chars().count()).sum::<usize>();

                // Pad missing columns (cells that display_iter skips,
                // e.g. wide-char spacers) with spaces in the default bg.
                if next_col < col {
                    let pad = col - next_col;
                    if let Some(last) = line.last_mut()
                        && last.fg == fg_default(palette)
                        && last.bg == default_bg
                        && !last.bold
                        && !last.italic
                        && !last.underline
                        && !is_cursor
                    {
                        for _ in 0..pad {
                            last.text.push(' ');
                        }
                    } else {
                        line.push(StyledRun {
                            text: " ".repeat(pad),
                            fg: fg_default(palette),
                            bg: default_bg,
                            bold: false,
                            italic: false,
                            underline: false,
                        });
                    }
                }

                let mergeable = line.last().is_some_and(|last| {
                    !is_cursor
                        && last.fg == fg
                        && last.bg == bg
                        && last.bold == bold
                        && last.italic == italic
                        && last.underline == underline
                });
                if mergeable {
                    line.last_mut().unwrap().text.push(cell.c);
                } else {
                    line.push(StyledRun {
                        text: cell.c.to_string(),
                        fg,
                        bg,
                        bold,
                        italic,
                        underline,
                    });
                }
            }

            // Empty rows still need a non-zero height; an empty `div` in
            // gpui collapses to 0 lines tall. A single space in the
            // default bg gives us a placeholder character to anchor the
            // line height.
            for line in lines.iter_mut() {
                if line.is_empty() {
                    line.push(StyledRun {
                        text: " ".to_string(),
                        fg: fg_default(palette),
                        bg: default_bg,
                        bold: false,
                        italic: false,
                        underline: false,
                    });
                }
            }

            TerminalSnapshot {
                lines,
                cursor_line,
                cursor_column,
                mode,
                columns,
                screen_lines,
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

fn fg_default(palette: &ColorPalette) -> Hsla {
    palette.foreground
}

/// One contiguous run of cells with identical styling on a single row.
#[derive(Debug, Clone)]
pub struct StyledRun {
    pub text: String,
    pub fg: Hsla,
    pub bg: Hsla,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
}

/// One frame's worth of grid contents, prepared for the gpui View. Lines
/// are indexed by visible row (0-based, top-down).
#[derive(Debug, Clone)]
pub struct TerminalSnapshot {
    pub lines: Vec<Vec<StyledRun>>,
    pub cursor_line: i32,
    pub cursor_column: usize,
    pub mode: TermMode,
    pub columns: usize,
    pub screen_lines: usize,
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
