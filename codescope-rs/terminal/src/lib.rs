//! CodeScope's terminal emulator.
//!
//! Three-layer architecture, modeled on Zed's `crates/terminal/`:
//!
//! * [`backend`] — PTY lifecycle, alacritty `Term` driver, event loop, and
//!   conhost-position synchronization (the missing piece in `gpui-terminal`
//!   that breaks PSReadLine on Windows).
//! * `view` — gpui View; keyboard, scrollback navigation, mouse selection,
//!   and clipboard integration. _Not yet implemented._
//! * `element` — low-level rendering element with batched text runs.
//!   _Not yet implemented._
//!
//! For now the [`backend`] surface is a placeholder so the rest of the
//! workspace can already declare a dependency and reserve the namespace.

pub mod backend;

pub use backend::Backend;
