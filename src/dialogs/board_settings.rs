//! Board settings dialog (ADR-0027): which column of the board plays which role, and which
//! project works on the items of a repository that several projects have a remote for. *Save*
//! writes both to the board's entry in `settings.json`.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use codescope_core::board::dispatch::{ProjectRemotes, projects_for_repository};
use codescope_core::board::{
    Board, BoardSettings, ColumnMapping, ColumnRole, RoleKind, effective_mapping, remap,
};
use codescope_core::{Settings, Theme};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Bounds, Context, ElementId, FocusHandle, Hsla, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Pixels, SharedString,
    StatefulInteractiveElement, Styled, Window, anchored, deferred, div, point, px,
};

use crate::app::AppShell;
use crate::app::board_dispatch::read_remotes;
use crate::theme;

/// A dropdown of the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dropdown {
    /// The column this role maps to.
    Role(ColumnRole),
    /// The row of [`BoardSettingsDialogState::repositories`] at this index.
    Repository(usize),
}

/// What the dialog edits, until *Save* writes it or *Cancel* drops it.
pub struct BoardSettingsDialogState {
    /// Holds the keyboard while the dialog is open.
    pub focus_handle: FocusHandle,
    /// The `settings.json` entry this edits.
    pub board_url: String,
    /// The board as last read, without its items: its columns are what the roles map to.
    pub board: Board,
    /// The mapping as edited, with every role that has a column on the board.
    pub mapping: ColumnMapping,
    /// `None` while the projects' remotes are read.
    pub repositories: Option<Vec<RepositoryPick>>,
    /// The dropdown whose popup shows.
    pub open: Option<Dropdown>,
    /// Where each dropdown painted last frame, for its popup to open under it.
    pub anchors: Rc<RefCell<HashMap<Dropdown, Bounds<Pixels>>>>,
    /// Why the last *Save* failed.
    pub error: Option<String>,
}

/// A repository several projects have a remote for, and the one picked to work on its items.
#[derive(Debug, PartialEq, Eq)]
pub struct RepositoryPick {
    /// `owner/name`, cased as the board's items or the kept pick have it.
    pub repository: String,
    /// `(project id, name)` of each project with a remote for it.
    pub projects: Vec<(String, String)>,
    /// `None` asks every time.
    pub picked: Option<String>,
}

impl AppShell {
    /// Open the dialog for the board at `url`, once it has been read.
    pub fn open_board_settings_dialog(
        &mut self,
        url: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.board_settings_dialog.is_some() {
            return;
        }
        let Some(settings) = self.settings.boards.iter().find(|b| b.url == url).cloned() else {
            return;
        };
        let Some(read) = self.board.reads_by_url.get(&settings.url) else {
            return;
        };
        let Some(board) = read.last_good.as_ref() else {
            return;
        };
        let item_repositories: Vec<String> = board
            .items
            .iter()
            .filter_map(|item| item.repository.clone())
            .collect();
        let local = self.local_projects(cx);
        let url = settings.url.clone();
        cx.spawn(async move |this, cx| {
            let remotes = cx.background_spawn(async move { read_remotes(local) }).await;
            let _ = this.update(cx, |this, cx| {
                this.fill_board_settings_projects(&url, &item_repositories, &remotes, cx);
            });
        })
        .detach();
        let board = Board {
            items: Vec::new(),
            ..board.clone()
        };
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        // gpui hands the focus to the root's `track_focus` on the click that opened this.
        window.prevent_default();
        self.board_settings_dialog = Some(BoardSettingsDialogState {
            focus_handle,
            mapping: effective_mapping(&board, &settings.columns),
            board_url: settings.url,
            board,
            repositories: None,
            open: None,
            anchors: Rc::default(),
            error: None,
        });
        cx.notify();
    }

    fn fill_board_settings_projects(
        &mut self,
        url: &str,
        item_repositories: &[String],
        remotes: &[ProjectRemotes],
        cx: &mut Context<Self>,
    ) {
        let Some(settings) = self.settings.boards.iter().find(|b| b.url == url) else {
            return;
        };
        let projects = self.current_projects(cx);
        let picks = repository_picks(item_repositories, settings, remotes, |id| {
            projects.project(id).map(|p| p.name.clone())
        });
        if let Some(dialog) = self
            .board_settings_dialog
            .as_mut()
            .filter(|d| d.board_url == url)
        {
            dialog.repositories = Some(picks);
            cx.notify();
        }
    }

    /// Close the dialog without saving.
    pub fn cancel_board_settings_dialog(&mut self, cx: &mut Context<Self>) {
        if self.board_settings_dialog.take().is_some() {
            cx.notify();
        }
    }

    fn toggle_board_dropdown(&mut self, dropdown: Dropdown, cx: &mut Context<Self>) {
        if let Some(dialog) = self.board_settings_dialog.as_mut() {
            dialog.open = (dialog.open != Some(dropdown)).then_some(dropdown);
            cx.notify();
        }
    }

    fn choose_column(&mut self, role: ColumnRole, column: String, cx: &mut Context<Self>) {
        if let Some(dialog) = self.board_settings_dialog.as_mut() {
            remap(&mut dialog.mapping, role, &column);
            dialog.mapping = effective_mapping(&dialog.board, &dialog.mapping);
            dialog.open = None;
            cx.notify();
        }
    }

    fn choose_repository_project(
        &mut self,
        row: usize,
        project_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(dialog) = self.board_settings_dialog.as_mut()
            && let Some(pick) = dialog.repositories.as_mut().and_then(|r| r.get_mut(row))
        {
            pick.picked = project_id;
            dialog.open = None;
            cx.notify();
        }
    }

    /// Write the mapping and the picks to the board's entry, then read the board again. A pick
    /// row not filled in yet leaves the kept picks as they are.
    pub fn save_board_settings_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.board_settings_dialog.as_ref() else {
            return;
        };
        let url = dialog.board_url.clone();
        let read = &dialog.board;
        let columns = effective_mapping(read, &dialog.mapping);
        let projects: Option<BTreeMap<String, String>> = dialog.repositories.as_ref().map(|r| {
            r.iter()
                .filter_map(|pick| Some((pick.repository.clone(), pick.picked.clone()?)))
                .collect()
        });
        let now = codescope_core::time::now_iso8601();
        let saved = Settings::update(self.paths_ref(), |settings| {
            if let Some(board) = settings.boards.iter_mut().find(|b| b.url == url) {
                board.remap_columns(read, columns, &now);
                if let Some(projects) = projects {
                    board.projects = projects;
                }
            }
        });
        match saved {
            Ok(settings) => {
                self.board_settings_dialog = None;
                self.apply_settings(settings, cx);
                self.refresh_boards(cx);
            }
            Err(err) => {
                if let Some(dialog) = self.board_settings_dialog.as_mut() {
                    dialog.error = Some(format!("Couldn't save settings: {err:#}"));
                }
                cx.notify();
            }
        }
    }

    /// The backdrop and card, and the popup of an open dropdown above them.
    pub fn render_board_settings_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.board_settings_dialog.as_ref()?;
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
            .flex_row()
            .items_center()
            .gap_2()
            .px_6()
            .py(px(18.0))
            .border_b_1()
            .border_color(divider)
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(ink)
                    .child("Board settings"),
            )
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .text_size(px(13.0))
                    .text_color(muted)
                    .child(dialog.board.title.clone()),
            )
            .child(
                div()
                    .id("board-settings-close")
                    .px_2()
                    .text_size(px(14.0))
                    .text_color(muted)
                    .cursor_pointer()
                    .hover(move |s| s.text_color(ink))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.cancel_board_settings_dialog(cx);
                        }),
                    )
                    .child("×"),
            );

        let open = dialog.open;
        let rows = ColumnRole::ALL.into_iter().map(|role| {
            let (title, meaning) = role_text(role);
            let (kind, kind_color) = kind_badge(role.kind(), theme);
            let column = dialog.mapping.get(&role).cloned();
            let dropdown = Dropdown::Role(role);
            let trigger = dropdown_trigger(
                ("board-settings-role", role as usize),
                dropdown,
                column.clone().map_or("Not on this board".into(), Into::into),
                column.is_none(),
                open == Some(dropdown),
                &dialog.anchors,
                theme,
                cx,
            );
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .px_3()
                .py(px(10.0))
                .border_b_1()
                .border_color(divider)
                .child(
                    div().w(px(84.0)).flex_shrink_0().child(
                        div()
                            .flex()
                            .flex_row()
                            .child(
                                div()
                                    .px(px(6.0))
                                    .py(px(1.0))
                                    .rounded(px(4.0))
                                    .border_1()
                                    .border_color(kind_color)
                                    .text_size(px(10.0))
                                    .font(theme::font_mono())
                                    .text_color(kind_color)
                                    .child(kind),
                            ),
                    ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(gpui::FontWeight::MEDIUM)
                                .text_color(ink)
                                .child(title),
                        )
                        .child(div().text_size(px(12.0)).text_color(faint).child(meaning)),
                )
                .child(div().w(px(190.0)).flex_shrink_0().child(trigger))
                .into_any_element()
        });
        let columns = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .text_color(ink)
                            .child("Map columns"),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(faint)
                            .child("Picking a column another role has swaps the two."),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .border_1()
                    .border_color(divider)
                    .rounded(px(8.0))
                    .children(rows),
            );

        let repositories = dialog.repositories.as_deref().unwrap_or_default();
        let projects = (!repositories.is_empty()).then(|| {
            let rows = repositories.iter().enumerate().map(|(row, pick)| {
                let dropdown = Dropdown::Repository(row);
                let label = pick
                    .picked
                    .as_deref()
                    .and_then(|id| pick.projects.iter().find(|(choice, _)| choice == id))
                    .map_or("Ask every time".to_string(), |(_, name)| name.clone());
                let trigger = dropdown_trigger(
                    ("board-settings-repository", row),
                    dropdown,
                    label.into(),
                    pick.picked.is_none(),
                    open == Some(dropdown),
                    &dialog.anchors,
                    theme,
                    cx,
                );
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    .px_3()
                    .py(px(10.0))
                    .border_b_1()
                    .border_color(divider)
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(px(12.0))
                            .font(theme::font_mono())
                            .text_color(ink)
                            .child(pick.repository.clone()),
                    )
                    .child(div().w(px(240.0)).flex_shrink_0().child(trigger))
                    .into_any_element()
            });
            div()
                .flex()
                .flex_col()
                .gap(px(10.0))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(13.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .text_color(ink)
                                .child("Projects"),
                        )
                        .child(div().text_size(px(12.0)).text_color(faint).child(
                            "Several projects have these repositories as a remote. The one \
                             picked works on their items on this board.",
                        )),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .border_1()
                        .border_color(divider)
                        .rounded(px(8.0))
                        .children(rows),
                )
        });

        let body = div()
            .id("board-settings-body")
            .flex()
            .flex_col()
            .gap(px(22.0))
            .px_6()
            .py_5()
            .max_h(viewport.height * 0.7)
            .overflow_y_scroll()
            .child(columns)
            .children(projects);

        let cancel = div()
            .id("board-settings-cancel")
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
                    this.cancel_board_settings_dialog(cx);
                }),
            )
            .child("Cancel");
        let save = div()
            .id("board-settings-save")
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
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.save_board_settings_dialog(cx);
                }),
            )
            .child("Save");
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
                    .text_color(theme::danger())
                    .children(dialog.error.clone()),
            )
            .child(cancel)
            .child(save);

        let card = div()
            .flex()
            .flex_col()
            .w(px(720.0))
            .bg(theme::elevated(theme))
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .track_focus(&dialog.focus_handle)
            .key_context("BoardSettingsDialog")
            .on_key_down(cx.listener(handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(body)
            .child(footer);

        let backdrop = div()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.cancel_board_settings_dialog(cx)),
            )
            .child(card);

        let mut layers = vec![
            deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
                .with_priority(10)
                .into_any_element(),
        ];
        layers.extend(self.render_board_dropdown(dialog, viewport, theme, cx));
        Some(div().children(layers).into_any_element())
    }

    fn render_board_dropdown(
        &self,
        dialog: &BoardSettingsDialogState,
        viewport: gpui::Size<Pixels>,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let Some(open) = dialog.open else {
            return Vec::new();
        };
        let Some(trigger) = dialog.anchors.borrow().get(&open).copied() else {
            return Vec::new();
        };
        let ink = theme::ink(theme);
        let accent = theme::accent(theme);
        let faint = theme::text_faint();
        let frost = theme::frost_10(theme);
        let width = if trigger.size.width > POPUP_MIN_WIDTH {
            trigger.size.width
        } else {
            POPUP_MIN_WIDTH
        };
        let option = |id: ElementId, label: String, note: Option<&'static str>, current: bool| {
            div()
                .id(id)
                .h(px(28.0))
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .text_size(px(12.0))
                .text_color(if current { accent } else { ink })
                .cursor_pointer()
                .hover(move |s| s.bg(frost))
                .child(
                    div()
                        .when(note.is_some(), |s| s.flex_shrink_0())
                        .when(note.is_none(), |s| s.flex_1().min_w(px(0.0)).truncate())
                        .child(label),
                )
                .children(note.map(|note| {
                    div().flex_1().flex().justify_end().text_color(faint).child(note)
                }))
        };
        let rows: Vec<AnyElement> = match open {
            Dropdown::Role(role) => dialog
                .board
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    let holder = dialog
                        .mapping
                        .iter()
                        .find(|(other, name)| **other != role && **name == column.name)
                        .map(|(other, _)| role_text(*other).0);
                    let current = dialog.mapping.get(&role) == Some(&column.name);
                    let name = column.name.clone();
                    option(
                        ("board-settings-column", index).into(),
                        column.name.clone(),
                        holder,
                        current,
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.choose_column(role, name.clone(), cx);
                        }),
                    )
                    .into_any_element()
                })
                .collect(),
            Dropdown::Repository(row) => {
                let Some(pick) = dialog.repositories.as_ref().and_then(|r| r.get(row)) else {
                    return Vec::new();
                };
                let ask = option(
                    "board-settings-ask".into(),
                    "Ask every time".to_string(),
                    None,
                    pick.picked.is_none(),
                )
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.choose_repository_project(row, None, cx);
                    }),
                )
                .into_any_element();
                let projects = pick.projects.iter().enumerate().map(|(index, (id, name))| {
                    let id = id.clone();
                    option(
                        ("board-settings-project", index).into(),
                        name.clone(),
                        None,
                        pick.picked.as_deref() == Some(id.as_str()),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.choose_repository_project(row, Some(id.clone()), cx);
                        }),
                    )
                    .into_any_element()
                });
                projects.chain([ask]).collect()
            }
        };
        let popup = div()
            .id("board-settings-popup")
            .w(width)
            .max_h(px(280.0))
            .py_1()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .bg(theme::elevated(theme))
            .border_1()
            .border_color(theme::divider(theme))
            .rounded_md()
            .shadow_lg()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .children(rows);
        let click_catcher = deferred(
            anchored().position(point(px(0.0), px(0.0))).child(
                div()
                    .w(viewport.width)
                    .h(viewport.height)
                    .occlude()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.toggle_board_dropdown(open, cx);
                        }),
                    ),
            ),
        )
        .with_priority(15)
        .into_any_element();
        let popup_layer = deferred(
            anchored()
                .position(point(trigger.right() - width, trigger.bottom() + px(4.0)))
                .snap_to_window_with_margin(px(8.0))
                .child(popup),
        )
        .with_priority(20)
        .into_any_element();
        vec![click_catcher, popup_layer]
    }
}

/// Wide enough for a column name and the role that has it.
const POPUP_MIN_WIDTH: Pixels = px(320.0);

#[allow(clippy::too_many_arguments)]
fn dropdown_trigger(
    id: impl Into<ElementId>,
    dropdown: Dropdown,
    label: SharedString,
    placeholder: bool,
    open: bool,
    anchors: &Rc<RefCell<HashMap<Dropdown, Bounds<Pixels>>>>,
    theme: &Arc<Theme>,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let anchors = anchors.clone();
    div()
        .id(id)
        .relative()
        .px_3()
        .h(px(32.0))
        .bg(theme::canvas(theme))
        .border_1()
        .border_color(if open { theme::accent(theme) } else { theme::divider(theme) })
        .rounded(px(6.0))
        .text_size(px(13.0))
        .text_color(if placeholder { theme::text_faint() } else { theme::ink(theme) })
        .cursor_pointer()
        .flex()
        .flex_row()
        .items_center()
        .gap_2()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.toggle_board_dropdown(dropdown, cx);
            }),
        )
        .child(div().flex_grow().truncate().child(label))
        .child(div().text_color(theme::ink_ghost(theme)).child("▾"))
        .child(
            gpui::canvas(
                move |bounds, _, _| {
                    anchors.borrow_mut().insert(dropdown, bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
}

/// What a role means, as its row says it: a title and a line under it.
fn role_text(role: ColumnRole) -> (&'static str, &'static str) {
    let picks_up = "You move an item here; CodeScope picks it up";
    let starts = "CodeScope moves the item here when it starts";
    match role {
        ColumnRole::Todo => ("Backlog", "Items wait here; Move back to Todo puts them here"),
        ColumnRole::Research => ("Start research", picks_up),
        ColumnRole::Researching => ("Research running", starts),
        ColumnRole::NeedsDecision => ("Findings ready", "Findings are on the issue; you decide"),
        ColumnRole::Ready => ("Start implementation", picks_up),
        ColumnRole::InProgress => ("Implementation running", starts),
        ColumnRole::InReview => ("Pull request open", "The agent opened a pull request"),
        ColumnRole::Done => ("Done", "GitHub's workflows move items here on merge or close"),
    }
}

fn kind_badge(kind: RoleKind, theme: &Theme) -> (&'static str, Hsla) {
    match kind {
        RoleKind::Rest => ("BACKLOG", theme::ink_dim(theme)),
        RoleKind::Trigger => ("TRIGGER", theme::accent(theme)),
        RoleKind::Progress => ("PROGRESS", theme::ink_muted(theme)),
        RoleKind::Host => ("GITHUB", crate::board_panel::done_purple()),
    }
}

/// A row for each repository of the board's items that several projects have a remote for,
/// and for each repository the board kept a pick for; once whatever its casing, sorted without
/// case. A pick of a project without the remote, or no longer in CodeScope, asks every time.
fn repository_picks(
    item_repositories: &[String],
    settings: &BoardSettings,
    remotes: &[ProjectRemotes],
    name_of: impl Fn(&str) -> Option<String>,
) -> Vec<RepositoryPick> {
    let shared = item_repositories
        .iter()
        .filter(|r| projects_for_repository(remotes, r).len() > 1);
    let mut found: Vec<&str> = Vec::new();
    for repository in shared.chain(settings.projects.keys()) {
        if !found.iter().any(|known| known.eq_ignore_ascii_case(repository)) {
            found.push(repository);
        }
    }
    found.sort_by_key(|repository| repository.to_ascii_lowercase());
    found
        .into_iter()
        .map(|repository| {
            let projects: Vec<(String, String)> = projects_for_repository(remotes, repository)
                .into_iter()
                .filter_map(|(project, _)| {
                    Some((project.project_id.clone(), name_of(&project.project_id)?))
                })
                .collect();
            let picked = settings
                .picked_project(repository)
                .filter(|id| projects.iter().any(|(choice, _)| choice == id))
                .map(str::to_string);
            RepositoryPick {
                repository: repository.to_string(),
                projects,
                picked,
            }
        })
        .collect()
}

fn handle_key_down(
    shell: &mut AppShell,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) {
    cx.stop_propagation();
    let Some(open) = shell.board_settings_dialog.as_ref().map(|d| d.open) else {
        return;
    };
    match (event.keystroke.key.as_str(), open) {
        ("escape", Some(dropdown)) => shell.toggle_board_dropdown(dropdown, cx),
        ("escape", None) => shell.cancel_board_settings_dialog(cx),
        ("enter", None) => shell.save_board_settings_dialog(cx),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remotes(project_id: &str, repositories: &[&str]) -> ProjectRemotes {
        ProjectRemotes {
            project_id: project_id.into(),
            path: format!("D:/{project_id}"),
            remotes: repositories
                .iter()
                .map(|r| ("origin".to_string(), format!("https://github.com/{r}.git")))
                .collect(),
        }
    }

    #[test]
    fn a_row_for_each_repository_projects_share_or_the_board_kept_a_pick_for() {
        let items: Vec<String> = ["acme/web", "maui1911/CodeScope", "maui1911/codescope"]
            .map(String::from)
            .to_vec();
        let remotes = [
            remotes("main", &["maui1911/CodeScope", "acme/web"]),
            remotes("review", &["maui1911/CodeScope"]),
            remotes("gone", &["maui1911/CodeScope"]),
        ];
        let mut settings = BoardSettings::default();
        settings.pick_project("MAUI1911/CODESCOPE", "review");
        settings.pick_project("acme/old", "main");
        let name_of = |id: &str| (id != "gone").then(|| format!("{id} project"));
        let pick = |repository: &str, projects: &[&str], picked: Option<&str>| RepositoryPick {
            repository: repository.into(),
            projects: projects
                .iter()
                .map(|id| (id.to_string(), format!("{id} project")))
                .collect(),
            picked: picked.map(String::from),
        };
        assert_eq!(
            repository_picks(&items, &settings, &remotes, name_of),
            vec![
                pick("acme/old", &[], None),
                pick("maui1911/CodeScope", &["main", "review"], Some("review")),
            ]
        );
    }

    #[test]
    fn every_role_says_what_it_means() {
        for role in ColumnRole::ALL {
            let (title, meaning) = role_text(role);
            assert!(!title.is_empty() && !meaning.is_empty(), "{role:?}");
        }
        assert_eq!(role_text(ColumnRole::Ready).0, "Start implementation");
    }
}
