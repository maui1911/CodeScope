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
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::tty::{self, Options as TtyOptions, Pty, Shell};
use anyhow::{Context, Result};

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

        let term = Term::new(Config::default(), &GridSize::from_window(size), proxy.clone());
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
