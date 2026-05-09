//! `cargo run --bin window` — minimal demo of `codescope_terminal::TerminalView`.
//!
//! Spawns the platform default shell through our own `Backend`, hosts it
//! inside a single gpui window, and lets you type. No scrollback, no
//! per-cell colour yet — see `codescope-terminal/src/view.rs` for the
//! current limits and the planned next steps.

use anyhow::Result;
use codescope_terminal::{Backend, Shell, SpawnConfig, TerminalSize, TerminalView};
use gpui::{
    AppContext, Context, Entity, Focusable, IntoElement, ParentElement, Render, Styled,
    TitlebarOptions, Window, WindowOptions, div,
};

struct TerminalApp {
    terminal: Entity<TerminalView>,
}

impl Render for TerminalApp {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.terminal.clone())
    }
}

fn main() -> Result<()> {
    let app = gpui::Application::new();

    app.run(move |cx| {
        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some("codescope-rs window".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let shell = std::env::var("CODESCOPE_SHELL")
                        .ok()
                        .map(|program| Shell::new(program, Vec::new()))
                        .or_else(|| {
                            if cfg!(windows) {
                                Some(Shell::new("pwsh.exe".into(), Vec::new()))
                            } else {
                                None
                            }
                        });

                    let mut env = std::collections::HashMap::new();
                    env.insert("TERM".into(), "xterm-256color".into());
                    env.insert("COLORTERM".into(), "truecolor".into());
                    // Identify the host so TUIs that detect minimal
                    // terminals via TERM_PROGRAM (claude-code, some
                    // Ink-based apps, …) recognise us as a real
                    // graphical terminal. Empty TERM_PROGRAM is the
                    // default on Windows ConPTY, which is why claude
                    // falls back to its stripped UI.
                    env.insert("TERM_PROGRAM".into(), "CodeScope".into());
                    env.insert("TERM_PROGRAM_VERSION".into(), "0.0.1".into());

                    let backend = Backend::spawn(SpawnConfig {
                        shell,
                        env,
                        size: TerminalSize {
                            num_lines: 30,
                            num_cols: 100,
                            cell_width: 8,
                            cell_height: 18,
                        },
                        ..SpawnConfig::default()
                    })
                    .expect("Backend::spawn failed");

                    let terminal = cx.new(|cx| TerminalView::new(backend, cx));
                    terminal.read(cx).focus_handle(cx).focus(window);
                    cx.new(|_| TerminalApp { terminal })
                },
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });

    Ok(())
}
