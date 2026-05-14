//! Diagnostic: validates that portable-pty + ConPTY produces output on Windows.
//!
//! Spawns cmd.exe (or pwsh.exe via CODESCOPE_SHELL) inside a PTY, then reads
//! bytes from the master end on a thread and prints what it sees, with hex
//! and "lossy UTF-8" decoding side-by-side. Exits when the child closes
//! stdout, when no bytes arrive for 5 seconds, or when you press Ctrl-C.
//!
//! This isolates the PTY pipeline from gpui — if THIS prints bytes,
//! portable-pty works fine and the issue is in gpui-terminal's render path.

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::Read;
use std::sync::mpsc;
use std::time::{Duration, Instant};

fn main() -> anyhow::Result<()> {
    let shell = std::env::var("CODESCOPE_SHELL").unwrap_or_else(|_| "cmd.exe".to_string());
    eprintln!("[ptytest] spawning shell: {shell}");

    let pty_system = native_pty_system();
    let pair = pty_system.openpty(PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    eprintln!("[ptytest] PTY opened (24x80)");

    let mut cmd = CommandBuilder::new(&shell);
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    let mut child = pair.slave.spawn_command(cmd)?;
    eprintln!("[ptytest] child spawned");

    let mut reader = pair.master.try_clone_reader()?;
    drop(pair.slave);

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    eprintln!("[ptytest] reader: EOF");
                    break;
                }
                Ok(n) => {
                    let chunk = buf[..n].to_vec();
                    if tx.send(chunk).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    eprintln!("[ptytest] reader error: {e}");
                    break;
                }
            }
        }
    });

    let start = Instant::now();
    let mut last_byte = Instant::now();
    let mut total_bytes = 0usize;
    let mut chunks = 0usize;

    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(bytes) => {
                chunks += 1;
                total_bytes += bytes.len();
                last_byte = Instant::now();
                let preview: String = String::from_utf8_lossy(&bytes).chars().take(120).collect();
                eprintln!(
                    "[ptytest] +{:?} chunk #{chunks}: {} bytes  text=<{}>",
                    start.elapsed(),
                    bytes.len(),
                    preview.replace('\r', "\\r").replace('\n', "\\n").replace('\x1b', "\\e")
                );
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if last_byte.elapsed() > Duration::from_secs(5) && total_bytes > 0 {
                    eprintln!(
                        "[ptytest] idle 5s after {total_bytes} bytes / {chunks} chunks — exiting"
                    );
                    break;
                }
                if last_byte.elapsed() > Duration::from_secs(10) && total_bytes == 0 {
                    eprintln!("[ptytest] NOTHING received in 10s — PTY pipeline is broken");
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("[ptytest] reader thread gone");
                break;
            }
        }
    }

    let _ = child.kill();
    eprintln!(
        "[ptytest] DONE — total: {total_bytes} bytes in {chunks} chunks over {:?}",
        start.elapsed()
    );
    Ok(())
}
