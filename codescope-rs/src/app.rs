//! Application shell — the chrome that wraps one or more
//! `TerminalView` entities.
//!
//! Layout (top to bottom):
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │ tab strip (40px) — pill tabs · "+" · caption controls │  ← in titlebar
//! ├────────────────────────────────────────────────────────┤
//! │                                                        │
//! │              active TerminalView (size_full)           │
//! │                                                        │
//! └────────────────────────────────────────────────────────┘
//! ```
//!
//! Visuals follow `src/CodeScope.App/Styles/DesignTokens.xaml`:
//! pure-black canvas, pure-white ink, single Framer Blue accent,
//! frosted-glass surfaces. See [`crate::theme`] for the tokens.
//!
//! Each tab owns its own [`Backend`] + [`TerminalView`]. Closing a
//! tab drops the entity, which drops the backend, which sends
//! `Msg::Shutdown` to the alacritty event loop and joins the worker
//! thread.

use std::collections::HashMap;

use codescope_terminal::{Backend, Shell, SpawnConfig, TerminalSize, TerminalView};
use gpui::{
    AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Styled, Window, div, px,
};

use crate::theme;

/// One tab = one terminal session.
struct Tab {
    id: u64,
    title: SharedString,
    terminal: Entity<TerminalView>,
}

pub struct AppShell {
    tabs: Vec<Tab>,
    active_tab: usize,
    next_id: u64,
    focus_handle: FocusHandle,
}

impl AppShell {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let mut shell = Self {
            tabs: Vec::new(),
            active_tab: 0,
            next_id: 0,
            focus_handle,
        };
        shell.spawn_tab(window, cx);
        shell
    }

    /// Open a fresh shell session and append it as a new tab. The new
    /// tab becomes the active one and the terminal grabs focus.
    fn spawn_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
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

        let mut env = HashMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "CodeScope".into());
        env.insert("TERM_PROGRAM_VERSION".into(), "0.0.1".into());

        let backend = match Backend::spawn(SpawnConfig {
            shell,
            env,
            size: TerminalSize {
                num_lines: 30,
                num_cols: 100,
                cell_width: 8,
                cell_height: 18,
            },
            ..SpawnConfig::default()
        }) {
            Ok(b) => b,
            Err(err) => {
                eprintln!("failed to spawn terminal backend: {err:#}");
                return;
            }
        };

        let terminal = cx.new(|cx| TerminalView::new(backend, cx));
        let id = self.next_id;
        self.next_id += 1;
        self.tabs.push(Tab {
            id,
            title: format!("Terminal {}", id + 1).into(),
            terminal,
        });
        let new_idx = self.tabs.len() - 1;
        self.activate_tab(new_idx, window, cx);
    }

    fn close_tab(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        self.tabs.remove(idx);
        if self.tabs.is_empty() {
            // Last tab closed — quit the app.
            cx.quit();
            return;
        }
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        } else if self.active_tab > idx {
            self.active_tab -= 1;
        }
        self.activate_tab(self.active_tab, window, cx);
    }

    fn activate_tab(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.tabs.len() {
            return;
        }
        self.active_tab = idx;
        let handle = self.tabs[idx].terminal.read(cx).focus_handle(cx);
        handle.focus(window);
        cx.notify();
    }

    fn next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let next = (self.active_tab + 1) % self.tabs.len();
        self.activate_tab(next, window, cx);
    }

    fn prev_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabs.is_empty() {
            return;
        }
        let prev = if self.active_tab == 0 {
            self.tabs.len() - 1
        } else {
            self.active_tab - 1
        };
        self.activate_tab(prev, window, cx);
    }

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mods = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        // Cmd on macOS, Ctrl elsewhere — gpui already maps `platform`
        // to the right modifier per OS, so we just check both.
        let app_mod = mods.control || mods.platform;
        if !app_mod || mods.alt {
            return;
        }
        match key {
            "t" if !mods.shift => {
                cx.stop_propagation();
                self.spawn_tab(window, cx);
            }
            "w" if !mods.shift => {
                cx.stop_propagation();
                self.close_tab(self.active_tab, window, cx);
            }
            "tab" if !mods.shift => {
                cx.stop_propagation();
                self.next_tab(window, cx);
            }
            "tab" if mods.shift => {
                cx.stop_propagation();
                self.prev_tab(window, cx);
            }
            d if !mods.shift && d.len() == 1 => {
                if let Some(n) = d.chars().next().and_then(|c| c.to_digit(10)) {
                    if n >= 1 && n <= 9 {
                        let idx = (n as usize) - 1;
                        if idx < self.tabs.len() {
                            cx.stop_propagation();
                            self.activate_tab(idx, window, cx);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

impl Focusable for AppShell {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppShell {
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Snapshot the data we need into owned values up front. Lets us
        // hand each `cx.listener` its own mutable borrow without
        // overlapping with the immutable borrow `self.tabs.iter()`
        // would otherwise hold.
        let active_idx = self.active_tab;
        let tab_meta: Vec<(usize, u64, SharedString)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(idx, tab)| (idx, tab.id, tab.title.clone()))
            .collect();
        let active_terminal = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.terminal.clone());

        let tabs = tab_meta.into_iter().map(|(idx, id, title)| {
            let active = idx == active_idx;
            let bg = if active {
                theme::frost_10()
            } else {
                theme::canvas()
            };
            let text_color = if active {
                theme::ink()
            } else {
                theme::ink_dim()
            };

            div()
                .id(("tab", id))
                .h_full()
                .my(px(6.0))
                .px_3()
                .min_w(px(120.0))
                .max_w(px(220.0))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .rounded_md()
                .bg(bg)
                .text_color(text_color)
                .hover(|s| s.bg(theme::frost_10()))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.activate_tab(idx, window, cx);
                    }),
                )
                .child(div().flex_grow().truncate().child(title))
                .child(
                    div()
                        .id(("close", idx as u64))
                        .w(px(18.0))
                        .h(px(18.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_color(theme::ink_ghost())
                        .hover(|s| s.bg(theme::frost_20()).text_color(theme::ink()))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(idx, window, cx);
                            }),
                        )
                        .child("×"),
                )
        });

        let new_tab_button = div()
            .id("new-tab")
            .my(px(6.0))
            .w(px(28.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .text_color(theme::ink_dim())
            .hover(|s| s.bg(theme::frost_10()).text_color(theme::ink()))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.spawn_tab(window, cx);
                }),
            )
            .child("+");

        let tab_strip = div()
            .h(px(40.0))
            .flex()
            .flex_row()
            .px_2()
            .gap_1()
            .border_b_1()
            .border_color(theme::divider())
            .bg(theme::near_black())
            .children(tabs)
            .child(new_tab_button);

        let body = if let Some(terminal) = active_terminal {
            div().size_full().child(terminal).into_any_element()
        } else {
            // No tabs left — usually we've just quit, but render a
            // black void in the meantime so we never flash.
            div().size_full().into_any_element()
        };

        div()
            .key_context("AppShell")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::canvas())
            .text_color(theme::ink())
            .child(tab_strip)
            .child(div().flex_grow().child(body))
    }
}
