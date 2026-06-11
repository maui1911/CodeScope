//! Bridge between alacritty's `EventListener` and our consumers.
//!
//! Alacritty's terminal core calls `EventListener::send_event` from the
//! event-loop thread for everything that needs the embedder's attention:
//! redraw hints, title changes, bell, exits, and request/response payloads
//! that must be written back to the PTY (`Event::PtyWrite`).
//!
//! [`EventProxy`] forwards user-visible variants through a flume channel
//! and pumps `Event::PtyWrite` payloads back to the PTY through the
//! event loop's own input queue. Routing PTY responses through the
//! event-loop sender (instead of grabbing the PTY writer directly) keeps
//! all PTY writes serialised on a single thread, which matches alacritty's
//! own design.
//!
//! `Event::PtyWrite` cannot be dropped: shells like `pwsh` and `cmd` send
//! `ESC[6n` (Device Status Report) at startup and stall their prompt
//! until the response lands. The vendored `gpui-terminal` crate had this
//! exact bug — see `vendor/gpui-terminal/CODESCOPE-PATCHES.md`.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoopSender, Msg};
use flume::Sender;
use parking_lot::Mutex;

use crate::colors::ColorPalette;

/// Subset of alacritty events that consumers of this crate actually care
/// about. Mirrors `gpui-terminal`'s `TerminalEvent`, but keeps `ChildExit`
/// distinct from `Exit` so the View layer can differentiate "the shell
/// exited cleanly" from "alacritty asked us to tear the whole thing down".
#[derive(Clone)]
pub enum BackendEvent {
    /// New content rendered into the grid; the View should redraw.
    Wakeup,
    /// BEL received.
    Bell,
    /// Title changed via OSC 0 / OSC 2.
    Title(String),
    /// Title reset to the default.
    ResetTitle,
    /// Mouse cursor visibility/style hint changed.
    MouseCursorDirty,
    /// OSC 52 paste request — the View can decide whether to honour it.
    ClipboardLoad,
    /// OSC 52 copy request with the payload.
    ClipboardStore(String),
    /// Alacritty requested termination of the terminal session.
    Exit,
    /// The child process exited; payload is the OS exit code.
    ChildExit(i32),
}

impl std::fmt::Debug for BackendEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wakeup => write!(f, "Wakeup"),
            Self::Bell => write!(f, "Bell"),
            Self::Title(t) => f.debug_tuple("Title").field(t).finish(),
            Self::ResetTitle => write!(f, "ResetTitle"),
            Self::MouseCursorDirty => write!(f, "MouseCursorDirty"),
            Self::ClipboardLoad => write!(f, "ClipboardLoad"),
            Self::ClipboardStore(s) => f.debug_tuple("ClipboardStore").field(s).finish(),
            Self::Exit => write!(f, "Exit"),
            Self::ChildExit(c) => f.debug_tuple("ChildExit").field(c).finish(),
        }
    }
}

/// Cloneable handle suitable for `Term::new` and `EventLoop::new`. The
/// inner state is reference-counted so cloning the proxy is cheap.
#[derive(Clone)]
pub struct EventProxy {
    inner: Arc<Inner>,
}

struct Inner {
    tx: Sender<BackendEvent>,
    /// Filled in once the event loop is running. Used to forward
    /// `Event::PtyWrite` payloads back to the PTY without violating the
    /// `&self` receiver of `EventListener::send_event`.
    pty_writer: OnceLock<EventLoopSender>,
    /// Palette used to answer OSC colour queries on the spot. Some
    /// TUIs (claude-code, htop, etc.) probe terminal capabilities at
    /// startup and silently fall back to a stripped-down UI if they
    /// don't get a quick response. Sending the answer from the
    /// event-loop thread directly (instead of routing through gpui's
    /// async update tick on the View) keeps round-trip below a frame.
    /// Behind a mutex so a theme switch can swap it mid-session and
    /// colour queries keep matching what's painted on screen.
    palette: Mutex<ColorPalette>,
    /// Latest `WindowSize` the View pushed to the backend, used to
    /// answer `\x1b[14t` text-area-size queries instantly. The View
    /// updates this on every resize.
    size: Arc<Mutex<WindowSize>>,
}

impl EventProxy {
    /// Build a fresh proxy. The returned receiver yields one [`BackendEvent`]
    /// per relevant alacritty event, in arrival order.
    pub fn new(palette: ColorPalette, initial_size: WindowSize) -> (Self, flume::Receiver<BackendEvent>) {
        let (tx, rx) = flume::unbounded();
        let proxy = Self {
            inner: Arc::new(Inner {
                tx,
                pty_writer: OnceLock::new(),
                palette: Mutex::new(palette),
                size: Arc::new(Mutex::new(initial_size)),
            }),
        };
        (proxy, rx)
    }

    /// Wire up the event-loop sender so `Event::PtyWrite` payloads can be
    /// queued as `Msg::Input`. Calling this more than once is a no-op.
    pub fn install_pty_writer(&self, sender: EventLoopSender) {
        let _ = self.inner.pty_writer.set(sender);
    }

    /// Update the cached window size so subsequent `Event::TextAreaSizeRequest`
    /// answers reflect the latest layout. Cheap — just a mutex write.
    pub fn update_size(&self, size: WindowSize) {
        *self.inner.size.lock() = size;
    }

    /// Swap the palette used to answer OSC 4 / 10-12 colour queries so
    /// TUIs probing colours after a theme switch get values that match
    /// what's actually painted. Cheap — just a mutex write.
    pub fn update_palette(&self, palette: ColorPalette) {
        *self.inner.palette.lock() = palette;
    }

    fn forward(&self, event: BackendEvent) {
        // A disconnected receiver just means the consumer dropped first;
        // the event loop will keep going until shutdown is requested.
        let _ = self.inner.tx.send(event);
    }

    fn write_pty(&self, payload: String) {
        if payload.is_empty() {
            return;
        }
        if let Some(sender) = self.inner.pty_writer.get() {
            let _ = sender.send(Msg::Input(Cow::Owned(payload.into_bytes())));
        }
    }
}

impl EventListener for EventProxy {
    fn send_event(&self, event: Event) {
        match event {
            Event::PtyWrite(payload) => {
                if let Some(sender) = self.inner.pty_writer.get() {
                    let _ = sender.send(Msg::Input(Cow::Owned(payload.into_bytes())));
                }
            }
            Event::Wakeup => self.forward(BackendEvent::Wakeup),
            Event::Bell => self.forward(BackendEvent::Bell),
            Event::Title(title) => self.forward(BackendEvent::Title(title)),
            Event::ResetTitle => self.forward(BackendEvent::ResetTitle),
            Event::MouseCursorDirty => self.forward(BackendEvent::MouseCursorDirty),
            Event::ClipboardStore(_kind, data) => {
                self.forward(BackendEvent::ClipboardStore(data));
            }
            Event::ClipboardLoad(_kind, _format) => {
                self.forward(BackendEvent::ClipboardLoad);
            }
            Event::Exit => self.forward(BackendEvent::Exit),
            Event::ChildExit(code) => self.forward(BackendEvent::ChildExit(code)),
            // Answer colour / size queries inline: TUIs that probe these
            // at startup expect a reply within milliseconds and otherwise
            // assume a degraded terminal. Routing through the View's
            // async update would add a frame of latency. We resolve
            // colours against our default palette only — per-terminal
            // OSC 4 overrides are rare and not worth the deadlock risk
            // of locking the term from the event loop thread.
            Event::ColorRequest(index, response) => {
                let rgb = self.inner.palette.lock().resolve_rgb_no_overrides(index);
                self.write_pty(response(rgb));
            }
            Event::TextAreaSizeRequest(response) => {
                let size = *self.inner.size.lock();
                self.write_pty(response(size));
            }
            // Informational; alacritty's own state is enough.
            Event::CursorBlinkingChange => {}
        }
    }
}
