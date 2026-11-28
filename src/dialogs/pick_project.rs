//! Needs-you dialog for an item several projects could work on (ADR-0027): one row per project
//! with a remote for its repository, and whether the pick holds for every item of that
//! repository on the board. *Start* hands the pick to the dispatch, which starts the round.

use std::sync::Arc;

use codescope_core::Theme;
use codescope_core::board::{ColumnRole, effective_mapping};
use gpui::{
    AnyElement, App, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, ParentElement, StatefulInteractiveElement, Styled, Window, anchored, deferred,
    div, point, px,
};

use crate::app::AppShell;
use crate::app::board_dispatch::{ProjectPick, work_after_research};
use crate::dialogs::decide::{checkbox, radio};
use crate::theme;

pub struct PickProjectDialogState {
    pub focus_handle: FocusHandle,
    pub board_url: String,
    pub item_id: String,
    pub number: u32,
    pub repository: String,
    /// The column the item waits in.
    pub column: String,
    /// What *Start* starts: `research`, `implementation` or `a review`.
    pub work: &'static str,
    pub projects: Vec<ProjectChoice>,
    pub chosen: usize,
    pub for_board: bool,
}

/// A project with a remote for the item's repository.
pub struct ProjectChoice {
    pub project_id: String,
    pub name: String,
    pub path: String,
    /// The name of its remote for the repository, such as `upstream`.
    pub remote: String,
}

impl AppShell {
    /// Open the dialog for an item that two or more projects have a remote for; else nothing
    /// happens.
    pub fn open_pick_project_dialog(
        &mut self,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.pick_project_dialog.is_some() {
            return;
        }
        let focus_handle = cx.focus_handle();
        let Some(dialog) = self.pick_project_state(item_id, focus_handle, cx) else {
            return;
        };
        dialog.focus_handle.focus(window);
        // gpui hands the focus to the root's `track_focus` on the click that opened this.
        window.prevent_default();
        self.pick_project_dialog = Some(dialog);
        cx.notify();
    }

    fn pick_project_state(
        &self,
        item_id: &str,
        focus_handle: FocusHandle,
        cx: &App,
    ) -> Option<PickProjectDialogState> {
        let (settings, board) = self.settings.boards.iter().find_map(|settings| {
            let board = self.board.reads_by_url.get(&settings.url)?.last_good.as_ref()?;
            board.items.iter().any(|item| item.id == item_id).then_some((settings, board))
        })?;
        let item = board.items.iter().find(|item| item.id == item_id)?;
        let repository = item.repository.clone()?;
        let column = item.column_id.as_deref().and_then(|id| board.column(id))?;
        let work = match board.role_of(item, &effective_mapping(board, &settings.columns)) {
            Some(ColumnRole::Research) => "research",
            _ => work_after_research(item.kind),
        };
        let projects = self.current_projects(cx);
        let choices: Vec<ProjectChoice> =
            self.projects_with_remote(&repository)
                .into_iter()
                .filter_map(|(remotes, remote)| {
                    let project = projects.project(&remotes.project_id)?;
                    Some(ProjectChoice {
                        project_id: project.id.clone(),
                        name: project.name.clone(),
                        path: project.path.clone(),
                        remote: remote.to_string(),
                    })
                })
                .collect();
        if choices.len() < 2 {
            return None;
        }
        Some(PickProjectDialogState {
            focus_handle,
            board_url: settings.url.clone(),
            item_id: item_id.to_string(),
            number: item.number?,
            repository,
            column: column.name.clone(),
            work,
            projects: choices,
            chosen: 0,
            for_board: true,
        })
    }

    pub fn cancel_pick_project_dialog(&mut self, cx: &mut Context<Self>) {
        if self.pick_project_dialog.take().is_some() {
            cx.notify();
        }
    }

    fn choose_project(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(dialog) = self.pick_project_dialog.as_mut()
            && index < dialog.projects.len()
        {
            dialog.chosen = index;
            cx.notify();
        }
    }

    fn toggle_pick_for_board(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.pick_project_dialog.as_mut() {
            dialog.for_board = !dialog.for_board;
            cx.notify();
        }
    }

    pub fn submit_pick_project_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dialog) = self.pick_project_dialog.take() else {
            return;
        };
        let Some(choice) = dialog.projects.into_iter().nth(dialog.chosen) else {
            return;
        };
        let pick = ProjectPick {
            board_url: dialog.board_url,
            item_id: dialog.item_id,
            repository: dialog.repository,
            project_id: choice.project_id,
            for_board: dialog.for_board,
        };
        self.pick_project(pick, window, cx);
    }

    pub fn render_pick_project_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.pick_project_dialog.as_ref()?;
        let viewport = window.viewport_size();
        let ink = theme::ink(theme);
        let muted = theme::ink_muted(theme);
        let faint = theme::text_faint();
        let divider = theme::divider(theme);
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);
        let frost = theme::frost_10(theme);

        let header = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .px_6()
            .pt_5()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .text_size(px(11.0))
                    .font(theme::font_mono())
                    .child(div().text_color(theme::signal_review()).child("NEEDS YOU"))
                    .child(div().text_color(faint).child(format!(
                        "#{} · {} · in {}",
                        dialog.number, dialog.repository, dialog.column
                    ))),
            )
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(ink)
                    .child("Which project should work on this?"),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(muted)
                    .child(point_at(dialog.projects.len(), &dialog.repository)),
            );

        let projects = div()
            .id("pick-project-list")
            .flex()
            .flex_col()
            .gap_2()
            .px_6()
            .max_h(viewport.height * 0.5)
            .overflow_y_scroll()
            .children(dialog.projects.iter().enumerate().map(|(index, choice)| {
                let on = index == dialog.chosen;
                div()
                    .id(("pick-project", index))
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap(px(10.0))
                    .p_3()
                    .bg(canvas)
                    .rounded(px(8.0))
                    .border_1()
                    .border_color(if on { accent } else { divider })
                    .cursor_pointer()
                    .hover(move |s| s.bg(frost))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.choose_project(index, cx);
                        }),
                    )
                    .child(radio(on, accent, divider))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(ink)
                                    .child(choice.name.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font(theme::font_mono())
                                    .text_color(faint)
                                    .child(format!("{} · {}", choice.path, choice.remote)),
                            ),
                    )
                    .into_any_element()
            }));

        let for_board = div()
            .id("pick-project-for-board")
            .px_6()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .text_size(px(13.0))
            .text_color(muted)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_pick_for_board(cx);
                }),
            )
            .child(checkbox(dialog.for_board, accent, divider, canvas))
            .child(format!(
                "Use this project for {} on this board from now on",
                dialog.repository
            ));

        let later = div()
            .id("pick-project-later")
            .px(px(14.0))
            .py_2()
            .text_size(px(13.0))
            .text_color(ink)
            .border_1()
            .border_color(divider)
            .rounded(px(6.0))
            .flex_shrink_0()
            .cursor_pointer()
            .hover(move |s| s.bg(frost))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.cancel_pick_project_dialog(cx);
                }),
            )
            .child("Not now");
        let start = div()
            .id("pick-project-start")
            .px(px(14.0))
            .py_2()
            .text_size(px(13.0))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(canvas)
            .bg(accent)
            .rounded(px(6.0))
            .flex_shrink_0()
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.submit_pick_project_dialog(window, cx);
                }),
            )
            .child(format!("Start {}", dialog.work));
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_6()
            .py(px(14.0))
            .border_t_1()
            .border_color(divider)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(12.0))
                    .text_color(faint)
                    .child(format!("Until then the item waits in {}.", dialog.column)),
            )
            .child(later)
            .child(start);

        let card = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(560.0))
            .bg(theme::elevated(theme))
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .track_focus(&dialog.focus_handle)
            .key_context("PickProjectDialog")
            .on_key_down(cx.listener(handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(projects)
            .child(for_board)
            .child(footer);

        let backdrop = div()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.cancel_pick_project_dialog(cx)),
            )
            .child(card);

        Some(
            deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
                .with_priority(10)
                .into_any_element(),
        )
    }
}

/// `Two projects point at owner/name.`
fn point_at(count: usize, repository: &str) -> String {
    let count = match count {
        2 => "Two".to_string(),
        3 => "Three".to_string(),
        4 => "Four".to_string(),
        n => n.to_string(),
    };
    format!("{count} projects point at {repository}.")
}

fn handle_key_down(
    shell: &mut AppShell,
    event: &KeyDownEvent,
    window: &mut Window,
    cx: &mut Context<AppShell>,
) {
    cx.stop_propagation();
    let Some(chosen) = shell.pick_project_dialog.as_ref().map(|d| d.chosen) else {
        return;
    };
    match event.keystroke.key.as_str() {
        "escape" => shell.cancel_pick_project_dialog(cx),
        "enter" => shell.submit_pick_project_dialog(window, cx),
        "up" => shell.choose_project(chosen.saturating_sub(1), cx),
        "down" => shell.choose_project(chosen + 1, cx),
        "space" => shell.toggle_pick_for_board(cx),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dialog_says_how_many_projects_point_at_the_repository() {
        assert_eq!(point_at(2, "maui1911/CodeScope"), "Two projects point at maui1911/CodeScope.");
        assert_eq!(point_at(5, "o/r"), "5 projects point at o/r.");
    }
}
