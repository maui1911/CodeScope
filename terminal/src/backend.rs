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
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use parking_lot::Mutex;

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
    /// Optional palette override. `None` falls back to
    /// [`ColorPalette::default`] (VS Code dark). The palette is shared
    /// with the [`EventProxy`] so OSC 4 / 10-12 colour queries answer
    /// against the theme the user actually sees.
    pub palette: Option<ColorPalette>,
    /// Maximum scrollback line count.
    pub scrollback: usize,
    /// Default cursor style + blink state. TUIs that emit DECSCUSR
    /// override this on the fly.
    pub default_cursor_style: CursorStylePreset,
    /// On Windows, escape command-line arguments per CRT rules. Set
    /// `false` only if you know the child does its own argv parsing.
    #[cfg(target_os = "windows")]
    pub escape_args: bool,
}

/// Cursor shape + blink that the terminal starts with — what shells
/// without DECSCUSR (PSReadLine, plain bash) inherit. Mirrors
/// [`alacritty_terminal::vte::ansi::CursorShape`] in a Copy-cheap form
/// the [`SpawnConfig`] caller can hand around. Not directly serde —
/// the app layer parses the string from `settings.json` (`"block"`,
/// `"beam"`, …) and constructs this struct, keeping alacritty's
/// `CursorShape` enum out of the on-disk surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorStylePreset {
    pub shape: CursorShape,
    pub blinking: bool,
}

impl Default for CursorStylePreset {
    fn default() -> Self {
        // Match Windows Terminal's out-of-the-box feel.
        Self { shape: CursorShape::Beam, blinking: true }
    }
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
            palette: None,
            scrollback: 10_000,
            default_cursor_style: CursorStylePreset::default(),
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
    /// Kept around so resize calls can update the cached size the
    /// proxy uses to answer text-area-size queries.
    proxy: EventProxy,
    /// Directory the child was spawned in — the anchor for resolving
    /// relative file paths during link detection in [`Backend::snapshot`].
    working_directory: Option<PathBuf>,
    /// Memoised `Path::exists` answers for file-path link detection.
    /// Snapshots run on every PTY wakeup, so without this each frame
    /// would re-stat every path-looking token on screen. Bounded by
    /// [`PATH_EXISTS_CACHE_CAP`]; cleared wholesale on overflow. The
    /// trade-off: a file created *after* its path scrolled on screen
    /// stays non-clickable until the cache turns over — acceptable for
    /// a hover affordance.
    path_exists_cache: Mutex<HashMap<PathBuf, bool>>,
}

impl Backend {
    /// Spawn a child process inside a fresh PTY and start the event loop.
    pub fn spawn(config: SpawnConfig) -> Result<Self> {
        let SpawnConfig {
            shell,
            working_directory,
            env,
            size,
            palette,
            scrollback,
            default_cursor_style,
            #[cfg(target_os = "windows")]
            escape_args,
        } = config;

        // Fall back to the host process's cwd when the caller didn't
        // pin one — that's where the PTY child actually starts in
        // that case (alacritty's tty layer inherits the parent cwd),
        // so relative-path link resolution keeps working for default
        // spawns (e.g. examples) too.
        let working_directory_for_links = working_directory
            .clone()
            .or_else(|| std::env::current_dir().ok());
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

        // Adopt the freshly-spawned child into the process-wide job
        // object so it (and every grandchild — claude/codex/etc.) is
        // killed when CodeScope exits, even on a hard crash. Mirrors
        // `ProcessTreeKiller.Adopt` in the C# build. No-op on non-
        // Windows targets.
        #[cfg(windows)]
        {
            // SAFETY: `child_watcher().raw_handle()` returns a valid
            // process handle owned by alacritty for the lifetime of
            // the `Pty`. We only borrow it long enough to call
            // `AssignProcessToJobObject`, which copies what it needs.
            let raw = pty.child_watcher().raw_handle();
            let handle = windows::Win32::Foundation::HANDLE(raw as *mut _);
            if let Err(err) = crate::process_group::adopt_handle(handle) {
                // Non-fatal: a failure here means orphaned children
                // are *possible* on hard crash, but normal operation
                // is unaffected. Log loudly so a regression is
                // noticeable in the dev console.
                eprintln!("process_group: failed to adopt pty child: {err:#}");
            }
        }

        // EventProxy answers colour / text-area-size queries directly
        // on the event-loop thread, so it needs a palette to resolve
        // against. We hand it a clone of whatever the caller chose
        // (typically derived from the active theme) so OSC 4 / 10-12
        // responses match what the user actually sees.
        let palette = palette.unwrap_or_default();
        let (proxy, events) = EventProxy::new(palette, size);

        let cursor_style = alacritty_terminal::vte::ansi::CursorStyle {
            shape: default_cursor_style.shape,
            blinking: default_cursor_style.blinking,
        };
        let term_config = Config {
            scrolling_history: scrollback,
            default_cursor_style: cursor_style,
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
            proxy,
            working_directory: working_directory_for_links,
            path_exists_cache: Mutex::new(HashMap::new()),
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
        self.proxy.update_size(size);
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

    /// Push a new palette into the event proxy so OSC 4 / 10-12 colour
    /// queries answer against the live theme instead of the spawn-time
    /// one. Pairs with re-snapshotting on the View side — both halves
    /// together make a theme switch fully take over a running terminal.
    pub fn update_palette(&self, palette: &ColorPalette) {
        self.proxy.update_palette(palette.clone());
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
        let mut snapshot = self.with_term(|term| {
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
            // Blink-bit lives on `cursor_style()` rather than the
            // RenderableContent cursor — the latter only carries the
            // shape. The shape itself can disagree with cursor_style if
            // the TUI sent DECSCUSR mid-frame, so we trust each source
            // for its own field.
            let cursor_blinking = term.cursor_style().blinking;
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
                // SGR 2 (faint) dims the *foreground* only — never the
                // background, same as xterm / alacritty. claude-code
                // leans on it for hints and shortcut legends, so
                // dropping the flag rendered that text at full
                // strength and lost the visual hierarchy.
                let mut fg = if flags.contains(Flags::DIM) {
                    palette.resolve_faint(cell.fg, content.colors)
                } else {
                    palette.resolve(cell.fg, content.colors)
                };
                let mut bg = palette.resolve(cell.bg, content.colors);
                if inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                let bold = flags.contains(Flags::BOLD);
                let italic = flags.contains(Flags::ITALIC);
                // Hyperlinks (OSC 8) always render with an underline,
                // regardless of whether the cell explicitly carries
                // `Flags::UNDERLINE`. Saves the user from emitting
                // both — and matches every other terminal's hover
                // affordance for clickable text.
                let hyperlink: Option<Arc<str>> =
                    cell.hyperlink().map(|h| Arc::from(h.uri()));
                let underline = flags.contains(Flags::UNDERLINE) || hyperlink.is_some();

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
                        // Don't merge across hyperlinks — adjacent
                        // links to different URIs would otherwise
                        // collapse, and we'd lose the per-run uri
                        // we use for click handling.
                        && last.hyperlink == hyperlink
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
                        hyperlink,
                    });
                }
            }

            // Sweep each row for plain-text URLs (claude-code,
            // `gh pr view`, `cargo --message-format json`, … emit
            // them without OSC 8) and turn them into clickable runs.
            // Existing OSC 8 hyperlinks are left alone; URL detection
            // never overrides an explicit one.
            inject_url_hyperlinks(&mut lines);

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
                    blinking: cursor_blinking,
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
        });

        // File-path link detection runs *outside* `with_term` — a
        // cache miss costs a filesystem stat, and the event-loop
        // thread must not wait on disk I/O to feed the parser.
        {
            let mut cache = self.path_exists_cache.lock();
            let mut exists = |path: &Path| {
                if let Some(&hit) = cache.get(path) {
                    return hit;
                }
                let result = path.exists();
                // Crude but sufficient eviction: visible screens
                // repeat the same handful of paths frame after frame,
                // so a full clear on overflow still keeps the steady-
                // state hit rate near 100 %.
                if cache.len() >= PATH_EXISTS_CACHE_CAP {
                    cache.clear();
                }
                cache.insert(path.to_path_buf(), result);
                result
            };
            inject_file_path_hyperlinks(
                &mut snapshot.lines,
                self.working_directory.as_deref(),
                &mut exists,
            );
        }

        snapshot
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

    /// Look up the OSC 8 hyperlink at a visible-row / column position.
    /// Used by the View to decide whether a click should open a URL.
    /// Returns the resolved URI, or `None` when the cell isn't a link
    /// (or coordinates are outside the grid).
    pub fn hyperlink_at(&self, visible_row: usize, col: usize) -> Option<String> {
        self.with_term(|term| {
            let display_offset = term.grid().display_offset() as i32;
            // Translate viewport row → absolute grid line. With
            // `display_offset = 0` (no scrollback) row 0 = line 0;
            // when scrolled, row 0 sits at line `-display_offset`.
            let line = visible_row as i32 - display_offset;
            let point = Point::new(Line(line), Column(col));
            let cell = &term.grid()[point];
            cell.hyperlink().map(|h| h.uri().to_string())
        })
    }
}

/// Post-processing pass that finds plain-text URLs (`https://…`,
/// `http://…`, `file://…`, …) inside each line of styled runs and
/// retro-tags the matching cells with a hyperlink, just like an OSC 8
/// link would have done. Lets `claude-code` / `gh` / `cargo` output
/// stay clickable even though they emit URLs as bare text.
///
/// Runs that already carry an OSC 8 hyperlink are left untouched —
/// the explicit signal always wins. Runs containing wide chars (where
/// `len_cols != chars().count()`) are skipped to keep splitting safe;
/// real-world URLs are pure ASCII so this is rarely a constraint.
fn inject_url_hyperlinks(lines: &mut [Vec<StyledRun>]) {
    let finder = linkify::LinkFinder::new();
    for line in lines.iter_mut() {
        if line.is_empty() {
            continue;
        }
        let text = line_text(line);
        let urls: Vec<(usize, usize, Arc<str>)> = finder
            .links(&text)
            .filter(|link| link.kind() == &linkify::LinkKind::Url)
            .map(|link| (link.start(), link.end(), Arc::from(link.as_str())))
            .collect();
        for (b_start, b_end, url) in urls {
            let col_start = byte_to_col(&text, b_start);
            let col_end = byte_to_col(&text, b_end);
            apply_url_to_line(line, col_start, col_end, url);
        }
    }
}

/// Rebuild a row's column-aligned plain text from its styled runs.
/// Gaps between runs are padded with spaces so byte/char positions in
/// the result line up with screen columns (modulo wide chars — see
/// `apply_url_to_line`'s skip rule).
///
/// `current_col` is tracked alongside the string so padding is
/// O(total_chars) instead of O(chars²) — `chars().count()` in a
/// while-loop rescans the whole string per padded space, which would
/// dominate snapshot time on long lines.
fn line_text(line: &[StyledRun]) -> String {
    let mut text = String::new();
    let mut current_col: usize = 0;
    for run in line.iter() {
        while current_col < run.start_col {
            text.push(' ');
            current_col += 1;
        }
        text.push_str(&run.text);
        current_col += run.text.chars().count();
    }
    text
}

/// Translate a byte index inside reconstructed line text to a screen
/// column. For ASCII prefixes (URLs and the vast majority of paths)
/// byte == char == column, so the common path stays O(1); the
/// `chars().count()` fallback handles non-ASCII content.
fn byte_to_col(text: &str, byte: usize) -> usize {
    if text.is_char_boundary(byte) && text[..byte].is_ascii() {
        byte
    } else {
        text[..byte].chars().count()
    }
}

/// Upper bound on [`Backend::path_exists_cache`]. 4096 distinct paths
/// is far beyond what a visible screen plus scroll churn produces
/// between clears, while capping worst-case memory at well under a
/// megabyte.
const PATH_EXISTS_CACHE_CAP: usize = 4096;

/// A candidate file-path span inside one line of reconstructed text.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PathCandidate {
    /// Byte offset where the clickable span starts.
    start: usize,
    /// Byte offset one past the clickable span. Includes a trailing
    /// `:line` / `:line:col` suffix when present — visually the suffix
    /// reads as part of the link, even though the opener ignores it.
    end: usize,
    /// The bare path text: wrapping punctuation and any `:line:col`
    /// suffix stripped.
    path: String,
}

/// Scan one line of text for tokens that look like file paths.
///
/// Deliberately conservative: whitespace-delimited tokens only (paths
/// with spaces are undetectable without quoting context), wrapping
/// punctuation peeled, URLs and CLI flags rejected. A token qualifies
/// when it contains a path separator or looks like `stem.ext`. Whether
/// the path actually exists is the caller's job — shape alone would
/// produce far too many false positives (`and/or`, version strings).
fn file_path_candidates(text: &str) -> Vec<PathCandidate> {
    const LEADING_TRIM: &[char] = &['(', '[', '{', '<', '"', '\'', '`'];
    const TRAILING_TRIM: &[char] = &[')', ']', '}', '>', '"', '\'', '`', '.', ',', ';', ':', '!', '?'];
    /// Tokens longer than this are never paths worth stat-ing.
    const MAX_TOKEN_LEN: usize = 512;

    let mut out = Vec::new();
    let mut tokens: Vec<(usize, &str)> = Vec::new();
    let mut start: Option<usize> = None;
    for (i, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(s) = start.take() {
                tokens.push((s, &text[s..i]));
            }
        } else if start.is_none() {
            start = Some(i);
        }
    }
    if let Some(s) = start {
        tokens.push((s, &text[s..]));
    }

    for (tok_start, raw) in tokens {
        let mut span_start = tok_start;
        let mut tok = raw;
        while let Some(ch) = tok.chars().next() {
            if !LEADING_TRIM.contains(&ch) {
                break;
            }
            span_start += ch.len_utf8();
            tok = &tok[ch.len_utf8()..];
        }
        while let Some(ch) = tok.chars().last() {
            if !TRAILING_TRIM.contains(&ch) {
                break;
            }
            tok = &tok[..tok.len() - ch.len_utf8()];
        }
        if tok.len() < 2 || tok.len() > MAX_TOKEN_LEN {
            continue;
        }
        // URLs belong to `inject_url_hyperlinks`; flags are never paths.
        if tok.contains("://") || tok.starts_with('-') {
            continue;
        }
        // Strip up to two trailing `:digits` groups — the `path:line`
        // and `path:line:col` shapes every compiler/grepper emits.
        let mut path = tok;
        for _ in 0..2 {
            match path.rfind(':') {
                Some(idx)
                    if !path[idx + 1..].is_empty()
                        && path[idx + 1..].bytes().all(|b| b.is_ascii_digit()) =>
                {
                    path = &path[..idx];
                }
                _ => break,
            }
        }
        if !looks_like_path(path) {
            continue;
        }
        out.push(PathCandidate {
            start: span_start,
            end: span_start + tok.len(),
            path: path.to_string(),
        });
    }
    out
}

/// Shape gate for [`file_path_candidates`]: a separator anywhere, or a
/// bare `stem.ext` filename. The extension must contain a letter so
/// version strings (`v0.4.0`, `rc.14`) don't trigger a filesystem stat
/// for every numeric token on screen.
fn looks_like_path(token: &str) -> bool {
    if token.is_empty() || token.chars().all(|c| c == '/' || c == '\\' || c == '.') {
        return false;
    }
    if token.contains('/') || token.contains('\\') {
        return true;
    }
    match token.rsplit_once('.') {
        Some((stem, ext)) => {
            !stem.is_empty()
                && !ext.is_empty()
                && ext.len() <= 10
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && ext.chars().any(|c| c.is_ascii_alphabetic())
        }
        None => false,
    }
}

/// Resolve a detected path against the terminal's working directory.
/// Absolute paths pass through; relative ones need a base. Windows
/// drive-rooted-but-driveless shapes (`\foo`) resolve against the
/// base's drive via `Path::join`'s own semantics.
fn resolve_candidate(path: &str, base_dir: Option<&Path>) -> Option<PathBuf> {
    let p = Path::new(path);
    if p.is_absolute() {
        Some(p.to_path_buf())
    } else {
        base_dir.map(|base| base.join(p))
    }
}

/// Post-processing pass that finds file-path tokens (`src\app.rs:3644`,
/// `./scripts/build.ps1`, `C:\dev\foo\Cargo.toml`, …) in each line and
/// retro-tags the matching cells with a hyperlink carrying the resolved
/// absolute path, so Ctrl+click opens the file in the OS default
/// application — same affordance bare URLs already get.
///
/// Only paths that pass the `exists` probe become links; shape alone
/// would underline half of every build log. `exists` is injected (and
/// memoised by the caller) so tests stay hermetic and snapshots stay
/// cheap. Runs with an explicit OSC 8 hyperlink or a detected URL are
/// never overridden — this pass runs last and `apply_url_to_line`
/// skips already-linked runs.
fn inject_file_path_hyperlinks(
    lines: &mut [Vec<StyledRun>],
    base_dir: Option<&Path>,
    exists: &mut dyn FnMut(&Path) -> bool,
) {
    for line in lines.iter_mut() {
        if line.is_empty() {
            continue;
        }
        let text = line_text(line);
        for cand in file_path_candidates(&text) {
            let Some(resolved) = resolve_candidate(&cand.path, base_dir) else {
                continue;
            };
            if !exists(&resolved) {
                continue;
            }
            let col_start = byte_to_col(&text, cand.start);
            let col_end = byte_to_col(&text, cand.end);
            let uri: Arc<str> = Arc::from(resolved.to_string_lossy().as_ref());
            apply_url_to_line(line, col_start, col_end, uri);
        }
    }
}

fn apply_url_to_line(
    line: &mut Vec<StyledRun>,
    url_start: usize,
    url_end: usize,
    url: Arc<str>,
) {
    let mut new_runs: Vec<StyledRun> = Vec::with_capacity(line.len() + 2);
    for run in line.drain(..) {
        let r_start = run.start_col;
        let r_end = run.start_col + run.len_cols;
        // Run sits entirely outside the URL → keep as-is.
        // Run already has its own (OSC 8) hyperlink → don't override.
        // Run has wide chars → splitting on column boundaries gets
        // ambiguous, skip the URL injection here.
        if r_end <= url_start
            || r_start >= url_end
            || run.hyperlink.is_some()
            || run.text.chars().count() != run.len_cols
        {
            new_runs.push(run);
            continue;
        }
        let chars: Vec<char> = run.text.chars().collect();
        let split_left = url_start.saturating_sub(r_start);
        let split_right = (url_end - r_start).min(run.len_cols);

        if split_left > 0 {
            let pre: String = chars[..split_left].iter().collect();
            new_runs.push(StyledRun {
                text: pre,
                start_col: r_start,
                len_cols: split_left,
                fg: run.fg,
                bg: run.bg,
                bold: run.bold,
                italic: run.italic,
                underline: run.underline,
                hyperlink: None,
            });
        }
        let mid: String = chars[split_left..split_right].iter().collect();
        new_runs.push(StyledRun {
            text: mid,
            start_col: r_start + split_left,
            len_cols: split_right - split_left,
            fg: run.fg,
            bg: run.bg,
            bold: run.bold,
            italic: run.italic,
            underline: true,
            hyperlink: Some(url.clone()),
        });
        if split_right < run.len_cols {
            let post: String = chars[split_right..].iter().collect();
            new_runs.push(StyledRun {
                text: post,
                start_col: r_start + split_right,
                len_cols: run.len_cols - split_right,
                fg: run.fg,
                bg: run.bg,
                bold: run.bold,
                italic: run.italic,
                underline: run.underline,
                hyperlink: None,
            });
        }
    }
    *line = new_runs;
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
    /// OSC 8 hyperlink URI, if the cells in this run are clickable.
    /// Always paints underlined (the renderer treats `hyperlink.is_some()`
    /// as an implicit `underline = true`). Click-to-open is wired in
    /// `TerminalView::on_mouse_down`.
    pub hyperlink: Option<Arc<str>>,
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
    /// Whether the TUI requested a blinking cursor (`\x1b[1 q` /
    /// `\x1b[3 q` / `\x1b[5 q`). The renderer drives the actual blink
    /// timer and skips the cursor paint during the off phase.
    pub blinking: bool,
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

impl TerminalSnapshot {
    /// Find the OSC 8 / detected-URL hyperlink at a viewport-relative
    /// (row, col). Walks the row's runs for the one covering `col` —
    /// O(runs-per-row), tiny in practice. The View uses this for
    /// hover-cursor and Ctrl-click handling, so it doesn't have to
    /// re-lock the live `Term` just to look up a URL.
    pub fn hyperlink_at(&self, row: usize, col: usize) -> Option<Arc<str>> {
        let line = self.lines.get(row)?;
        line.iter()
            .find(|r| col >= r.start_col && col < r.start_col + r.len_cols)?
            .hyperlink
            .clone()
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        // Best-effort shutdown from Drop: both the channel and the
        // event-loop thread may already be torn down (e.g. child
        // exited on its own, or the loop panicked). We can't surface
        // anything from this path and we don't want to panic during
        // unwinding, so the send/join results are intentionally
        // swallowed.
        let _ = self.sender.send(Msg::Shutdown);
        if let Some(handle) = self.join.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain_run(text: &str, start_col: usize) -> StyledRun {
        StyledRun {
            text: text.to_string(),
            start_col,
            len_cols: text.chars().count(),
            fg: gpui::black(),
            bg: gpui::black(),
            bold: false,
            italic: false,
            underline: false,
            hyperlink: None,
        }
    }

    fn single_run_line(text: &str) -> Vec<StyledRun> {
        vec![plain_run(text, 0)]
    }

    fn candidate_paths(text: &str) -> Vec<String> {
        file_path_candidates(text)
            .into_iter()
            .map(|c| c.path)
            .collect()
    }

    #[test]
    fn candidates_detects_separator_and_bare_filename_tokens() {
        assert_eq!(
            candidate_paths("edit src/main.rs and Cargo.toml please"),
            vec!["src/main.rs".to_string(), "Cargo.toml".to_string()]
        );
        assert_eq!(
            candidate_paths(r"error in C:\dev\foo\bar.txt today"),
            vec![r"C:\dev\foo\bar.txt".to_string()]
        );
    }

    #[test]
    fn candidates_strips_line_col_suffix_but_keeps_it_in_the_span() {
        let text = r"  --> src\app.rs:3644:9";
        let cands = file_path_candidates(text);
        assert_eq!(cands.len(), 1);
        let cand = &cands[0];
        assert_eq!(cand.path, r"src\app.rs");
        assert_eq!(&text[cand.start..cand.end], r"src\app.rs:3644:9");
    }

    #[test]
    fn candidates_peels_wrapping_punctuation() {
        let text = r#"see (src/sidebar.rs), "docs/DESIGN.md" and `core/paths.rs`."#;
        assert_eq!(
            candidate_paths(text),
            vec![
                "src/sidebar.rs".to_string(),
                "docs/DESIGN.md".to_string(),
                "core/paths.rs".to_string(),
            ]
        );
        // Spans must not include the wrapping punctuation.
        let first = &file_path_candidates(text)[0];
        assert_eq!(&text[first.start..first.end], "src/sidebar.rs");
    }

    #[test]
    fn candidates_rejects_urls_flags_versions_and_separator_noise() {
        assert!(candidate_paths("https://example.com/a/b.html").is_empty());
        assert!(candidate_paths("--manifest-path foo").is_empty());
        assert!(candidate_paths("v0.4.0 and rc.14 and 1.2.3").is_empty());
        assert!(candidate_paths("/ \\ . .. ./ ...").is_empty());
    }

    #[test]
    fn looks_like_path_requires_a_letter_in_the_extension() {
        assert!(looks_like_path("main.rs"));
        assert!(looks_like_path("archive.7z"));
        assert!(looks_like_path("a/b"));
        assert!(!looks_like_path("v0.4.0"));
        assert!(!looks_like_path("readme"));
        assert!(!looks_like_path(".gitignore-ish.")); // trailing dot → empty ext
    }

    #[test]
    fn resolve_candidate_joins_relative_against_base_and_passes_absolute() {
        let base = Path::new(r"C:\repo");
        assert_eq!(
            resolve_candidate("src/main.rs", Some(base)),
            Some(base.join("src/main.rs"))
        );
        // Relative paths without a base are unresolvable.
        assert_eq!(resolve_candidate("src/main.rs", None), None);
        #[cfg(windows)]
        assert_eq!(
            resolve_candidate(r"C:\other\x.rs", Some(base)),
            Some(PathBuf::from(r"C:\other\x.rs"))
        );
    }

    #[test]
    fn inject_marks_existing_paths_and_leaves_missing_ones_alone() {
        let base = Path::new(r"C:\repo");
        let mut lines = vec![
            single_run_line("edit src/main.rs now"),
            single_run_line("ghost src/missing.rs here"),
        ];
        let expected = base.join("src/main.rs");
        let mut exists = |p: &Path| p == expected;
        inject_file_path_hyperlinks(&mut lines, Some(base), &mut exists);

        // Existing path: run split in three, middle one clickable.
        let linked: Vec<_> = lines[0]
            .iter()
            .filter(|r| r.hyperlink.is_some())
            .collect();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].text, "src/main.rs");
        assert!(linked[0].underline);
        assert_eq!(
            linked[0].hyperlink.as_deref(),
            Some(expected.to_string_lossy().as_ref())
        );

        // Missing path: untouched single run.
        assert_eq!(lines[1].len(), 1);
        assert!(lines[1][0].hyperlink.is_none());
    }

    #[test]
    fn inject_clickable_span_includes_line_col_suffix() {
        let base = Path::new(r"C:\repo");
        let mut lines = vec![single_run_line(r"  --> src\app.rs:3644:9")];
        let mut exists = |_: &Path| true;
        inject_file_path_hyperlinks(&mut lines, Some(base), &mut exists);

        let linked: Vec<_> = lines[0]
            .iter()
            .filter(|r| r.hyperlink.is_some())
            .collect();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].text, r"src\app.rs:3644:9");
        // The link target is the resolved path *without* the suffix.
        assert_eq!(
            linked[0].hyperlink.as_deref(),
            Some(base.join(r"src\app.rs").to_string_lossy().as_ref())
        );
    }

    #[test]
    fn inject_skips_relative_paths_without_a_working_directory() {
        let mut lines = vec![single_run_line("edit src/main.rs now")];
        let mut exists = |_: &Path| true;
        inject_file_path_hyperlinks(&mut lines, None, &mut exists);
        assert!(lines[0].iter().all(|r| r.hyperlink.is_none()));
    }

    #[test]
    fn inject_never_overrides_an_existing_hyperlink() {
        let base = Path::new(r"C:\repo");
        let mut run = plain_run("src/main.rs", 0);
        run.hyperlink = Some(Arc::from("https://example.com"));
        let mut lines = vec![vec![run]];
        let mut exists = |_: &Path| true;
        inject_file_path_hyperlinks(&mut lines, Some(base), &mut exists);
        assert_eq!(lines[0].len(), 1);
        assert_eq!(
            lines[0][0].hyperlink.as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn line_text_pads_gaps_between_runs_to_screen_columns() {
        let line = vec![plain_run("ab", 0), plain_run("cd", 5)];
        assert_eq!(line_text(&line), "ab   cd");
    }
}
