//! `cargo run --bin window` — launches the gpui app shell.
//!
//! Sets up one window with our custom title-bar treatment (transparent
//! native titlebar so the tab strip can occupy that space) and hands
//! control to [`crate::app::AppShell`], which owns the tab list and the
//! active terminal.

mod app;
mod theme;

use anyhow::Result;
use gpui::{
    AppContext, Context, IntoElement, ParentElement, Render, Styled, TitlebarOptions, Window,
    WindowOptions, div,
};

use crate::app::AppShell;

struct Root {
    shell: gpui::Entity<AppShell>,
}

impl Render for Root {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().bg(theme::canvas()).child(self.shell.clone())
    }
}

fn main() -> Result<()> {
    let app = gpui::Application::new();

    app.run(move |cx| {
        cx.spawn(async move |cx| {
            cx.open_window(
                WindowOptions {
                    titlebar: Some(TitlebarOptions {
                        title: Some("CodeScope".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                |window, cx| {
                    let shell = cx.new(|cx| AppShell::new(window, cx));
                    cx.new(|_| Root { shell })
                },
            )?;
            Ok::<_, anyhow::Error>(())
        })
        .detach();
    });

    Ok(())
}
