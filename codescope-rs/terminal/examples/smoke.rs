//! Backend smoke test.
//!
//! Spawns a shell through `codescope_terminal::Backend`, writes a couple
//! of commands, and dumps the events that come back. No GUI — just a
//! stdout-only sanity check that the PTY + alacritty event loop actually
//! pumps bytes on this machine.
//!
//! Run with:
//!
//! ```pwsh
//! cargo run --example smoke -p codescope-terminal
//! ```

use std::time::Duration;

use alacritty_terminal::grid::Dimensions;
use anyhow::Result;
use codescope_terminal::{Backend, BackendEvent, SpawnConfig, TerminalSize};

fn main() -> Result<()> {
    let size = TerminalSize {
        num_lines: 24,
        num_cols: 100,
        cell_width: 8,
        cell_height: 16,
    };

    let backend = Backend::spawn(SpawnConfig {
        size,
        ..SpawnConfig::default()
    })?;

    // Give the shell time to spin up PSReadLine etc. before we type.
    std::thread::sleep(Duration::from_millis(1500));

    let line = if cfg!(target_os = "windows") {
        b"echo codescope-smoke-ok\r\n".to_vec()
    } else {
        b"echo codescope-smoke-ok\n".to_vec()
    };
    backend.write_input(line);

    let events = backend.events();
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    let mut wakeups = 0usize;
    while let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) {
        match events.recv_timeout(remaining) {
            Ok(BackendEvent::Wakeup) => {
                wakeups += 1;
            }
            Ok(BackendEvent::Title(t)) => println!("[event] title: {t:?}"),
            Ok(BackendEvent::ResetTitle) => println!("[event] reset-title"),
            Ok(BackendEvent::Bell) => println!("[event] bell"),
            Ok(BackendEvent::ChildExit(code)) => {
                println!("[event] child exit code={code}");
                break;
            }
            Ok(BackendEvent::Exit) => {
                println!("[event] exit");
                break;
            }
            Ok(other) => println!("[event] {other:?}"),
            Err(flume::RecvTimeoutError::Timeout) => break,
            Err(flume::RecvTimeoutError::Disconnected) => break,
        }
    }
    println!("[event] wakeups received: {wakeups}");

    println!("--- grid (full screen) ---");
    backend.with_term(|term| {
        let grid = term.grid();
        let lines = grid.screen_lines();
        let cols = grid.columns();
        for line_idx in 0..lines {
            let line = alacritty_terminal::index::Line(line_idx as i32);
            let mut buf = String::with_capacity(cols);
            for col in 0..cols {
                let cell =
                    &grid[line][alacritty_terminal::index::Column(col)];
                buf.push(cell.c);
            }
            println!("| {}", buf.trim_end());
        }
    });

    Ok(())
}
