//! PTY + alacritty `Term` driver.
//!
//! Owns the child-process lifecycle and pumps bytes through alacritty's VTE
//! parser. Exposes a writer for keyboard input and a resize hook. Tracks
//! the conhost scroll offset on Windows so absolute cursor-position
//! sequences (`ESC[<row>;<col>H`) emitted by ConPTY land in the correct
//! grid row even after pwsh's startup clear-screen dance.
//!
//! Implementation lands incrementally. The current type is a placeholder
//! so that `codescope-terminal::Backend` is a stable name from day one.

/// Owns the PTY and alacritty `Term`. Drives the read loop on a worker
/// thread; consumers push input via [`Backend::input`] and read snapshots
/// via [`Backend::with_term`].
///
/// Not yet implemented.
pub struct Backend;
