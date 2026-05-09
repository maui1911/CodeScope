//! Application shell — the chrome that wraps one or more
//! `TerminalView` entities.
//!
//! Layout (top to bottom):
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │ brand · [strip g0] | [strip g1] · drag · ▭ ▢ ✕         │  ← 40 px titlebar row
//! ├────────────────────────────────────────────────────────┤
//! │ side │  pane g0          │  pane g1                    │
//! │ bar  │  (active term)    │  (active term)              │
//! └────────────────────────────────────────────────────────┘
//! ```
//!
//! Each top-level "group" is a tab strip + active terminal. Multiple
//! groups sit side-by-side with the same weight per column on the
//! tab-strip row and the body row, so clicking a strip section focuses
//! the matching pane below. Groups mirror the C# build's
//! `EditorGroupViewModel` flat list — no recursive split tree.
//!
//! Visuals follow `src/CodeScope.App/Styles/DesignTokens.xaml`:
//! pure-black canvas, pure-white ink, single Framer Blue accent,
//! frosted-glass surfaces. See [`crate::theme`] for the tokens.
//!
//! Each tab owns its own [`Backend`] + [`TerminalView`]. Closing a
//! tab drops the entity, which drops the backend, which sends
//! `Msg::Shutdown` to the alacritty event loop and joins the worker
//! thread. When the last tab in a non-only group is closed the group
//! itself collapses (mirroring `MainViewModel.CloseGroup`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codescope_core::{AppPaths, LayoutState, ProjectsConfig, Settings, Theme, WindowState};
use codescope_terminal::{
    Backend, ColorPalette, CursorStylePreset, FontConfig, Shell, SpawnConfig, TerminalSize,
    TerminalView,
};
#[cfg(not(target_os = "windows"))]
use gpui::ClickEvent;
#[cfg(not(target_os = "windows"))]
use gpui::StatefulInteractiveElement;
use gpui::{
    AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Styled, Window, WindowBounds,
    WindowControlArea, div, px,
};
use parking_lot::Mutex;

use crate::sidebar::{SIDEBAR_WIDTH, Sidebar, SidebarEvent};
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

/// One column in the work area: a tab strip + the currently-selected
/// tab's terminal. Mirrors `EditorGroupViewModel` from the C# build —
/// flat list, no recursion. The group's identity is its `id` so we can
/// stamp gpui element ids that survive across renders without going
/// through the index (which shifts when a sibling group collapses).
struct Group {
    id: u64,
    tabs: Vec<Tab>,
    /// Local tab index inside this group. Each group has its own
    /// active tab — focusing one group doesn't change another's
    /// selection.
    active_tab: usize,
}

/// Live state for an in-flight splitter drag. Captured at mouse-down
/// on a divider so each subsequent `on_mouse_move` can recompute the
/// new weights from the *original* numbers — re-deriving from the
/// live weights every frame would compound rounding error across the
/// many small moves a real drag generates.
struct SplitterDrag {
    /// Index of the gap being dragged. Resolves to splitting between
    /// `groups[split_idx]` (left) and `groups[split_idx + 1]` (right).
    split_idx: usize,
    /// Cursor X at drag start, in window coords.
    start_x: gpui::Pixels,
    /// Snapshot of weights at drag start, indexed parallel to
    /// `groups`. We only ever modify entries `split_idx` and
    /// `split_idx + 1`; the rest stay at their snapshot values so
    /// neighbouring groups don't shift while we resize.
    start_weights: Vec<f32>,
    /// Pixels of work-area width per unit of weight at drag start.
    /// Set from `(viewport_width - sidebar_width) / total_weight`.
    /// Used to translate cursor delta-x into a weight delta.
    px_per_unit: f32,
}

/// Smallest weight we let either side of a drag go to. Below this the
/// pane visibly disappears and the user can't get focus back to it
/// without a Ctrl+Shift+W to remove the empty group. Mirrors C#'s
/// `MinWidth = 200` on the GridSplitter columns at the conceptual
/// level; ours is in weight units rather than pixels because the
/// total-width depends on the viewport.
const MIN_GROUP_WEIGHT: f32 = 0.15;
/// Width of the actual splitter hit-target. Wider than the painted
/// 1px divider so the user can actually grab it without pixel-perfect
/// aiming.
const SPLITTER_HIT_WIDTH: f32 = 6.0;

pub struct AppShell {
    /// Flat list of tab groups laid out left-to-right. Always at
    /// least one entry — `close_tab` collapses an emptied group only
    /// when there are siblings to fall back on, and `close_focused_group`
    /// quits the app when invoked on the only remaining group.
    groups: Vec<Group>,
    /// Index into `groups` of the currently-focused column. Keyboard
    /// shortcuts (Ctrl+T, Ctrl+\, Ctrl+W, Ctrl+1..9) target this
    /// group; click on any pane / tab strip section moves the focus.
    focused_group: usize,
    /// Per-group flex weights. Length always matches `groups.len()`.
    /// `split_right` pushes 1.0; `close_tab`'s collapse drops the
    /// matching entry. The render loop maps these to flex_grow values
    /// so a 1.5 / 1.0 split allocates 60% / 40% of the work area.
    group_weights: Vec<f32>,
    /// In-flight splitter drag, if any. `Some` between mouse-down on
    /// a divider and mouse-up. Tracks the gap index (which two
    /// adjacent groups the splitter sits between) plus the cursor
    /// origin and weight snapshot so each `mouse_move` can recompute
    /// from the original numbers — re-deriving from the live weights
    /// would compound rounding error across many small mouse moves.
    splitter_drag: Option<SplitterDrag>,
    /// Threading the on-disk path bundle through so `save_layout` can
    /// reach `paths.layout_file()` without us having to pull it from
    /// the sidebar every time.
    paths: Arc<AppPaths>,
    /// In-memory copy of `layout.json` — kept in sync as group
    /// weights / focus / counts change so a save-on-change writes the
    /// full struct instead of the field we touched.
    layout: LayoutState,
    next_group_id: u64,
    next_tab_id: u64,
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
        // The sidebar reads sidebar-* fields and selectedProjectId
        // out of the same `LayoutState`; we keep our own clone for
        // the group fields so save-on-change here doesn't trample
        // sidebar writes (and vice versa). Two writers to the same
        // file is acceptable — both go through the same atomic write
        // wrapper, last-writer-wins on order.
        let sidebar = cx.new(|_| {
            Sidebar::new(projects, layout.clone(), theme.clone(), paths.clone())
        });

        // Spawn a tab whenever the sidebar asks us to — fired by a
        // worktree-row click in the project list and by a successful
        // `submit_new_worktree_dialog`. `subscribe_in` (vs
        // `subscribe`) is the variant that hands us `&mut Window`,
        // which we need so the freshly-spawned terminal can grab
        // focus inline.
        cx.subscribe_in(&sidebar, window, |this, _sidebar, event, window, cx| {
            match event {
                SidebarEvent::OpenSession { working_directory, title } => {
                    this.spawn_tab_in(
                        Some(working_directory.clone()),
                        Some(title.clone()),
                        window,
                        cx,
                    );
                }
            }
        })
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
                if let Some(p) = to_save
                    && let Err(err) = p.state.save(&paths_for_timer)
                {
                    eprintln!("warning: failed to save window state: {err:#}");
                }
            }
        })
        .detach();

        let mut shell = Self {
            groups: vec![Group { id: 0, tabs: Vec::new(), active_tab: 0 }],
            focused_group: 0,
            group_weights: vec![1.0],
            splitter_drag: None,
            paths: paths.clone(),
            layout,
            next_group_id: 1,
            next_tab_id: 0,
            focus_handle,
            settings,
            theme,
            sidebar,
        };
        shell.spawn_tab(window, cx);
        shell
    }

    /// Persist the current group layout (weights + focus index) to
    /// `layout.json`. Called after splitter-drag end, split-right, and
    /// group-collapse — anything that mutates either field. Never
    /// fails fatally; logs and moves on, the next save will retry.
    fn save_layout(&mut self) {
        self.layout.group_weights = self.group_weights.clone();
        self.layout.focused_group_index = self.focused_group;
        if let Err(err) = self.layout.save(self.paths.as_ref()) {
            eprintln!("warning: failed to save layout.json: {err:#}");
        }
    }

    fn focused_group(&self) -> &Group {
        &self.groups[self.focused_group]
    }

    /// Open a fresh shell session and append it as a new tab. The new
    /// tab becomes the active one and the terminal grabs focus.
    ///
    /// Without an explicit `working_directory` / `title`, working
    /// directory + tab title come from the sidebar's currently
    /// selected project (cold launch, "+ new tab" button). Callers
    /// that already know which path to pin the terminal to (sidebar
    /// worktree clicks, post-create-worktree spawns) hand both in
    /// directly.
    fn spawn_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.spawn_tab_in(None, None, window, cx);
    }

    fn spawn_tab_in(
        &mut self,
        working_directory: Option<std::path::PathBuf>,
        title_override: Option<SharedString>,
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

        // Resolve working directory + tab title. Explicit args win;
        // otherwise pull project context from the sidebar — clone the
        // path + name so we don't hold a borrow across `cx.new`.
        let active_project = self
            .sidebar
            .read(cx)
            .active_project()
            .map(|p| (p.path.clone(), p.name.clone()));
        let working_directory = working_directory.or_else(|| {
            active_project
                .as_ref()
                .map(|(path, _)| std::path::PathBuf::from(path))
        });

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
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let title: SharedString = title_override
            .or_else(|| active_project.map(|(_, name)| name.into()))
            .unwrap_or_else(|| format!("Terminal {}", id + 1).into());
        let group_idx = self.focused_group;
        let group = &mut self.groups[group_idx];
        group.tabs.push(Tab { id, title, terminal });
        let new_idx = group.tabs.len() - 1;
        self.activate_tab(group_idx, new_idx, window, cx);
    }

    /// Close the tab at `(group_idx, tab_idx)`. When the group's last
    /// tab closes we collapse the group entirely (provided at least
    /// one sibling remains); when no groups remain we quit.
    ///
    /// Closing a tab in an *unfocused* group keeps focus where it was
    /// (mirrors C# `MainViewModel.CloseTabAsync` — the user clicked an
    /// "x" on a sibling, they didn't ask to switch contexts). Auto-
    /// collapsing a focused group that becomes empty does refocus the
    /// fallback group so typing keeps landing somewhere.
    fn close_tab(
        &mut self,
        group_idx: usize,
        tab_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.groups.get_mut(group_idx) else { return };
        if tab_idx >= group.tabs.len() {
            return;
        }
        let was_focused_group = self.focused_group == group_idx;
        group.tabs.remove(tab_idx);
        if group.tabs.is_empty() {
            // Empty group — collapse if there are siblings, otherwise
            // quit.
            if self.groups.len() == 1 {
                cx.quit();
                return;
            }
            self.groups.remove(group_idx);
            // Drop the matching weight slot. Surviving weights stay
            // unchanged — closing one column doesn't redistribute, so
            // the remaining groups keep their ratios. Mirrors C#'s
            // `MainViewModel.CloseGroup`.
            if group_idx < self.group_weights.len() {
                self.group_weights.remove(group_idx);
            }
            // If the focused group was at or after `group_idx`, slide
            // its index left so it keeps pointing at the same group.
            // Re-focus the (possibly new) focused group so keyboard
            // input lands somewhere live.
            if self.focused_group >= self.groups.len() {
                self.focused_group = self.groups.len() - 1;
            } else if group_idx <= self.focused_group && self.focused_group > 0 {
                self.focused_group -= 1;
            }
            let active = self.groups[self.focused_group].active_tab;
            self.activate_tab(self.focused_group, active, window, cx);
            self.save_layout();
            return;
        }
        // Group still has tabs — slide the active index left if we
        // closed at or before it.
        let group = &mut self.groups[group_idx];
        if group.active_tab >= group.tabs.len() {
            group.active_tab = group.tabs.len() - 1;
        } else if group.active_tab > tab_idx {
            group.active_tab -= 1;
        }
        if was_focused_group {
            let new_active = group.active_tab;
            self.activate_tab(group_idx, new_active, window, cx);
        } else {
            // Closing a tab in a sibling group — adjust its `active_tab`
            // (already done above) and redraw, but leave keyboard focus
            // on the user's actually-focused group.
            cx.notify();
        }
    }

    /// Activate `(group_idx, tab_idx)`. Sets focused group, marks the
    /// tab as the group's active one, and routes keyboard focus to the
    /// terminal so typing lands in the right pty without an extra
    /// click.
    fn activate_tab(
        &mut self,
        group_idx: usize,
        tab_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.groups.get_mut(group_idx) else { return };
        if tab_idx >= group.tabs.len() {
            return;
        }
        group.active_tab = tab_idx;
        let prev_focused = self.focused_group;
        self.focused_group = group_idx;
        let handle = self.groups[group_idx].tabs[tab_idx].terminal.read(cx).focus_handle(cx);
        handle.focus(window);
        cx.notify();
        if prev_focused != group_idx {
            self.save_layout();
        }
    }

    fn next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let group_idx = self.focused_group;
        let group = &self.groups[group_idx];
        if group.tabs.is_empty() {
            return;
        }
        let next = (group.active_tab + 1) % group.tabs.len();
        self.activate_tab(group_idx, next, window, cx);
    }

    fn prev_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let group_idx = self.focused_group;
        let group = &self.groups[group_idx];
        if group.tabs.is_empty() {
            return;
        }
        let prev = if group.active_tab == 0 {
            group.tabs.len() - 1
        } else {
            group.active_tab - 1
        };
        self.activate_tab(group_idx, prev, window, cx);
    }

    /// Drop the focused group entirely. Pre-condition: the group is
    /// empty (caller checks) and there's at least one sibling. After
    /// removal we land focus on the previous (or first remaining)
    /// group's active tab so typing keeps working without an extra
    /// click. Mirrors `MainViewModel.CloseGroup`.
    fn close_focused_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        debug_assert!(self.focused_group < self.groups.len());
        debug_assert!(self.groups[self.focused_group].tabs.is_empty());
        debug_assert!(self.groups.len() > 1);
        let removed = self.focused_group;
        self.groups.remove(removed);
        if removed < self.group_weights.len() {
            self.group_weights.remove(removed);
        }
        if self.focused_group >= self.groups.len() {
            self.focused_group = self.groups.len() - 1;
        }
        let active = self.groups[self.focused_group].active_tab;
        self.activate_tab(self.focused_group, active, window, cx);
        self.save_layout();
    }

    /// Append a new empty group to the right of the focused one and
    /// move focus to it. Mirrors `MainViewModel.SplitRight` (Ctrl+\ in
    /// the C# build). The new group has no tabs — the caller usually
    /// follows up with `spawn_tab`, or the user does via Ctrl+T / the
    /// `+` button on the new strip.
    fn split_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_group_id;
        self.next_group_id += 1;
        let insert_at = self.focused_group + 1;
        self.groups.insert(
            insert_at,
            Group { id, tabs: Vec::new(), active_tab: 0 },
        );
        // New group enters with weight 1.0 — equal share of whatever
        // a unit weight resolves to in the current layout. The
        // existing groups keep their weights so a 1.5/1.0 split that
        // gets a third group becomes 1.5/1.0/1.0 (≈42.9/28.6/28.6%).
        self.group_weights.insert(insert_at, 1.0);
        self.focused_group = insert_at;
        // Drop keyboard focus back on AppShell's root handle so the
        // next typed character isn't routed to a now-stale terminal
        // (the previously focused group's). Once the user types Ctrl+T
        // / clicks +, `activate_tab` will rehome focus.
        self.focus_handle.focus(window);
        cx.notify();
        self.save_layout();
    }

    /// Mouse pressed on the splitter at gap `split_idx`. Captures the
    /// snapshot the drag-update needs and stamps the cursor as
    /// col-resize until release.
    fn begin_splitter_drag(
        &mut self,
        split_idx: usize,
        cursor_x: gpui::Pixels,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if split_idx + 1 >= self.groups.len() {
            return;
        }
        // Pixels-per-weight conversion. Total work-area width is the
        // viewport minus the sidebar (and minus the per-splitter
        // hit-targets, but those are tiny — close enough for drag
        // feel that we ignore them here). Total weight is the sum of
        // all current weights.
        let viewport: f32 = window.viewport_size().width.into();
        let work_width = (viewport - SIDEBAR_WIDTH).max(1.0);
        let total_weight: f32 = self.group_weights.iter().copied().sum::<f32>().max(0.001);
        let px_per_unit = work_width / total_weight;
        self.splitter_drag = Some(SplitterDrag {
            split_idx,
            start_x: cursor_x,
            start_weights: self.group_weights.clone(),
            px_per_unit,
        });
    }

    /// Cursor moved while a splitter drag is in flight. Recomputes the
    /// two affected weights from the start snapshot — re-deriving
    /// from the live values would compound rounding error across the
    /// many per-pixel moves a real drag generates.
    fn update_splitter_drag(&mut self, cursor_x: gpui::Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.splitter_drag.as_ref() else { return };
        let split_idx = drag.split_idx;
        if split_idx + 1 >= self.group_weights.len() {
            // Group count changed under us (collapse mid-drag) — bail.
            self.splitter_drag = None;
            return;
        }
        let dx: f32 = (cursor_x - drag.start_x).into();
        let mut delta_units = dx / drag.px_per_unit;
        let left = drag.start_weights[split_idx];
        let right = drag.start_weights[split_idx + 1];
        // Clamp so neither side disappears below MIN_GROUP_WEIGHT.
        if left + delta_units < MIN_GROUP_WEIGHT {
            delta_units = MIN_GROUP_WEIGHT - left;
        }
        if right - delta_units < MIN_GROUP_WEIGHT {
            delta_units = right - MIN_GROUP_WEIGHT;
        }
        self.group_weights[split_idx] = left + delta_units;
        self.group_weights[split_idx + 1] = right - delta_units;
        cx.notify();
    }

    /// Mouse released — commit the drag-end weights to disk so the
    /// resized column survives a restart.
    fn end_splitter_drag(&mut self, cx: &mut Context<Self>) {
        if self.splitter_drag.take().is_some() {
            self.save_layout();
            cx.notify();
        }
    }

    /// Move focus to the group at `idx`. Routes keyboard focus to the
    /// group's currently-active tab so typing resumes in the right
    /// terminal. No-op when the index is out of range or the group is
    /// empty (an empty group has no terminal to focus — we still set
    /// `focused_group` so a subsequent Ctrl+T lands in this column).
    fn focus_group(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.groups.len() {
            return;
        }
        if self.groups[idx].tabs.is_empty() {
            self.focused_group = idx;
            self.focus_handle.focus(window);
            cx.notify();
            return;
        }
        let active = self.groups[idx].active_tab;
        self.activate_tab(idx, active, window, cx);
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
                let g = self.focused_group;
                let group = self.focused_group();
                if group.tabs.is_empty() {
                    // Empty focused group — collapse it so the user
                    // can undo an accidental split right without
                    // having to spawn a tab first. Mirrors the
                    // `tab is null` branch of `MainViewModel.CloseTabAsync`.
                    // No-op when this is the only group; the user has
                    // to close their last tab to quit.
                    if self.groups.len() > 1 {
                        self.close_focused_group(window, cx);
                    }
                } else {
                    let t = group.active_tab;
                    self.close_tab(g, t, window, cx);
                }
            }
            // Ctrl+\ — split the focused group to the right. Matches
            // the C# binding (`SplitRightCommand`). Backslash is a
            // single-key chord on US/most layouts, so we hit it here
            // alongside the shifted variant in case of layouts that
            // treat the bare key as a different glyph.
            "\\" => {
                cx.stop_propagation();
                self.split_right(window, cx);
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
                if let Some(n) = d.chars().next().and_then(|c| c.to_digit(10))
                    && (1..=9).contains(&n)
                {
                    let idx = (n as usize) - 1;
                    let group_idx = self.focused_group;
                    if idx < self.groups[group_idx].tabs.len() {
                        cx.stop_propagation();
                        self.activate_tab(group_idx, idx, window, cx);
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
        // Snapshot per-group + per-tab metadata up front so each
        // `cx.listener` closure can hold owned values without
        // overlapping the immutable borrow `self.groups.iter()` would
        // otherwise extend across the rest of `render`.
        let focused_group_idx = self.focused_group;
        let groups_meta: Vec<GroupRenderData> = self
            .groups
            .iter()
            .enumerate()
            .map(|(g_idx, group)| GroupRenderData {
                group_idx: g_idx,
                group_id: group.id,
                active_tab: group.active_tab,
                is_focused: g_idx == focused_group_idx,
                weight: self.group_weights.get(g_idx).copied().unwrap_or(1.0),
                tabs: group
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(t_idx, tab)| TabRenderData {
                        tab_idx: t_idx,
                        tab_id: tab.id,
                        title: tab.title.clone(),
                    })
                    .collect(),
                active_terminal: group
                    .tabs
                    .get(group.active_tab)
                    .map(|t| t.terminal.clone()),
            })
            .collect();

        // The drag region fills the gap between the strips and the
        // caption controls so the user can grab the bar to move the
        // window. `window_control_area(Drag)` annotates the hitbox so
        // Windows hit-tests it as `HTCAPTION` — that gets us native
        // drag, snap-layouts, and *correct* double-click toggle
        // (maximize ↔ restore) for free. We deliberately don't attach
        // an `on_mouse_down(start_window_move)` or
        // `on_click(zoom_window)` here on Windows: both fight the
        // native NC handling. `zoom_window()` in particular is
        // `SW_MAXIMIZE`-only on Windows (no toggle), so wiring it
        // would maximize on click and never restore.
        //
        // On Wayland / X11 the compositor needs an explicit
        // `start_window_move()` because there's no NC-area hit-test
        // model — keep that path under `#[cfg(not(target_os = "windows"))]`.
        let drag_region = {
            let base = div().id("titlebar-drag").flex_grow().h(px(40.0));
            #[cfg(target_os = "windows")]
            let base = base.window_control_area(WindowControlArea::Drag);
            #[cfg(not(target_os = "windows"))]
            let base = base
                .window_control_area(WindowControlArea::Drag)
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _, window, _| window.start_window_move()),
                )
                .on_click(cx.listener(|_, event: &ClickEvent, window, _| {
                    if event.click_count() >= 2 {
                        window.zoom_window();
                    }
                }));
            base
        };

        // Caption controls: minimise, maximise/restore, close.
        // 46×40 hitboxes hugging the right edge of the strip, styled
        // to match the rest of the chrome (ink_dim foreground on
        // transparent, frost-10 hover; close hover-red).
        // `window_control_area(...)` annotates the hitbox so Windows
        // snap-layouts and the platform's accessibility tree know
        // which native control each button maps to.
        let ink = theme::ink(&theme);
        let ink_dim = theme::ink_dim(&theme);
        let frost_hover = theme::frost_10(&theme);
        let close_hover_bg = theme::danger(&theme);
        let caption_base = move |id: &'static str, area: WindowControlArea, glyph: &'static str| {
            div()
                .id(id)
                .h(px(40.0))
                .w(px(46.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.0))
                .text_color(ink_dim)
                .cursor_pointer()
                .window_control_area(area)
                .child(glyph)
        };

        // On Windows the `WindowControlArea::*` annotations make
        // gpui's hit-test return `HTMINBUTTON` / `HTMAXBUTTON` /
        // `HTCLOSE`, and gpui's NC-mouse-up handler does the right
        // thing natively (toggle on max, post WM_CLOSE on close,
        // SW_MINIMIZE on min). Wiring our own `on_mouse_down`
        // handlers here would race the native flow — and for the max
        // button specifically would *break* the toggle, since
        // `zoom_window()` on Windows is `SW_MAXIMIZE`-only with no
        // restore path.
        //
        // On non-Windows targets we still need explicit handlers
        // because there's no equivalent NC-button native handling.
        let minimize_btn = caption_base("titlebar-min", WindowControlArea::Min, "—")
            .hover(move |s| s.bg(frost_hover).text_color(ink));
        let maximize_btn = caption_base("titlebar-max", WindowControlArea::Max, "▢")
            .hover(move |s| s.bg(frost_hover).text_color(ink));
        let close_btn = caption_base("titlebar-close", WindowControlArea::Close, "✕")
            .hover(move |s| s.bg(close_hover_bg).text_color(ink));
        #[cfg(not(target_os = "windows"))]
        let minimize_btn = minimize_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, window, _| window.minimize_window()),
        );
        #[cfg(not(target_os = "windows"))]
        let maximize_btn = maximize_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, window, _| window.zoom_window()),
        );
        #[cfg(not(target_os = "windows"))]
        let close_btn = close_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, window, _| window.remove_window()),
        );

        // Split-right caption button — sits left of the min/max/close
        // trio. Mirrors the C# build's titlebar split control. Pure
        // client-area button (no `WindowControlArea` annotation), so
        // `on_mouse_down` runs the same way it does on a tab.
        let split_btn = div()
            .id("titlebar-split")
            .h(px(40.0))
            .w(px(46.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.0))
            .text_color(ink_dim)
            .cursor_pointer()
            .hover(move |s| s.bg(frost_hover).text_color(ink))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.split_right(window, cx);
                }),
            )
            // Two thin verticals + a divider — pure-shape so we don't
            // need a glyph font. Aligns visually with C#'s
            // `Ctx.Icon.SplitGroup`.
            .child(
                div()
                    .w(px(14.0))
                    .h(px(12.0))
                    .flex()
                    .flex_row()
                    .gap(px(2.0))
                    .child(
                        div()
                            .w(px(6.0))
                            .h_full()
                            .border_1()
                            .border_color(ink_dim)
                            .rounded_sm(),
                    )
                    .child(
                        div()
                            .w(px(6.0))
                            .h_full()
                            .border_1()
                            .border_color(ink_dim)
                            .rounded_sm(),
                    ),
            );

        // Brand mark — top-left of the tab strip. Pure-shape port of
        // the C# splash's `.brand-mark` (accent rounded square with a
        // small black inset square in the upper-right). Sized to fit
        // the 40 px strip with breathing room. Decorative for now;
        // clicking it does nothing — it's primarily a visual anchor
        // and the same affordance the C# build's splash uses for
        // brand recognition. The drag region above is what actually
        // moves the window when the user grabs the title bar.
        let accent_clr = theme::accent(&theme);
        let brand_mark = div()
            .w(px(40.0))
            .h(px(40.0))
            .flex()
            .items_center()
            .justify_center()
            // The mark itself sits inside a flex container so the
            // rounded square stays centred regardless of the strip's
            // exact height. `window_control_area(Drag)` makes the
            // surrounding 40×40 cell draggable like the rest of the
            // bar — clicking the mark itself starts a window move
            // since the inner shape doesn't intercept clicks.
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .w(px(22.0))
                    .h(px(22.0))
                    .rounded(px(5.0))
                    .bg(accent_clr)
                    .flex()
                    .flex_row()
                    .items_start()
                    .justify_end()
                    .p(px(4.0))
                    .child(
                        div()
                            .w(px(7.0))
                            .h(px(7.0))
                            .rounded(px(2.0))
                            .bg(gpui::black()),
                    ),
            );

        // Build the per-group strip sections + per-group panes in one
        // pass so the tab strip and the body row stay in lock-step (a
        // group is one column; both layers must agree on which group
        // is in which column).
        let group_count = groups_meta.len();
        let mut strip_sections: Vec<gpui::AnyElement> = Vec::with_capacity(group_count * 2);
        let mut group_panes: Vec<gpui::AnyElement> = Vec::with_capacity(group_count * 2);
        let divider_color = theme::divider(&theme);
        for (col_idx, gmeta) in groups_meta.into_iter().enumerate() {
            if col_idx > 0 {
                // The split lives between groups[col_idx-1] (left) and
                // groups[col_idx] (right). The strip layer gets a
                // pure-visual 1 px divider; the work-area layer gets a
                // wider interactive splitter so the user can grab it
                // without pixel-perfect aiming.
                let split_idx = col_idx - 1;
                strip_sections.push(
                    div()
                        .w_px()
                        .h_full()
                        .bg(divider_color)
                        .into_any_element(),
                );
                let splitter = div()
                    .id(("group-splitter", split_idx as u64))
                    .w(px(SPLITTER_HIT_WIDTH))
                    .h_full()
                    .flex()
                    .flex_row()
                    .justify_center()
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.begin_splitter_drag(split_idx, event.position.x, window, cx);
                        }),
                    )
                    // 1 px painted line, vertically centred inside the
                    // 6 px hit-target. Keeps the visual identical to
                    // the strip divider above so columns line up.
                    .child(div().w_px().h_full().bg(divider_color));
                group_panes.push(splitter.into_any_element());
            }
            let (strip, pane) = self.render_group(&theme, &gmeta, cx);
            strip_sections.push(strip.into_any_element());
            group_panes.push(pane.into_any_element());
        }

        // Sidebar-width spacer between the 40 px brand mark and the
        // first strip section so each strip column starts at the same
        // x-coordinate as the matching pane below — without this, the
        // strip dividers slide left of the work-area dividers and the
        // user sees mis-aligned columns. The spacer also serves as
        // titlebar drag region above the sidebar (window-control area
        // + start_window_move) so the user can grab the bar there.
        // Same Windows / non-Windows split as the main drag region:
        // on Windows we let `HTCAPTION` do the work natively (no
        // `start_window_move`), elsewhere we fire it explicitly.
        let sidebar_spacer_w = px(SIDEBAR_WIDTH) - px(40.0);
        let sidebar_spacer = {
            let base = div()
                .id("titlebar-sidebar-spacer")
                .w(sidebar_spacer_w)
                .h(px(40.0))
                .window_control_area(WindowControlArea::Drag);
            #[cfg(not(target_os = "windows"))]
            let base = base.on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, window, _| window.start_window_move()),
            );
            base
        };

        let tab_strip = div()
            .h(px(40.0))
            .flex()
            .flex_row()
            .border_b_1()
            .border_color(theme::divider(&theme))
            .bg(theme::elevated(&theme))
            .child(brand_mark)
            .child(sidebar_spacer)
            // Per-group strip sections share the available width
            // equally for now (each is `flex_grow`). Per-group weight
            // sliders + drag-resize land in the follow-up PR.
            .child(
                div()
                    .flex_grow()
                    .flex()
                    .flex_row()
                    .h_full()
                    .children(strip_sections),
            )
            .child(drag_region)
            .child(split_btn)
            .child(minimize_btn)
            .child(maximize_btn)
            .child(close_btn);

        let work_area = div()
            .flex_grow()
            .flex()
            .flex_row()
            .children(group_panes);

        let main_row = div()
            .flex_grow()
            .flex()
            .flex_row()
            .child(self.sidebar.clone())
            .child(work_area);

        // While a splitter drag is in flight we listen for mouse
        // moves anywhere in the window (the cursor commonly leaves
        // the 6 px hit-target during a fast drag) and clamp them
        // against the original snapshot. Mouse up — anywhere —
        // commits the new weights. We attach both handlers
        // unconditionally because gpui doesn't have an `is_some()`-
        // gated handler primitive; the closures are tiny and bail
        // when no drag is in flight.
        div()
            .key_context("AppShell")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.splitter_drag.is_some() {
                    this.update_splitter_drag(event.position.x, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.end_splitter_drag(cx);
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::canvas(&theme))
            .text_color(theme::ink(&theme))
            .child(tab_strip)
            .child(main_row)
    }
}

/// Frame-local snapshot for one tab inside a group. Owned strings so
/// the listener closures can `move` the title without keeping a borrow
/// on `self.groups`.
struct TabRenderData {
    tab_idx: usize,
    tab_id: u64,
    title: SharedString,
}

/// Frame-local snapshot for one group. We snapshot `Entity<TerminalView>`
/// for the active tab so the body row can render it without
/// re-borrowing `self.groups` after `groups_meta` is moved into the
/// per-group render loop.
struct GroupRenderData {
    group_idx: usize,
    group_id: u64,
    active_tab: usize,
    is_focused: bool,
    /// Flex weight for this group's column; mapped to `flex_grow` on
    /// both the strip section and the pane below so they stay
    /// column-aligned. `1.0` = equal share, `1.5` = 1.5× a sibling at
    /// `1.0`. Snapshotted from `AppShell.group_weights` at render time.
    weight: f32,
    tabs: Vec<TabRenderData>,
    active_terminal: Option<Entity<TerminalView>>,
}

impl AppShell {
    /// Build one group's tab strip section + body pane. Returned as a
    /// pair so `render` can interleave dividers between adjacent
    /// groups while keeping the strip and the pane below it in the
    /// same column.
    fn render_group(
        &self,
        theme: &Arc<Theme>,
        gmeta: &GroupRenderData,
        cx: &mut Context<Self>,
    ) -> (gpui::Stateful<gpui::Div>, gpui::Stateful<gpui::Div>) {
        let group_idx = gmeta.group_idx;
        let group_id = gmeta.group_id;
        let active_tab = gmeta.active_tab;
        let is_focused = gmeta.is_focused;
        let frost_10 = theme::frost_10(theme);
        let frost_20 = theme::frost_20(theme);
        let canvas = theme::canvas(theme);
        let elevated = theme::elevated(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let accent = theme::accent(theme);
        let divider = theme::divider(theme);

        let tabs = gmeta.tabs.iter().map(|tmeta| {
            let tab_idx = tmeta.tab_idx;
            let tab_id = tmeta.tab_id;
            let title = tmeta.title.clone();
            let active = tab_idx == active_tab && is_focused;
            // Tab styling follows the same shape as the single-group
            // version — active tab gets a canvas-coloured "card" with
            // an accent top border. In an unfocused group the active
            // tab still shows as the selected card (so the user sees
            // which tab will resume on focus) but without the accent
            // top border.
            let card = tab_idx == active_tab;
            let bg = if card { canvas } else { gpui::transparent_black() };
            let text_color = if active { ink } else { ink_dim };
            let top_border = if active { accent } else { gpui::transparent_black() };
            let status_dot = if active {
                theme::status_running()
            } else {
                ink_ghost
            };
            div()
                .id(("tab", tab_id))
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
                        this.activate_tab(group_idx, tab_idx, window, cx);
                    }),
                )
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
                        .id(("close", tab_id))
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
                                this.close_tab(group_idx, tab_idx, window, cx);
                            }),
                        )
                        .child("×"),
                )
        });

        // Per-strip "+" button — same height/width as the cold-start
        // singleton's, but lives inside the strip section so each group
        // gets its own. Click spawns into *this* group regardless of
        // which one is currently focused.
        let new_tab_frost = frost_10;
        let new_tab_accent = accent;
        let new_tab_button = div()
            .id(("new-tab", group_id))
            .h(px(40.0))
            .w(px(40.0))
            .flex()
            .items_center()
            .justify_center()
            .text_color(ink_dim)
            .text_size(px(18.0))
            .cursor_pointer()
            .hover(move |s| s.bg(new_tab_frost).text_color(new_tab_accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    // Spawning routes through the currently-focused
                    // group, so focus this one first to make the click
                    // do what the user expects (spawn here, not
                    // wherever focus happened to be).
                    this.focus_group(group_idx, window, cx);
                    this.spawn_tab(window, cx);
                }),
            )
            .child("+");

        // Strip section: tabs + "+" button. Click anywhere in the
        // remaining whitespace focuses the group so the next Ctrl+T
        // lands here. The trailing `flex_grow` filler captures the
        // empty area to the right of the rightmost tab.
        //
        // `flex_grow` is set via `style().flex_grow = Some(weight)`
        // because gpui's chainable `.flex_grow()` only sets the value
        // to 1.0 — we need arbitrary weights for the column layout.
        let mut strip = div()
            .id(("group-strip", group_id))
            .h_full()
            .flex()
            .flex_row()
            .flex_shrink()
            .bg(elevated)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.focus_group(group_idx, window, cx);
                }),
            )
            .children(tabs)
            .child(new_tab_button)
            // Empty trailing region — gives the user something to
            // click for "focus this group" without hitting a tab.
            .child(div().flex_grow().h_full());
        strip.style().flex_grow = Some(gmeta.weight);
        strip.style().flex_basis = Some(gpui::Length::Definite(px(0.0).into()));

        // Pane: active tab's terminal, or a black void when the group
        // has no tabs (split-right + Ctrl+T-not-yet-pressed). 2 px
        // accent rail at the top of the focused group's pane mirrors
        // the C# `EditorGroupView` `IsFocused` cue.
        let rail_color = if is_focused { accent } else { divider };
        let body_inner: gpui::AnyElement = if let Some(term) = gmeta.active_terminal.as_ref() {
            div().size_full().child(term.clone()).into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(ink_ghost)
                .text_size(px(12.0))
                .child("Empty group · press Ctrl+Shift+T to open a tab")
                .into_any_element()
        };
        let mut pane = div()
            .id(("group-pane", group_id))
            .h_full()
            .flex()
            .flex_col()
            .flex_shrink()
            .border_t_2()
            .border_color(rail_color)
            .bg(canvas)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.focus_group(group_idx, window, cx);
                }),
            )
            .child(body_inner);
        pane.style().flex_grow = Some(gmeta.weight);
        pane.style().flex_basis = Some(gpui::Length::Definite(px(0.0).into()));

        (strip, pane)
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
