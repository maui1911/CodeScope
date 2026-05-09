//! Left-rail PROJECTS sidebar.
//!
//! Single-purpose view: lists every entry in the loaded
//! [`codescope_core::ProjectsConfig`] and lets the user select one.
//! No "add project" / "remove project" interactions yet — that lives
//! one step further along the roadmap, behind a `+` action and a
//! command-palette entry.
//!
//! Layout (240 px wide):
//!
//! ```text
//! ┌───────────────┐
//! │ PROJECTS    + │ ← heading + add (placeholder)
//! ├───────────────┤
//! │ filter…       │ ← (placeholder, wired next session)
//! ├───────────────┤
//! │ ▍ project A   │ ← active = accent rail + frost bg
//! │   project B   │
//! │   project C   │
//! └───────────────┘
//! ```

use std::sync::Arc;

use codescope_core::{ProjectsConfig, Theme};
use gpui::{
    Context, InteractiveElement, IntoElement, MouseButton, ParentElement, Render, SharedString,
    Styled, Window, div, px,
};

use crate::theme;

/// Width of the sidebar pane. The C# build uses 240 with a
/// resizable splitter; we keep it fixed for now and add the splitter
/// when there's a second column to balance against.
pub const SIDEBAR_WIDTH: f32 = 240.0;

pub struct Sidebar {
    projects: Arc<ProjectsConfig>,
    /// Index of the currently-selected project. `None` when no
    /// projects exist yet.
    selected: Option<usize>,
    theme: Arc<Theme>,
}

impl Sidebar {
    pub fn new(projects: Arc<ProjectsConfig>, theme: Arc<Theme>) -> Self {
        let selected = (!projects.projects.is_empty()).then_some(0);
        Self { projects, selected, theme }
    }

    pub fn select(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx < self.projects.projects.len() {
            self.selected = Some(idx);
            cx.notify();
        }
    }

    /// Apply a fresh theme snapshot. Called by the AppShell when the
    /// user changes themes — the sidebar redraws on the next frame.
    #[allow(dead_code)]
    pub fn apply_theme(&mut self, theme: Arc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
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
            // Placeholder "+" button — wired up in a follow-up commit.
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
                    .hover({
                        let frost = theme::frost_10(&theme);
                        let ink = theme::ink(&theme);
                        move |s| s.bg(frost).text_color(ink)
                    })
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
