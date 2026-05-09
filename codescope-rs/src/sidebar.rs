//! Left-rail PROJECTS sidebar.
//!
//! Single-purpose view: lists every entry in the loaded
//! [`codescope_core::ProjectsConfig`] and lets the user select one.
//! The `+` button kicks off a directory picker and, on success,
//! appends a new project and persists `projects.json`. Right-click on
//! a project opens a context menu mirroring the C# build's
//! `BuildProjectMenu` — Reveal / Copy path / Open in Windows
//! Terminal / Remove project today, with `New worktree from branch…`
//! landing once the input dialog primitive exists.
//!
//! Layout (240 px wide):
//!
//! ```text
//! ┌───────────────┐
//! │ PROJECTS    + │ ← heading + add (file picker)
//! ├───────────────┤
//! │ filter…       │ ← (placeholder, wired later)
//! ├───────────────┤
//! │ ▍ project A   │ ← active = accent rail + frost bg
//! │   project B   │
//! │   project C   │
//! └───────────────┘
//! ```

use std::process::Command;
use std::sync::Arc;

use codescope_core::{AppPaths, LayoutState, Project, ProjectsConfig, Theme};
use gpui::{
    ClipboardItem, Context, Corner, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, PathPromptOptions, Pixels, Point, Render, SharedString, Styled, Window,
    anchored, deferred, div, point, px,
};

use crate::theme;

/// Width of the sidebar pane. The C# build uses 240 with a
/// resizable splitter; we keep it fixed for now and add the splitter
/// when there's a second column to balance against.
pub const SIDEBAR_WIDTH: f32 = 240.0;

/// Open right-click context menu state. `None` when no menu is
/// showing. The position is in window coordinates so we can hand it
/// straight to [`anchored`] without recomputing on render.
struct OpenMenu {
    project_idx: usize,
    position: Point<Pixels>,
}

/// Click handler for a context-menu row. Boxed so we can stash it in
/// the closure passed to `cx.listener` without leaking the helper's
/// generic-over-fn shape into every menu-row construction site.
type MenuItemAction = Box<dyn Fn(&mut Sidebar, &mut Context<Sidebar>) + 'static>;

pub struct Sidebar {
    projects: ProjectsConfig,
    /// Index of the currently-selected project. `None` when no
    /// projects exist yet.
    selected: Option<usize>,
    theme: Arc<Theme>,
    /// Where `projects.json` and `layout.json` live. Threaded in so
    /// add/remove + selection changes can persist without re-detecting
    /// the env.
    paths: Arc<AppPaths>,
    /// In-memory copy of `layout.json` — kept in sync as the user
    /// changes selection so a save-on-change writes out the full
    /// (correct) struct, not just the field we touched.
    layout: LayoutState,
    /// Currently-open project context menu, if any.
    menu: Option<OpenMenu>,
}

impl Sidebar {
    pub fn new(
        projects: ProjectsConfig,
        layout: LayoutState,
        theme: Arc<Theme>,
        paths: Arc<AppPaths>,
    ) -> Self {
        // Restore last-opened project if it still exists. Falls back
        // to the first project when the saved id is gone (project
        // removed between sessions) or absent (first launch).
        let selected = match layout.selected_project_id.as_deref() {
            Some(id) => projects.projects.iter().position(|p| p.id == id),
            None => None,
        }
        .or_else(|| (!projects.projects.is_empty()).then_some(0));
        Self { projects, selected, theme, paths, layout, menu: None }
    }

    pub fn select(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.projects.projects.len() || self.selected == Some(idx) {
            // Out-of-range or re-clicking the active row — no-op,
            // skip the synchronous `layout.json` write.
            return;
        }
        self.selected = Some(idx);
        self.layout.selected_project_id =
            Some(self.projects.projects[idx].id.clone());
        self.save_layout();
        cx.notify();
    }

    /// Persist `layout.json` after selection / sidebar-visibility
    /// changes. No debounce — selection is user-driven and slow.
    fn save_layout(&self) {
        if let Err(err) = self.layout.save(&self.paths) {
            eprintln!("warning: failed to save layout.json: {err:#}");
        }
    }

    /// The project the user currently has selected, if any. AppShell
    /// reads this when spawning a new tab so the terminal lands in
    /// the right cwd.
    pub fn active_project(&self) -> Option<&Project> {
        self.selected.and_then(|idx| self.projects.projects.get(idx))
    }

    /// Apply a fresh theme snapshot. Called by the AppShell when the
    /// user changes themes — the sidebar redraws on the next frame.
    #[allow(dead_code)]
    pub fn apply_theme(&mut self, theme: Arc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Open the platform "pick a folder" dialog. On confirm, hand the
    /// path to [`Self::add_project`] which writes `projects.json`
    /// before mutating in-memory state, so a save failure leaves both
    /// the disk and the UI in their previous (consistent) state.
    pub fn open_add_project_picker(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match rx.await {
                Ok(Ok(Some(paths))) => paths,
                // `Ok(None)` = user cancelled, `Ok(Err(...))` = picker
                // failed to open (Linux). Both end the flow silently
                // — the user already sees what happened on screen.
                Ok(Ok(None)) => return,
                Ok(Err(err)) => {
                    eprintln!("warning: file picker failed: {err:#}");
                    return;
                }
                Err(_) => return,
            };
            if let Some(path) = paths.into_iter().next() {
                let path_str = path.to_string_lossy().into_owned();
                let _ = this.update(cx, |this, cx| {
                    this.add_project(path_str, cx);
                });
            }
        })
        .detach();
    }

    /// Open the project context menu at `position` (window coords)
    /// for the project at `idx`. No-op if the index is out of range.
    fn open_project_menu(
        &mut self,
        idx: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        if idx >= self.projects.projects.len() {
            return;
        }
        self.menu = Some(OpenMenu { project_idx: idx, position });
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    /// Reveal the project's working tree in the OS file browser.
    /// Mirrors the C# `RevealInExplorerCommand`.
    fn reveal_in_explorer(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.projects.get(idx) else { return };
        let path = project.path.clone();
        // Spawn detached so a slow shell-extension doesn't stall the UI
        // thread. We don't care about the exit status — the user sees
        // the result on their desktop.
        #[cfg(target_os = "windows")]
        let result = Command::new("explorer.exe").arg(&path).spawn();
        #[cfg(target_os = "macos")]
        let result = Command::new("open").arg(&path).spawn();
        #[cfg(all(unix, not(target_os = "macos")))]
        let result = Command::new("xdg-open").arg(&path).spawn();
        if let Err(err) = result {
            eprintln!("warning: failed to reveal {path}: {err:#}");
        }
        self.close_menu(cx);
    }

    /// `wt -d <path>` — opens Windows Terminal with its starting
    /// directory pinned to the project root. Mirrors C#'s
    /// `OpenInWindowsTerminalCommand`. No-op on non-Windows.
    fn open_in_windows_terminal(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.projects.get(idx) else { return };
        let path = project.path.clone();
        #[cfg(target_os = "windows")]
        {
            if let Err(err) = Command::new("wt").args(["-d", &path]).spawn() {
                eprintln!("warning: failed to launch Windows Terminal: {err:#}");
            }
        }
        #[cfg(not(target_os = "windows"))]
        {
            let _ = path;
            eprintln!("info: 'Open in Windows Terminal' is Windows-only");
        }
        self.close_menu(cx);
    }

    /// Copy the project's absolute path to the system clipboard.
    /// Mirrors C#'s `CopyPathCommand` (Ctrl+Alt+C in the C# build —
    /// keybinding wiring lands when the global shortcut layer does).
    fn copy_path(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.projects.get(idx) else { return };
        cx.write_to_clipboard(ClipboardItem::new_string(project.path.clone()));
        self.close_menu(cx);
    }

    /// Drop a project from the sidebar list and persist `projects.json`.
    /// Does **not** touch anything on disk — the working tree stays
    /// where it is; the user just removes it from CodeScope's view.
    /// Save-then-commit ordering matches `add_project` so a write
    /// failure leaves both disk and UI in their previous state.
    fn remove_project(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.projects.projects.len() {
            return;
        }
        let prev_selected_id = self.layout.selected_project_id.clone();
        let mut next = self.projects.clone();
        next.projects.remove(idx);
        if let Err(err) = next.save(&self.paths) {
            eprintln!("warning: failed to save projects.json: {err:#}");
            return;
        }
        self.projects = next;
        // Selection housekeeping: if we just removed the selected
        // project, fall back to the previous row (or `None` when the
        // list is empty). Otherwise shift the cursor left when an
        // earlier row was removed so it keeps pointing at the same
        // project.
        self.selected = match self.selected {
            Some(sel) if sel == idx => {
                if self.projects.projects.is_empty() { None } else { Some(sel.min(self.projects.projects.len() - 1)) }
            }
            Some(sel) if sel > idx => Some(sel - 1),
            other => other,
        };
        self.layout.selected_project_id =
            self.selected.and_then(|i| self.projects.projects.get(i).map(|p| p.id.clone()));
        // Only persist layout when the persisted id actually changed —
        // i.e. when the removed project was the active one. Removing
        // a row before/after the active one leaves the id intact.
        if self.layout.selected_project_id != prev_selected_id {
            self.save_layout();
        }
        self.close_menu(cx);
    }

    /// Append a project at `path` and persist. Newly-added project
    /// becomes the selection — the user just chose it, so dropping
    /// them straight into it is what they expect.
    ///
    /// Save-then-commit ordering: we build a candidate `ProjectsConfig`,
    /// write it to disk, and only swap it into `self.projects` (and
    /// touch `selected` / `layout.json`) once the write succeeds. A
    /// failed write therefore leaves both disk and UI in their
    /// previous consistent state, instead of producing an in-memory
    /// row that disappears on relaunch — and (worse) a `layout.json`
    /// pointing at a project id that never made it to `projects.json`.
    pub fn add_project(&mut self, path: String, cx: &mut Context<Self>) {
        // Refuse exact duplicates by path. Two rows pointing at the
        // same directory would let a user "add" the same project
        // twice and then wonder why both rows behave identically.
        if let Some(idx) = self.projects.projects.iter().position(|p| p.path == path) {
            self.select(idx, cx);
            return;
        }
        let project = Project::new(path);
        let new_id = project.id.clone();
        // Clone-then-save: failure leaves `self.projects` untouched.
        let mut next = self.projects.clone();
        next.projects.push(project);
        if let Err(err) = next.save(&self.paths) {
            eprintln!("warning: failed to save projects.json: {err:#}");
            return;
        }
        // Disk is committed; now mirror the change in memory.
        self.projects = next;
        let new_idx = self.projects.projects.len() - 1;
        self.selected = Some(new_idx);
        self.layout.selected_project_id = Some(new_id);
        self.save_layout();
        cx.notify();
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let selected = self.selected;
        let rows: Vec<_> = self
            .projects
            .projects
            .iter()
            .enumerate()
            .map(|(idx, p)| (idx, p.id.clone(), SharedString::from(p.name.clone())))
            .collect();

        let heading = div()
            .h(px(40.0))
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .text_size(px(11.0))
            .text_color(theme::ink_muted(&theme))
            .child(div().flex_grow().child("PROJECTS"))
            .child(
                div()
                    .id("sidebar-add")
                    .w(px(20.0))
                    .h(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .text_color(theme::ink_ghost(&theme))
                    .cursor_pointer()
                    .hover({
                        let frost = theme::frost_10(&theme);
                        let ink = theme::ink(&theme);
                        move |s| s.bg(frost).text_color(ink)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.open_add_project_picker(window, cx);
                        }),
                    )
                    .child("+"),
            );

        let empty_state = if self.projects.projects.is_empty() {
            Some(
                div()
                    .px_3()
                    .py_4()
                    .text_size(px(12.0))
                    .text_color(theme::ink_ghost(&theme))
                    .child("No projects yet. Click + to add one."),
            )
        } else {
            None
        };

        let project_rows = rows.into_iter().map(|(idx, id, name)| {
            let active = selected == Some(idx);
            let bg = if active {
                theme::frost_10(&theme)
            } else {
                gpui::transparent_black()
            };
            let rail = if active {
                theme::accent(&theme)
            } else {
                gpui::transparent_black()
            };
            let text_color = if active {
                theme::ink(&theme)
            } else {
                theme::ink_dim(&theme)
            };
            let frost_hover = theme::frost_10(&theme);
            let ink_hover = theme::ink(&theme);

            div()
                .id(("project", id_hash(&id)))
                .h(px(32.0))
                .flex()
                .flex_row()
                .items_center()
                .pr_3()
                .border_l_2()
                .border_color(rail)
                .pl(px(10.0)) // 12px - 2px border = 10
                .bg(bg)
                .text_color(text_color)
                .text_size(px(13.0))
                .cursor_pointer()
                .hover(move |s| {
                    if active { s } else { s.bg(frost_hover).text_color(ink_hover) }
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| this.select(idx, cx)),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.open_project_menu(idx, event.position, cx);
                    }),
                )
                .child(div().flex_grow().truncate().child(name))
        });

        let mut body = div()
            .flex()
            .flex_col()
            .py_1()
            .children(project_rows);
        if let Some(es) = empty_state {
            body = body.child(es);
        }

        let menu_overlay = self.menu.as_ref().and_then(|menu| {
            let project = self.projects.projects.get(menu.project_idx)?;
            Some(self.render_project_menu(menu.project_idx, menu.position, project, &theme, cx))
        });

        let mut root = div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::elevated(&theme))
            .border_r_1()
            .border_color(theme::divider(&theme))
            .child(heading)
            .child(div().h_px().bg(theme::divider(&theme)))
            .child(body);
        if let Some(overlay) = menu_overlay {
            root = root.child(overlay);
        }
        root
    }
}

impl Sidebar {
    /// Build the floating project context menu. Anchored to the
    /// click position and `deferred` so it paints over the rest of
    /// the chrome instead of being clipped by the sidebar's bounds.
    /// Click outside (anywhere in the window) dismisses via
    /// `on_mouse_down_out`.
    fn render_project_menu(
        &self,
        idx: usize,
        position: Point<Pixels>,
        project: &Project,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let header_label: SharedString = project.name.clone().into();
        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let frost = theme::frost_10(theme);
        let danger = theme::danger(theme);

        let item = |id: &'static str,
                    label: &'static str,
                    danger_row: bool,
                    on_click: MenuItemAction|
         -> gpui::Stateful<gpui::Div> {
            let base_color = if danger_row { danger } else { ink_dim };
            let hover_color = if danger_row { danger } else { ink };
            let frost_hover = frost;
            div()
                .id(id)
                .h(px(28.0))
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                .text_size(px(13.0))
                .text_color(base_color)
                .cursor_pointer()
                .hover(move |s| s.bg(frost_hover).text_color(hover_color))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        on_click(this, cx);
                    }),
                )
                .child(label)
        };

        let menu_body = div()
            .flex()
            .flex_col()
            .py_1()
            .min_w(px(220.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_md()
            .shadow_lg()
            // Header — non-interactive, mirrors the C# `BuildContextHeader`
            // (project name dimmed, "project" qualifier).
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(11.0))
                    .text_color(ink_ghost)
                    .child(div().text_color(ink).text_size(px(13.0)).truncate().child(header_label))
                    .child(div().child("project")),
            )
            .child(div().h_px().bg(divider).my_1())
            // "New worktree from branch…" lands once the input-dialog
            // primitive exists; rendering it disabled now keeps the
            // menu shape stable so muscle memory carries over.
            .child(
                div()
                    .id("menu-new-worktree-disabled")
                    .h(px(28.0))
                    .px_3()
                    .flex()
                    .flex_row()
                    .items_center()
                    .text_size(px(13.0))
                    .text_color(ink_ghost)
                    .child("New worktree from branch…"),
            )
            .child(div().h_px().bg(divider).my_1())
            .child(item(
                "menu-reveal",
                reveal_in_file_browser_label(),
                false,
                Box::new(move |this, cx| this.reveal_in_explorer(idx, cx)),
            ))
            // "Open in Windows Terminal" is genuinely Windows-only —
            // `wt.exe` doesn't exist on macOS / Linux. Hide the row
            // entirely on other platforms instead of shipping a
            // misleading no-op. `.children(Option<_>)` yields 0 or 1
            // child without splitting the chain.
            .children(cfg!(target_os = "windows").then(|| {
                item(
                    "menu-wt",
                    "Open in Windows Terminal",
                    false,
                    Box::new(move |this, cx| this.open_in_windows_terminal(idx, cx)),
                )
            }))
            .child(item(
                "menu-copy-path",
                "Copy path",
                false,
                Box::new(move |this, cx| this.copy_path(idx, cx)),
            ))
            .child(div().h_px().bg(divider).my_1())
            .child(item(
                "menu-remove",
                "Remove project",
                true,
                Box::new(move |this, cx| this.remove_project(idx, cx)),
            ))
            // Click on the menu itself shouldn't bubble out and trigger
            // the dismiss handler we install below.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_menu(cx)));

        // `deferred` paints the menu after the rest of the frame so it
        // overlays the tab strip / terminal area instead of being
        // clipped to the 240 px sidebar column. `anchored` snaps it
        // to a window edge if the click happens close to one.
        deferred(
            anchored()
                .position(point(position.x, position.y))
                .anchor(Corner::TopLeft)
                .snap_to_window_with_margin(px(8.0))
                .child(menu_body),
        )
    }
}

/// Platform-appropriate label for the "Reveal in <native file browser>"
/// menu row. Mirrors the underlying spawn target in
/// [`Sidebar::reveal_in_explorer`] (`explorer.exe` / `open` /
/// `xdg-open`) so the UI matches what actually happens. The C# build
/// is Windows-only and uses "Reveal in File Explorer" verbatim — we
/// keep that string on Windows and pick a native equivalent
/// elsewhere instead of shipping a Windows-centric label on macOS /
/// Linux.
fn reveal_in_file_browser_label() -> &'static str {
    if cfg!(target_os = "windows") {
        "Reveal in File Explorer"
    } else if cfg!(target_os = "macos") {
        "Reveal in Finder"
    } else {
        "Reveal in File Manager"
    }
}

/// Cheap hash for use as a gpui element id derived from a string id.
/// `id` itself isn't `Hash` for gpui's id needs but `(static_str, u64)`
/// is — so we shrink the project id to a u64.
fn id_hash(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}
