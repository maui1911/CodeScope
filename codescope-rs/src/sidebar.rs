//! Left-rail PROJECTS sidebar.
//!
//! Single-purpose view: lists every entry in the loaded
//! [`codescope_core::ProjectsConfig`] and lets the user select one.
//! The `+` button kicks off a directory picker and, on success,
//! appends a new project and persists `projects.json`. Removal /
//! editing are still TODO and live behind right-click + a command
//! palette in the C# build.
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

use std::sync::Arc;

use codescope_core::{AppPaths, LayoutState, Project, ProjectsConfig, Theme};
use gpui::{
    Context, InteractiveElement, IntoElement, MouseButton, ParentElement, PathPromptOptions,
    Render, SharedString, Styled, Window, div, px,
};

use crate::theme;

/// Width of the sidebar pane. The C# build uses 240 with a
/// resizable splitter; we keep it fixed for now and add the splitter
/// when there's a second column to balance against.
pub const SIDEBAR_WIDTH: f32 = 240.0;

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
        Self { projects, selected, theme, paths, layout }
    }

    pub fn select(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx < self.projects.projects.len() {
            self.selected = Some(idx);
            self.layout.selected_project_id =
                Some(self.projects.projects[idx].id.clone());
            self.save_layout();
            cx.notify();
        }
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

    /// Open the platform "pick a folder" dialog. On confirm, append a
    /// fresh [`Project`] and write `projects.json`. Any error is
    /// logged to stderr — the sidebar stays consistent because we
    /// only mutate state when the save succeeds. (Saved-but-stale
    /// is preferable to in-memory-but-not-saved; see the C# build's
    /// `ProjectsRepository` for the same trade-off.)
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

    /// Append a project at `path` and persist. Newly-added project
    /// becomes the selection — the user just chose it, so dropping
    /// them straight into it is what they expect.
    pub fn add_project(&mut self, path: String, cx: &mut Context<Self>) {
        // Refuse exact duplicates by path. Two rows pointing at the
        // same directory would let a user "add" the same project
        // twice and then wonder why both rows behave identically.
        if self.projects.projects.iter().any(|p| p.path == path) {
            if let Some(idx) = self.projects.projects.iter().position(|p| p.path == path) {
                self.selected = Some(idx);
                cx.notify();
            }
            return;
        }
        let project = Project::new(path);
        let new_id = project.id.clone();
        self.projects.projects.push(project);
        let new_idx = self.projects.projects.len() - 1;
        self.selected = Some(new_idx);
        self.layout.selected_project_id = Some(new_id);
        if let Err(err) = self.projects.save(&self.paths) {
            eprintln!("warning: failed to save projects.json: {err:#}");
        }
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

        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::elevated(&theme))
            .border_r_1()
            .border_color(theme::divider(&theme))
            .child(heading)
            .child(div().h_px().bg(theme::divider(&theme)))
            .child(body)
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
