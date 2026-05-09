//! codescope-rs spike: a single gpui window with one embedded terminal.
//!
//! On Windows it spawns pwsh.exe through ConPTY (via portable-pty).
//! Override the shell with `$env:CODESCOPE_SHELL = "powershell.exe"` (or any
//! other path) before launching.
//!
//! Goal: validate that gpui renders on this Windows machine, that ConPTY +
//! alacritty_terminal correctly drive an interactive shell, and that the
//! claude-code CLI (started inside the spawned shell) renders cleanly.

use anyhow::Result;
use gpui::{
    AppContext, Context, Edges, Entity, IntoElement, ParentElement, Render, Styled, Window, div,
    px,
};
use gpui_terminal::{ColorPalette, TerminalConfig, TerminalView};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::Arc;

/// Reader wrapper that logs every chunk of PTY output to stderr.
struct LoggingReader<R: Read>(R);
impl<R: Read> Read for LoggingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.0.read(buf)?;
        if n > 0 {
            let preview = String::from_utf8_lossy(&buf[..n])
                .chars()
                .take(180)
                .collect::<String>()
                .replace('\x1b', "\\e")
                .replace('\r', "\\r")
                .replace('\n', "\\n");
            eprintln!("[PTY→TERM] {n:>4}B  {preview}");
        }
        Ok(n)
    }
}

/// Writer wrapper that logs every byte we send to the PTY.
struct LoggingWriter<W: Write>(W);
impl<W: Write> Write for LoggingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let preview = String::from_utf8_lossy(buf)
            .chars()
            .take(180)
            .collect::<String>()
            .replace('\x1b', "\\e")
            .replace('\r', "\\r")
            .replace('\n', "\\n");
        eprintln!("[TERM→PTY] {:>4}B  {preview}", buf.len());
        self.0.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

struct TerminalApp {
    terminal: Entity<TerminalView>,
}

impl Render for TerminalApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.terminal.clone())
    }
}

fn resolve_shell() -> String {
    if let Ok(custom) = std::env::var("CODESCOPE_SHELL") {
        return custom;
    }
    if cfg!(windows) {
        "pwsh.exe".to_string()
    } else {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

fn main() -> Result<()> {
    let app = gpui::Application::new();

    app.run(move |cx| {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 30,
                cols: 100,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty failed");

        let mut cmd = CommandBuilder::new(resolve_shell());
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");

        let _child = pair
            .slave
            .spawn_command(cmd)
            .expect("spawn shell failed — is pwsh.exe on PATH? Set CODESCOPE_SHELL to override.");

        let writer = LoggingWriter(pair.master.take_writer().expect("take_writer"));
        let reader = LoggingReader(pair.master.try_clone_reader().expect("try_clone_reader"));
        let pty_master = Arc::new(parking_lot::Mutex::new(pair.master));
        drop(pair.slave);

        let pty_master_resize = pty_master.clone();
        cx.spawn(async move |cx| {
            let colors = ColorPalette::builder()
                .background(0x1e, 0x1e, 0x1e)
                .foreground(0xcc, 0xcc, 0xcc)
                .cursor(0xff, 0xff, 0xff)
                .black(0x00, 0x00, 0x00)
                .red(0xcd, 0x31, 0x31)
                .green(0x0d, 0xbc, 0x79)
                .yellow(0xe5, 0xe5, 0x10)
                .blue(0x24, 0x72, 0xc8)
                .magenta(0xbc, 0x3f, 0xbc)
                .cyan(0x11, 0xa8, 0xcd)
                .white(0xcc, 0xcc, 0xcc)
                .bright_black(0x66, 0x66, 0x66)
                .bright_red(0xf1, 0x4c, 0x4c)
                .bright_green(0x23, 0xd1, 0x8b)
                .bright_yellow(0xf5, 0xf5, 0x43)
                .bright_blue(0x3b, 0x8e, 0xea)
                .bright_magenta(0xd6, 0x70, 0xd6)
                .bright_cyan(0x29, 0xb8, 0xdb)
                .bright_white(0xff, 0xff, 0xff)
                .build();

            let config = TerminalConfig {
                font_family: "Cascadia Mono".into(),
                font_size: px(13.0),
                cols: 100,
                rows: 30,
                scrollback: 10_000,
                line_height_multiplier: 1.2,
                padding: Edges::all(px(8.0)),
                colors,
            };

            let resize_callback = move |cols: usize, rows: usize| {
                if let Err(e) = pty_master_resize.lock().resize(PtySize {
                    cols: cols as u16,
                    rows: rows as u16,
                    pixel_width: 0,
                    pixel_height: 0,
                }) {
                    eprintln!("PTY resize failed: {e}");
                }
            };

            cx.open_window(
                gpui::WindowOptions {
                    titlebar: Some(gpui::TitlebarOptions {
                        title: Some("codescope-rs spike".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let terminal = cx.new(|cx| {
                        TerminalView::new(writer, reader, config, cx)
                            .with_resize_callback(resize_callback)
                            .with_exit_callback(|_w, cx| cx.quit())
                    });
                    terminal.read(cx).focus_handle().focus(window);
                    cx.new(|_| TerminalApp { terminal })
                },
            )?;

            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });

    Ok(())
}
