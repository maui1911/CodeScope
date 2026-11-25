//! Board panel: a read-only view of the boards in `settings.json` (ADR-0027). It takes the
//! work-area slot the Overview, the Bots inbox and the diff viewer share; opening one closes
//! the others. Boards are read every 30 s whether or not the panel is on stage.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use codescope_core::Theme;
use codescope_core::board::{
    Board, BoardItem, BoardRef, BoardSettings, ColumnRole, ItemKind, ItemState, PanelColumn,
    PanelGroup, PanelSection, RoleKind, effective_mapping, github, missing_roles, panel_columns,
};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, AppContext as _, Context, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::app::AppShell;
use crate::dialogs::confirm::ConfirmSpec;
use crate::theme;

const BOARD_READ_INTERVAL: Duration = Duration::from_secs(30);
const READ_AGE_REFRESH: Duration = Duration::from_secs(5);
const DONE_PREVIEW: usize = 5;
/// GitHub's colour for a merged pull request or a completed issue.
const DONE_PURPLE: u32 = 0xbc3fbc;

#[derive(Default)]
pub(crate) struct BoardPanelState {
    pub reads_by_url: HashMap<String, BoardRead>,
    pub selected_url: Option<String>,
    pub show_all_done: bool,
    pub reading: bool,
    pub last_read: Option<Instant>,
}

#[derive(Default)]
pub(crate) struct BoardRead {
    pub last_good: Option<Board>,
    pub error: Option<String>,
    pub read_at: Option<Instant>,
}

type BoardResult = (String, Result<Board, String>);

fn read_boards(urls: Vec<String>) -> Vec<BoardResult> {
    urls.into_iter()
        .map(|url| {
            let result = match BoardRef::parse_url(&url) {
                Some(board) => github::fetch_board(&board).map_err(|err| format!("{err:#}")),
                None => Err("this is not a GitHub Projects board URL".to_string()),
            };
            (url, result)
        })
        .collect()
}

fn read_age(read_at: Option<Instant>) -> SharedString {
    let Some(at) = read_at else {
        return "not read yet".into();
    };
    let secs = at.elapsed().as_secs();
    match secs {
        0..5 => "read just now".into(),
        5..60 => format!("read {} s ago", secs / 5 * 5).into(),
        _ => format!("read {} min ago", secs / 60).into(),
    }
}

fn board_label(settings: &BoardSettings, read: Option<&BoardRead>) -> SharedString {
    if let Some(board) = read.and_then(|r| r.last_good.as_ref()) {
        return board.title.clone().into();
    }
    match BoardRef::parse_url(&settings.url) {
        Some(board) => format!("{}/{}", board.owner, board.number).into(),
        None => settings.url.clone().into(),
    }
}

fn who_moves(role: Option<ColumnRole>, has_column: bool) -> &'static str {
    match role.map(ColumnRole::kind) {
        Some(RoleKind::Trigger) => "you move items here",
        Some(RoleKind::Progress) => "CodeScope moves items here",
        Some(RoleKind::Rest) => "nothing runs",
        Some(RoleKind::Host) => "moved by GitHub",
        None if has_column => "no role",
        None => "",
    }
}

fn state_color(item: &BoardItem, theme: &Theme) -> Hsla {
    match (item.kind, item.state) {
        (ItemKind::Draft, _) => theme::text_faint(),
        (_, ItemState::Merged) | (ItemKind::Issue, ItemState::Closed) => {
            theme::rgb_to_hsla(codescope_core::Rgb::from_hex(DONE_PURPLE))
        }
        (ItemKind::PullRequest, ItemState::Closed) => theme::danger(),
        (ItemKind::PullRequest, ItemState::Open) => theme::accent(theme),
        (ItemKind::Issue, ItemState::Open) => theme::signal_ok(),
    }
}

fn item_reference(item: &BoardItem) -> SharedString {
    let repo = item
        .repository
        .as_deref()
        .map(|r| r.rsplit('/').next().unwrap_or(r))
        .unwrap_or_default();
    match (item.kind, item.number) {
        (ItemKind::Draft, _) | (_, None) => "draft".into(),
        (ItemKind::PullRequest, Some(n)) => format!("PR #{n} · {repo}").into(),
        (ItemKind::Issue, Some(n)) => format!("#{n} · {repo}").into(),
    }
}

impl AppShell {
    pub(crate) fn start_board_poll(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(due) = this.update(cx, |this, cx| {
                    if this.show_board {
                        cx.notify();
                    }
                    this.begin_board_read(false)
                }) else {
                    break;
                };
                if let Some(urls) = due {
                    let results = cx.background_spawn(async move { read_boards(urls) }).await;
                    if this
                        .update(cx, |this, cx| this.finish_board_read(results, cx))
                        .is_err()
                    {
                        break;
                    }
                }
                cx.background_executor().timer(READ_AGE_REFRESH).await;
            }
        })
        .detach();
    }

    pub(crate) fn refresh_boards(&mut self, cx: &mut Context<Self>) {
        let Some(urls) = self.begin_board_read(true) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            let results = cx.background_spawn(async move { read_boards(urls) }).await;
            let _ = this.update(cx, |this, cx| this.finish_board_read(results, cx));
        })
        .detach();
    }

    fn begin_board_read(&mut self, now: bool) -> Option<Vec<String>> {
        let state = &mut self.board;
        let due = now
            || state
                .last_read
                .is_none_or(|at| at.elapsed() >= BOARD_READ_INTERVAL);
        if state.reading || !due || self.settings.boards.is_empty() {
            return None;
        }
        state.reading = true;
        state.last_read = Some(Instant::now());
        Some(self.settings.boards.iter().map(|b| b.url.clone()).collect())
    }

    fn finish_board_read(&mut self, results: Vec<BoardResult>, cx: &mut Context<Self>) {
        let state = &mut self.board;
        state.reading = false;
        for (url, result) in results {
            let read = state.reads_by_url.entry(url).or_default();
            read.read_at = Some(Instant::now());
            match result {
                Ok(board) => {
                    read.last_good = Some(board);
                    read.error = None;
                }
                Err(err) => read.error = Some(err),
            }
        }
        let configured: Vec<&str> = self
            .settings
            .boards
            .iter()
            .map(|b| b.url.as_str())
            .collect();
        self.board
            .reads_by_url
            .retain(|url, _| configured.contains(&url.as_str()));
        if self.show_board {
            cx.notify();
        }
    }

    pub(crate) fn accept_board_read(&mut self, url: String, board: Board) {
        self.board.reads_by_url.insert(
            url.clone(),
            BoardRead {
                last_good: Some(board),
                error: None,
                read_at: Some(Instant::now()),
            },
        );
        self.board.selected_url = Some(url);
    }

    fn selected_board(&self) -> Option<&BoardSettings> {
        let boards = &self.settings.boards;
        self.board
            .selected_url
            .as_deref()
            .and_then(|url| boards.iter().find(|b| b.url == url))
            .or_else(|| boards.first())
    }

    pub(crate) fn set_show_board(&mut self, value: bool, cx: &mut Context<Self>) {
        if self.show_board == value {
            return;
        }
        if value {
            self.close_diff_viewer(cx);
            self.set_show_overview(false, cx);
            self.set_show_bots(false, cx);
        }
        self.show_board = value;
        self.sidebar_entity()
            .update(cx, |sidebar, cx| sidebar.set_board_visible(value, cx));
        if value {
            self.refresh_boards(cx);
        }
        cx.notify();
    }

    fn remove_board(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        let spec = ConfirmSpec::destructive(
            "Remove this board?",
            "CodeScope stops reading it. Nothing changes on GitHub.",
        )
        .with_detail(url.clone())
        .with_confirm_label("Remove");
        let answer = self.open_confirm_dialog(spec, window, cx);
        cx.spawn(async move |this, cx| {
            if !answer.await.unwrap_or(false) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                let mut settings = (*this.settings).clone();
                settings.boards.retain(|b| b.url != url);
                if let Err(err) = settings.save(this.paths_ref()) {
                    this.push_toast(
                        crate::app::ToastKind::Err,
                        "Couldn't remove the board",
                        Some(format!("{err:#}").into()),
                        cx,
                    );
                    return;
                }
                this.board.reads_by_url.remove(&url);
                if this.board.selected_url.as_deref() == Some(url.as_str()) {
                    this.board.selected_url = None;
                }
                this.apply_settings(settings, cx);
            });
        })
        .detach();
    }

    pub(crate) fn render_board_panel(
        &self,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let canvas = theme::canvas(theme);
        let ink = theme::ink(theme);
        let Some(settings) = self.selected_board().cloned() else {
            return div()
                .flex()
                .flex_col()
                .flex_grow()
                .size_full()
                .bg(canvas)
                .text_color(ink)
                .child(self.render_board_header(theme, None, cx))
                .child(self.render_board_empty(theme, cx))
                .into_any_element();
        };
        let read = self.board.reads_by_url.get(&settings.url);
        let mut body = div().id("board-body").flex_grow().overflow_scroll();
        body.style().min_size.height = Some(gpui::Length::Definite(px(0.0).into()));
        let body = match read.and_then(|r| r.last_good.as_ref()) {
            Some(board) => body.child(self.render_board_columns(theme, &settings, board, cx)),
            None => body.child(self.render_board_message(theme, read)),
        };
        let missing = read
            .and_then(|r| r.last_good.as_ref())
            .map(|board| missing_roles(board, &effective_mapping(board, &settings.columns)))
            .unwrap_or_default();

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .size_full()
            .bg(canvas)
            .text_color(ink)
            .font(theme::font_sans())
            .child(self.render_board_header(theme, Some(&settings), cx))
            .when(!missing.is_empty(), |s| {
                s.child(render_missing_roles(theme, &missing))
            })
            .child(body)
            .into_any_element()
    }

    fn render_board_header(
        &self,
        theme: &Arc<Theme>,
        selected: Option<&BoardSettings>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = theme::accent(theme);
        let divider = theme::divider(theme);
        let frost = theme::frost_10(theme);
        let read = selected.and_then(|s| self.board.reads_by_url.get(&s.url));

        let eyebrow = div()
            .px(px(7.0))
            .py(px(3.0))
            .border_1()
            .border_color(accent)
            .rounded(px(3.0))
            .text_size(px(10.0))
            .text_color(accent)
            .font(theme::font_mono())
            .child("BOARD");

        let pills: Vec<_> = self
            .settings
            .boards
            .iter()
            .enumerate()
            .map(|(index, board)| {
                let active = selected.is_some_and(|s| s.url == board.url);
                let url = board.url.clone();
                div()
                    .id(("board-pill", index))
                    .px(px(10.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .border_1()
                    .text_size(px(13.0))
                    .cursor_pointer()
                    .when(active, |s| {
                        s.bg(theme::elevated(theme))
                            .border_color(divider)
                            .text_color(theme::ink(theme))
                    })
                    .when(!active, |s| {
                        s.border_color(gpui::transparent_black())
                            .text_color(theme::ink_dim(theme))
                    })
                    .hover(move |s| s.bg(frost))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.board.selected_url = Some(url.clone());
                            this.board.show_all_done = false;
                            cx.notify();
                        }),
                    )
                    .child(board_label(board, self.board.reads_by_url.get(&board.url)))
            })
            .collect();

        let status: Option<(SharedString, Hsla)> =
            read.map(|read| match (&read.last_good, &read.error) {
                (_, Some(err)) => (format!("couldn't read: {err}").into(), theme::danger()),
                (Some(board), None) => {
                    let open = board.items.len();
                    let text = match (open, board.closed) {
                        (1, false) => "1 item".to_string(),
                        (n, false) => format!("{n} items"),
                        (n, true) => format!("{n} items · closed on GitHub"),
                    };
                    (text.into(), theme::ink_dim(theme))
                }
                (None, None) => ("reading…".into(), theme::ink_dim(theme)),
            });

        let text_button = move |id: &'static str, label: &'static str, color: Hsla| {
            div()
                .id(id)
                .flex_shrink_0()
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(4.0))
                .text_size(px(13.0))
                .text_color(color)
                .cursor_pointer()
                .hover(move |s| s.bg(frost))
                .child(label)
        };

        let selected_url = selected.map(|s| s.url.clone());
        let board_url = read
            .and_then(|r| r.last_good.as_ref())
            .map(|b| b.url.clone())
            .or(selected_url.clone());

        div()
            .min_h(px(56.0))
            .px(px(24.0))
            .py(px(10.0))
            .flex()
            .flex_row()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .border_b_1()
            .border_color(divider)
            .child(eyebrow)
            .children(pills)
            .children(status.map(|(text, color)| {
                div()
                    .ml(px(8.0))
                    .text_size(px(13.0))
                    .text_color(color)
                    .child(text)
            }))
            .child(div().flex_grow())
            .when(selected.is_some(), |s| {
                s.child(
                    div()
                        .text_size(px(11.0))
                        .text_color(theme::text_faint())
                        .font(theme::font_mono())
                        .child(if self.board.reading {
                            SharedString::from("reading…")
                        } else {
                            read_age(read.and_then(|r| r.read_at))
                        }),
                )
            })
            .child(
                text_button("board-add", "+ Add board", accent).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.open_add_board_dialog(window, cx)),
                ),
            )
            .children(board_url.map(|url| {
                text_button("board-open", "Open on GitHub", accent).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |_, _, _, _| crate::sidebar::open_url_in_browser(&url)),
                )
            }))
            .children(selected_url.map(|url| {
                text_button("board-remove", "Remove", theme::danger()).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.remove_board(url.clone(), window, cx);
                    }),
                )
            }))
            .child(
                text_button("board-back", "← Back to workspace", accent).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.set_show_board(false, cx)),
                ),
            )
            .into_any_element()
    }

    fn render_board_empty(&self, theme: &Arc<Theme>, cx: &mut Context<Self>) -> AnyElement {
        let accent = theme::accent(theme);
        div()
            .flex_grow()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(12.0))
            .child(
                div()
                    .text_size(px(16.0))
                    .text_color(theme::ink(theme))
                    .child("No board yet"),
            )
            .child(
                div()
                    .max_w(px(460.0))
                    .text_size(px(13.0))
                    .text_color(theme::ink_muted(theme))
                    .text_center()
                    .child(
                        "Add a GitHub Projects board to see its items here. CodeScope reads it \
                         every 30 seconds and does not change anything on it yet.",
                    ),
            )
            .child(
                div()
                    .id("board-empty-add")
                    .mt(px(4.0))
                    .px(px(14.0))
                    .py(px(8.0))
                    .rounded(px(6.0))
                    .bg(accent)
                    .text_size(px(13.0))
                    .text_color(theme::canvas(theme))
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| this.open_add_board_dialog(window, cx)),
                    )
                    .child("Add board"),
            )
            .into_any_element()
    }

    fn render_board_message(
        &self,
        theme: &Arc<Theme>,
        read: Option<&BoardRead>,
    ) -> impl IntoElement {
        let (title, detail): (SharedString, SharedString) = match read.and_then(|r| r.error.clone())
        {
            Some(err) => (
                "Couldn't read this board".into(),
                format!("{err}. CodeScope tries again every 30 seconds.").into(),
            ),
            None => ("Reading the board…".into(), "".into()),
        };
        div()
            .p(px(24.0))
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .text_size(px(15.0))
                    .text_color(theme::ink(theme))
                    .child(title),
            )
            .child(
                div()
                    .text_size(px(13.0))
                    .text_color(theme::ink_muted(theme))
                    .child(detail),
            )
    }

    fn render_board_columns(
        &self,
        theme: &Arc<Theme>,
        settings: &BoardSettings,
        board: &Board,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let columns = panel_columns(board, &effective_mapping(board, &settings.columns));
        div()
            .flex()
            .flex_row()
            .items_start()
            .gap(px(16.0))
            .px(px(24.0))
            .py(px(20.0))
            .children(columns.iter().map(|column| {
                match column.group {
                    PanelGroup::Done => self
                        .render_done_column(theme, column, board, cx)
                        .into_any_element(),
                    _ => render_group_column(theme, column, board).into_any_element(),
                }
            }))
            .into_any_element()
    }

    fn render_done_column(
        &self,
        theme: &Arc<Theme>,
        column: &PanelColumn,
        board: &Board,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let items: Vec<usize> = column
            .sections
            .iter()
            .flat_map(|s| s.items.iter().copied())
            .collect();
        let total = items.len();
        let shown = if self.board.show_all_done {
            total
        } else {
            total.min(DONE_PREVIEW)
        };
        let rows: Vec<_> = items[..shown]
            .iter()
            .map(|&index| {
                let item = &board.items[index];
                let url = item.url.clone().unwrap_or_else(|| board.url.clone());
                div()
                    .id(("board-done", index))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(8.0))
                    .border_1()
                    .border_color(theme::frost_10(theme))
                    .rounded(px(8.0))
                    .text_color(theme::ink_muted(theme))
                    .cursor_pointer()
                    .hover(|s| s.border_color(theme::frost_20(theme)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, _| crate::sidebar::open_url_in_browser(&url)),
                    )
                    .child(state_dot(state_color(item, theme)))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_size(px(11.0))
                            .text_color(theme::text_faint())
                            .font(theme::font_mono())
                            .child(item.number.map(|n| format!("#{n}")).unwrap_or_default()),
                    )
                    .child(
                        div()
                            .min_w(px(0.0))
                            .text_size(px(12.0))
                            .truncate()
                            .child(item.title.clone()),
                    )
            })
            .collect();
        let toggle = (total > DONE_PREVIEW).then(|| {
            let label: SharedString = if self.board.show_all_done {
                "Show fewer".into()
            } else {
                format!("Show all {total}").into()
            };
            div()
                .id("board-done-toggle")
                .px(px(2.0))
                .py(px(4.0))
                .text_size(px(12.0))
                .text_color(theme::accent(theme))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.board.show_all_done = !this.board.show_all_done;
                        cx.notify();
                    }),
                )
                .child(label)
        });
        div()
            .w(px(240.0))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(group_heading(
                theme,
                PanelGroup::Done,
                Some(total),
                "moved by GitHub",
            ))
            .when(column.sections.is_empty(), |s| s.child(not_on_board()))
            .children(rows)
            .children(toggle)
            .into_any_element()
    }
}

fn group_heading(
    theme: &Arc<Theme>,
    group: PanelGroup,
    count: Option<usize>,
    hint: &'static str,
) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(2.0))
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::ink(theme))
                .child(group.label()),
        )
        .children(count.map(|n| {
            div()
                .text_size(px(11.0))
                .text_color(theme::text_faint())
                .font(theme::font_mono())
                .child(n.to_string())
        }))
        .child(div().flex_grow())
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme::text_faint())
                .child(hint),
        )
}

fn not_on_board() -> impl IntoElement {
    div()
        .px(px(2.0))
        .text_size(px(12.0))
        .text_color(theme::text_faint())
        .child("No column for this on the board")
}

fn render_group_column(
    theme: &Arc<Theme>,
    column: &PanelColumn,
    board: &Board,
) -> impl IntoElement {
    let framed = matches!(
        column.group,
        PanelGroup::Research | PanelGroup::Implementation
    );
    let (width, hint) = match column.group {
        PanelGroup::Todo => (260.0, "backlog · nothing runs"),
        PanelGroup::Research => (300.0, "optional phase"),
        PanelGroup::Implementation => (300.0, "ends in a pull request"),
        PanelGroup::Done => (240.0, "moved by GitHub"),
    };
    let count = (!framed).then(|| column.sections.iter().map(|s| s.items.len()).sum());
    let lone_todo = column.group == PanelGroup::Todo;
    div()
        .w(px(width))
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .when(framed, |s| {
            s.p(px(12.0))
                .bg(theme::with_alpha(theme.chrome.ink, 0.02))
                .border_1()
                .border_color(theme::frost_10(theme))
                .rounded(px(10.0))
        })
        .child(group_heading(theme, column.group, count, hint))
        .when(column.sections.is_empty(), |s| s.child(not_on_board()))
        .children(column.sections.iter().map(|section| {
            let heading = !(lone_todo && section.role == Some(ColumnRole::Todo));
            render_section(theme, section, board, heading)
        }))
}

fn render_section(
    theme: &Arc<Theme>,
    section: &PanelSection,
    board: &Board,
    heading: bool,
) -> impl IntoElement {
    let trigger = section.role.map(ColumnRole::kind) == Some(RoleKind::Trigger);
    let name: SharedString = section
        .column
        .clone()
        .unwrap_or_else(|| "No status".to_string())
        .into();
    let who = who_moves(section.role, section.column.is_some());
    div()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .when(heading, |s| {
            s.child(
                div()
                    .pt(px(4.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .text_size(px(11.0))
                    .font(theme::font_mono())
                    .child(
                        div()
                            .text_color(if trigger {
                                theme::accent(theme)
                            } else {
                                theme::ink_muted(theme)
                            })
                            .child(name),
                    )
                    .child(
                        div()
                            .text_color(theme::text_faint())
                            .child(section.items.len().to_string()),
                    )
                    .child(div().flex_grow())
                    .child(div().text_color(theme::text_faint()).child(who)),
            )
        })
        .children(
            section
                .items
                .iter()
                .map(|&index| render_card(theme, &board.items[index], index, &board.url)),
        )
}

fn render_card(
    theme: &Arc<Theme>,
    item: &BoardItem,
    index: usize,
    board_url: &str,
) -> impl IntoElement {
    let url = item.url.clone().unwrap_or_else(|| board_url.to_string());
    let note = match item.state {
        ItemState::Closed => Some("closed"),
        ItemState::Merged => Some("merged"),
        ItemState::Open => None,
    };
    let accent = theme::accent(theme);
    div()
        .id(("board-card", index))
        .flex()
        .flex_col()
        .gap(px(6.0))
        .px(px(12.0))
        .py(px(10.0))
        .bg(theme::elevated(theme))
        .border_1()
        .border_color(theme::divider(theme))
        .rounded(px(8.0))
        .cursor_pointer()
        .hover(move |s| s.border_color(accent))
        .on_mouse_down(MouseButton::Left, move |_, _, _| {
            crate::sidebar::open_url_in_browser(&url)
        })
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .text_color(theme::text_faint())
                .font(theme::font_mono())
                .child(state_dot(state_color(item, theme)))
                .child(item_reference(item))
                .children(note.map(|n| div().child(format!("· {n}")))),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::ink(theme))
                .child(item.title.clone()),
        )
}

fn state_dot(color: Hsla) -> impl IntoElement {
    div().flex_shrink_0().size(px(6.0)).rounded_full().bg(color)
}

fn render_missing_roles(theme: &Arc<Theme>, missing: &[ColumnRole]) -> impl IntoElement {
    let names: Vec<&str> = missing.iter().map(|r| r.label()).collect();
    div()
        .px(px(24.0))
        .py(px(8.0))
        .border_b_1()
        .border_color(theme::divider(theme))
        .text_size(px(12.0))
        .text_color(theme::ink_muted(theme))
        .child(format!(
            "Not on this board: {}. Add them as options of the board's Status field on GitHub; \
             CodeScope finds them by name.",
            names.join(", ")
        ))
}
