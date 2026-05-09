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
use std::sync::Arc;
use std::time::{Duration, Instant};

use codescope_core::{AppPaths, LayoutState, ProjectsConfig, Settings, Theme, WindowState};
use codescope_terminal::{
    Backend, ColorPalette, CursorStylePreset, FontConfig, Shell, SpawnConfig, TerminalSize,
    TerminalView,
};
use gpui::{
    AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Styled, Window, WindowBounds,
    div, px,
};
use parking_lot::Mutex;

use crate::sidebar::{Sidebar, SidebarEvent};
use crate::theme;

/// How often the window-state debounce loop wakes up to check whether
/// the latest pending save has been stable long enough.
const WINDOW_SAVE_POLL: Duration = Duration::from_millis(150);
/// How long the pending save must sit untouched before we actually
/// hit disk. Long enough that a drag-resize doesn't write on every
/// pixel; short enough that a normal resize-and-let-go feels instant.
const WINDOW_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

struct PendingWindowSave {
    state: WindowState,
    set_at: Instant,
}

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
    /// Loaded user settings. Cloned into each new tab spawn so a
    /// future "reload settings" can swap this without disturbing
    /// already-running tabs.
    settings: Arc<Settings>,
    /// Active theme — chrome reads from this on every render. Kept
    /// in an `Arc` so swapping themes is a single pointer write.
    theme: Arc<Theme>,
    /// Left rail. Lives behind a feature flag in the layout state —
    /// hidden when the user collapses the sidebar (later).
    sidebar: Entity<Sidebar>,
}

impl AppShell {
    pub fn new(
        settings: Arc<Settings>,
        theme: Arc<Theme>,
        projects: ProjectsConfig,
        layout: LayoutState,
        paths: Arc<AppPaths>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let sidebar = cx.new(|_| {
            Sidebar::new(projects, layout, theme.clone(), paths.clone())
        });

        // Sidebar drives session lifecycle; we drive terminals + tabs.
        // The `OpenSession` event is the single hand-off — sidebar
        // builds the worktree and persists, we open a tab in it.
        // `subscribe_in` (vs plain `subscribe`) gives the handler a
        // `&mut Window`, which `spawn_tab_in` needs to focus the new
        // tab's terminal.
        cx.subscribe_in(
            &sidebar,
            window,
            |this, _emitter, event, window, cx| match event {
                SidebarEvent::OpenSession {
                    working_directory,
                    title,
                } => {
                    this.spawn_tab_in(
                        Some(working_directory.clone()),
                        Some(title.clone()),
                        window,
                        cx,
                    );
                }
            },
        )
        .detach();
        let pending_window_save: Arc<Mutex<Option<PendingWindowSave>>> = Arc::new(Mutex::new(None));

        // Persist live window geometry. The observer fires for every
        // resize / move tick; we just stash the latest state and let
        // the background debounce task hit disk once the dust settles.
        cx.observe_window_bounds(window, {
            let pending = pending_window_save.clone();
            move |_, window, _| {
                let state = window_state_from_window(window);
                *pending.lock() = Some(PendingWindowSave {
                    state,
                    set_at: Instant::now(),
                });
            }
        })
        .detach();

        // Debounced disk write. The loop dies when AppShell drops
        // (`this.upgrade()` returns None), which is what we want — at
        // app shutdown anything still pending is genuinely stale.
        let pending_for_timer = pending_window_save.clone();
        let paths_for_timer = paths.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WINDOW_SAVE_POLL).await;
                if this.upgrade().is_none() {
                    break;
                }
                let to_save = {
                    let mut guard = pending_for_timer.lock();
                    match guard.as_ref() {
                        Some(p) if p.set_at.elapsed() >= WINDOW_SAVE_DEBOUNCE => guard.take(),
                        _ => None,
                    }
                };
                if let Some(p) = to_save {
                    if let Err(err) = p.state.save(&paths_for_timer) {
                        eprintln!("warning: failed to save window state: {err:#}");
                    }
                }
            }
        })
        .detach();

        let mut shell = Self {
            tabs: Vec::new(),
            active_tab: 0,
            next_id: 0,
            focus_handle,
            settings,
            theme,
            sidebar,
        };
        shell.spawn_tab(window, cx);
        shell
    }

    /// Open a fresh shell session and append it as a new tab. The new
    /// tab becomes the active one and the terminal grabs focus.
    ///
    /// Working directory + tab title come from the sidebar's currently
    /// selected project. Without a selection (cold launch, no
    /// projects yet) the shell starts in whatever cwd the binary
    /// inherited.
    fn spawn_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Pull project context from the sidebar — clone the path +
        // name so we don't hold a borrow across `cx.new` further down.
        let (working_directory, title) = match self.sidebar.read(cx).active_project() {
            Some(p) => (Some(std::path::PathBuf::from(&p.path)), Some(p.name.clone().into())),
            None => (None, None),
        };
        self.spawn_tab_in(working_directory, title, window, cx);
    }

    /// Same as [`Self::spawn_tab`] but with explicit cwd + tab title.
    /// Used by the sidebar's session orchestration: a new worktree
    /// becomes a new tab rooted in that worktree, titled
    /// `{project}/{branch}`.
    fn spawn_tab_in(
        &mut self,
        working_directory: Option<std::path::PathBuf>,
        title: Option<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

        // Build the terminal palette + cursor preset from the active
        // theme + settings. Cloned per spawn so each tab carries its
        // own snapshot — themes can swap later without breaking
        // already-running terminals.
        let palette = ColorPalette::from_theme_palette(&self.theme.palette);
        let cursor_preset = CursorStylePreset {
            shape: cursor_shape_from_str(&self.settings.cursor.shape),
            blinking: self.settings.cursor.blinking,
        };
        let font = build_font_config(&self.settings);

        let backend = match Backend::spawn(SpawnConfig {
            shell,
            working_directory,
            env,
            size: TerminalSize {
                num_lines: 30,
                num_cols: 100,
                cell_width: 8,
                cell_height: 18,
            },
            palette: Some(palette.clone()),
            scrollback: self.settings.scrollback,
            default_cursor_style: cursor_preset,
            ..SpawnConfig::default()
        }) {
            Ok(b) => b,
            Err(err) => {
                eprintln!("failed to spawn terminal backend: {err:#}");
                return;
            }
        };

        let terminal = cx.new(|cx| TerminalView::new_full(backend, palette, font, cx));
        let id = self.next_id;
        self.next_id += 1;
        let title: SharedString = title.unwrap_or_else(|| format!("Terminal {}", id + 1).into());
        self.tabs.push(Tab { id, title, terminal });
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
        // Bindings match Windows Terminal / VS Code so users have
        // the muscle memory: Ctrl+Shift+T new tab, Ctrl+Shift+W
        // close. Plain Ctrl+T / Ctrl+W stay with the shell (`yank`
        // / `unix-word-rubout` in readline, transpose-words in
        // PSReadLine) — the View deliberately leaves the shifted
        // variants for us to pick up.
        match key {
            "t" if mods.shift => {
                cx.stop_propagation();
                self.spawn_tab(window, cx);
            }
            "w" if mods.shift => {
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
        let theme = self.theme.clone();
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
            // Active tab: canvas-coloured "card" punched into the
            // elevated strip, plus a 2 px accent top border — the
            // same shape the C# tab strip uses. Inactive tabs sit
            // transparent on the strip and only fill on hover.
            let bg = if active { theme::canvas(&theme) } else { gpui::transparent_black() };
            let text_color = if active { theme::ink(&theme) } else { theme::ink_dim(&theme) };
            let top_border = if active { theme::accent(&theme) } else { gpui::transparent_black() };
            let frost_10 = theme::frost_10(&theme);
            let frost_20 = theme::frost_20(&theme);
            let ink = theme::ink(&theme);
            let ink_ghost = theme::ink_ghost(&theme);
            let status_dot = if active { theme::status_running() } else { theme::ink_ghost(&theme) };

            div()
                .id(("tab", id))
                .h_full()
                .min_w(px(140.0))
                .max_w(px(240.0))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .border_t_2()
                .border_color(top_border)
                .bg(bg)
                .text_color(text_color)
                .hover(move |s| {
                    if active { s } else { s.bg(frost_10).text_color(ink) }
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.activate_tab(idx, window, cx);
                    }),
                )
                // Status dot — green = active session, dim = inactive.
                // Cheap visual hook even before we wire real status.
                .child(
                    div()
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded_full()
                        .bg(status_dot),
                )
                .child(div().flex_grow().truncate().child(title))
                .child(
                    div()
                        .id(("close", idx as u64))
                        .w(px(20.0))
                        .h(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .text_color(ink_ghost)
                        .hover(move |s| s.bg(frost_20).text_color(ink))
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

        let new_tab_frost = theme::frost_10(&theme);
        let new_tab_accent = theme::accent(&theme);
        // `h(40)` instead of `h_full()` because the strip's flex-row
        // doesn't always resolve `h_full` to the parent's 40 px before
        // hit-testing happens — the button looked the right size but
        // its hit area collapsed to 0, killing both hover and click.
        let new_tab_button = div()
            .id("new-tab")
            .h(px(40.0))
            .w(px(40.0))
            .flex()
            .items_center()
            .justify_center()
            .text_color(theme::ink_dim(&theme))
            .text_size(px(18.0))
            .cursor_pointer()
            .hover(move |s| s.bg(new_tab_frost).text_color(new_tab_accent))
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
            .border_b_1()
            .border_color(theme::divider(&theme))
            .bg(theme::elevated(&theme))
            .children(tabs)
            .child(new_tab_button);

        let body = if let Some(terminal) = active_terminal {
            div().size_full().child(terminal).into_any_element()
        } else {
            // No tabs left — usually we've just quit, but render a
            // black void in the meantime so we never flash.
            div().size_full().into_any_element()
        };

        let main_row = div()
            .flex_grow()
            .flex()
            .flex_row()
            .child(self.sidebar.clone())
            .child(div().flex_grow().child(body));

        div()
            .key_context("AppShell")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::canvas(&theme))
            .text_color(theme::ink(&theme))
            .child(tab_strip)
            .child(main_row)
    }
}

// ─── Window-state extraction ────────────────────────────────────────

/// Snapshot the platform window into the on-disk `WindowState`. We
/// always store the *restore* bounds so unmaximising lands at a
/// sensible size; `maximised` is a separate flag the loader uses to
/// pick `WindowBounds::Maximized` vs `Windowed`.
fn window_state_from_window(window: &Window) -> WindowState {
    let restore = match window.window_bounds() {
        WindowBounds::Windowed(b) => b,
        WindowBounds::Maximized(b) => b,
        WindowBounds::Fullscreen(b) => b,
    };
    let f32_x: f32 = restore.origin.x.into();
    let f32_y: f32 = restore.origin.y.into();
    let f32_w: f32 = restore.size.width.into();
    let f32_h: f32 = restore.size.height.into();
    WindowState {
        x: f32_x as i32,
        y: f32_y as i32,
        width: f32_w.max(0.0) as u32,
        height: f32_h.max(0.0) as u32,
        maximised: window.is_maximized(),
    }
}

// ─── Settings → terminal-config helpers ─────────────────────────────

fn cursor_shape_from_str(s: &str) -> codescope_terminal::CursorShape {
    use codescope_terminal::CursorShape;
    match s {
        "block" => CursorShape::Block,
        "underline" | "underscore" => CursorShape::Underline,
        "hollow-block" | "hollow_block" => CursorShape::HollowBlock,
        // "beam", anything else → beam (Windows-Terminal default).
        _ => CursorShape::Beam,
    }
}

fn build_font_config(settings: &Settings) -> FontConfig {
    let family: SharedString = if settings.font.family.is_empty() {
        // Empty `font.family` in settings.json falls back to whatever
        // `FontConfig::default()` picks — currently the same Nerd-Font-
        // first chain, just sourced from the env (`CODESCOPE_FONT`) or
        // hard-coded, *not* an OS-supplied "platform default monospace".
        // True system-default font picking would need a platform-
        // specific resolver (DirectWrite IDWriteSystemFontCollection on
        // Windows, NSFont/userFixedPitchFont on macOS, fontconfig on
        // Linux). Land that the day someone actually asks for it.
        FontConfig::default().family
    } else {
        settings.font.family.clone().into()
    };
    let fallbacks = settings.font.fallbacks.iter().map(|s| s.clone().into()).collect();
    let size = px(settings.font.size.max(1.0));
    // line_height_multiplier is recorded for later — gpui's
    // text_system measures line height directly, so we just stash
    // a placeholder here. A multiplier knob lands when the renderer
    // exposes it.
    let line_height = px(settings.font.size * settings.font.line_height_multiplier.max(0.5));
    FontConfig {
        family,
        fallbacks,
        size,
        line_height,
        cell_width: px(settings.font.size * 0.6),
    }
}
