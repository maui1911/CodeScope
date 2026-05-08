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

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::event_loop::{EventLoopSender, Msg};
use flume::Sender;

/// Subset of alacritty events that consumers of this crate actually care
/// about. Mirrors `gpui-terminal`'s `TerminalEvent`, but keeps `ChildExit`
/// distinct from `Exit` so the View layer can differentiate "the shell
/// exited cleanly" from "alacritty asked us to tear the whole thing down".
#[derive(Debug, Clone)]
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
}

impl EventProxy {
    /// Build a fresh proxy. The returned receiver yields one [`BackendEvent`]
    /// per relevant alacritty event, in arrival order.
    pub fn new() -> (Self, flume::Receiver<BackendEvent>) {
        let (tx, rx) = flume::unbounded();
        let proxy = Self {
            inner: Arc::new(Inner {
                tx,
                pty_writer: OnceLock::new(),
            }),
        };
        (proxy, rx)
    }

    /// Wire up the event-loop sender so `Event::PtyWrite` payloads can be
    /// queued as `Msg::Input`. Calling this more than once is a no-op.
    pub fn install_pty_writer(&self, sender: EventLoopSender) {
        let _ = self.inner.pty_writer.set(sender);
    }

    fn forward(&self, event: BackendEvent) {
        // A disconnected receiver just means the consumer dropped first;
        // the event loop will keep going until shutdown is requested.
        let _ = self.inner.tx.send(event);
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
            // We don't surface these yet; the defaults inside Term are fine.
            Event::ColorRequest(_, _)
            | Event::TextAreaSizeRequest(_)
            | Event::CursorBlinkingChange => {}
        }
    }
}
