//! The item drawer beside the board panel's columns (ADR-0027): one item's dispatch, what it
//! needs from the user, and what the drawer read of it.

use std::sync::Arc;

use codescope_core::Theme;
use codescope_core::board::findings::Paragraph;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, px,
};

use crate::app::AppShell;
use crate::app::board_dispatch::{
    CardAction, DetailsView, FindingsView, ItemDrawer, Mark, MoveRow, Needs, NoteTone,
};
use crate::theme;

pub(crate) const DRAWER_WIDTH: f32 = 420.0;
const LABEL_WIDTH: f32 = 88.0;

impl AppShell {
    pub(crate) fn select_board_item(&mut self, item_id: &str, cx: &mut Context<Self>) {
        self.board.drawer = Some(item_id.to_string());
        if let Some(url) = self.selected_board().map(|board| board.url.clone()) {
            self.read_item_details(&url, item_id, cx);
        }
        cx.notify();
    }

    pub(crate) fn close_item_drawer(&mut self, cx: &mut Context<Self>) {
        self.board.details = None;
        if self.board.drawer.take().is_some() {
            cx.notify();
        }
    }

    pub(crate) fn render_item_drawer(
        &self,
        theme: &Arc<Theme>,
        drawer: ItemDrawer,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let divider = theme::divider(theme);
        let mut body = div()
            .id("item-drawer-body")
            .flex_1()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap(px(20.0))
            .px(px(20.0))
            .py(px(16.0));
        body.style().min_size.height = Some(gpui::Length::Definite(px(0.0).into()));
        let test_by_hand = match &drawer.details {
            DetailsView::Read(read) => read.test_by_hand.as_deref(),
            DetailsView::None | DetailsView::Reading | DetailsView::Failed(_) => None,
        };
        let body = body
            .children(drawer.needs.as_ref().map(|needs| {
                render_needs(theme, needs, &drawer, cx).into_any_element()
            }))
            .children(drawer.needs.is_none().then(|| render_links(theme, &drawer, cx)).flatten())
            .children(drawer.note.as_ref().map(|(text, tone)| {
                div()
                    .text_size(px(12.0))
                    .text_color(tone_color(theme, *tone))
                    .child(text.clone())
                    .into_any_element()
            }))
            .children(test_by_hand.map(|paragraphs| {
                section("TEST BY HAND").child(render_paragraphs(theme, paragraphs))
            }))
            .child(render_rows(theme, &drawer))
            .children(render_details(theme, &drawer.details));
        div()
            .id("item-drawer")
            .w(px(DRAWER_WIDTH))
            .flex_shrink_0()
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::elevated(theme))
            .border_l_1()
            .border_color(divider)
            .child(render_header(theme, &drawer, cx))
            .child(body)
            .children(drawer.back_to_todo.as_ref().map(|back| {
                let (board_url, item_id) = (drawer.board_url.clone(), drawer.item_id.clone());
                let label = if back.moving {
                    "Moving back to Todo…"
                } else {
                    "Move back to Todo"
                };
                div()
                    .flex()
                    .flex_col()
                    .items_start()
                    .gap(px(6.0))
                    .px(px(20.0))
                    .py(px(14.0))
                    .border_t_1()
                    .border_color(divider)
                    .child(
                        secondary_button(theme, "item-drawer-todo", label).when(
                            !back.moving,
                            |button| {
                                button.on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.move_back_to_todo(&board_url, &item_id, cx);
                                    }),
                                )
                            },
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(theme::text_faint())
                            .child(back.hint),
                    )
            }))
            .into_any_element()
    }
}

fn render_header(
    theme: &Arc<Theme>,
    drawer: &ItemDrawer,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let accent = theme::accent(theme);
    let url = drawer.url.clone();
    let reference = div()
        .id("item-drawer-reference")
        .min_w(px(0.0))
        .truncate()
        .child(match url {
            Some(_) => format!("{} ↗", drawer.reference),
            None => drawer.reference.clone(),
        })
        .when_some(url, |s, url| {
            s.cursor_pointer()
                .hover(move |s| s.text_color(accent))
                .on_mouse_down(MouseButton::Left, move |_, _, _| {
                    crate::sidebar::open_url_in_browser(&url);
                })
        });
    let close = div()
        .id("item-drawer-close")
        .flex_shrink_0()
        .px(px(6.0))
        .text_size(px(14.0))
        .text_color(theme::ink_muted(theme))
        .cursor_pointer()
        .hover(|s| s.text_color(theme::ink(theme)))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.close_item_drawer(cx)),
        )
        .child("×");
    div()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .px(px(20.0))
        .pt(px(18.0))
        .pb(px(16.0))
        .border_b_1()
        .border_color(theme::divider(theme))
        .child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .text_size(px(11.0))
                .font(theme::font_mono())
                .text_color(theme::text_faint())
                .child(crate::board_panel::item_dot(drawer.kind, drawer.state, theme))
                .child(reference)
                .child(div().flex_grow())
                .child(close),
        )
        .child(
            div()
                .text_size(px(17.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::ink(theme))
                .child(drawer.title.clone()),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .children(drawer.column.clone().map(|column| {
                    chip(column.into(), theme::ink_muted(theme), theme::frost_20(theme))
                }))
                .children(drawer.round.map(|round| chip(round.into(), accent, accent))),
        )
}

fn chip(text: SharedString, color: Hsla, border: Hsla) -> impl IntoElement {
    div()
        .px(px(8.0))
        .py(px(2.0))
        .rounded(px(10.0))
        .border_1()
        .border_color(border)
        .text_size(px(11.0))
        .text_color(color)
        .child(text)
}

fn render_needs(
    theme: &Arc<Theme>,
    needs: &Needs,
    drawer: &ItemDrawer,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let warn = theme::signal_warn();
    let (board_url, item_id) = (drawer.board_url.clone(), drawer.item_id.clone());
    let action = needs.action;
    let primary_label = match action {
        CardAction::ReadyForReview => "Ready for review",
        CardAction::Decide => "Decide",
        CardAction::StartFromFork => "Start…",
    };
    let primary = div()
        .id("item-drawer-primary")
        .px(px(12.0))
        .py(px(7.0))
        .rounded(px(6.0))
        .bg(theme::accent(theme))
        .text_size(px(13.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(theme::canvas(theme))
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window: &mut Window, cx| match action {
                CardAction::ReadyForReview => this.ready_for_review(&item_id, cx),
                CardAction::Decide => this.open_decide_dialog(&item_id, window, cx),
                CardAction::StartFromFork => this.confirm_fork_start(&item_id, window, cx),
            }),
        )
        .child(primary_label);
    let item_id = drawer.item_id.clone();
    let open_tab = drawer.dispatched.then(|| {
        secondary_button(theme, "item-drawer-tab", "Open tab").on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                this.open_dispatch_tab(&board_url, &item_id, window, cx);
            }),
        )
    });
    let link = needs.link.clone().map(|(label, url)| {
        secondary_button(theme, "item-drawer-link", label).on_mouse_down(
            MouseButton::Left,
            move |_, _, _| crate::sidebar::open_url_in_browser(&url),
        )
    });
    div()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .p(px(12.0))
        .rounded(px(8.0))
        .bg(Hsla { a: 0.06, ..warn })
        .border_1()
        .border_color(Hsla { a: 0.35, ..warn })
        .child(
            div()
                .text_size(px(13.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(theme::ink(theme))
                .child(needs.title.clone()),
        )
        .child(
            div()
                .text_size(px(13.0))
                .text_color(theme::ink_muted(theme))
                .child(needs.text.clone()),
        )
        .child(
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(8.0))
                .child(primary)
                .children(open_tab)
                .children(link),
        )
        .child(
            div()
                .text_size(px(12.0))
                .text_color(theme::text_faint())
                .child(needs.hint.clone()),
        )
}

fn render_links(
    theme: &Arc<Theme>,
    drawer: &ItemDrawer,
    cx: &mut Context<AppShell>,
) -> Option<AnyElement> {
    if !drawer.dispatched && drawer.pull_request_url.is_none() {
        return None;
    }
    let (board_url, item_id) = (drawer.board_url.clone(), drawer.item_id.clone());
    let open_tab = drawer.dispatched.then(|| {
        secondary_button(theme, "item-drawer-tab", "Open tab").on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _, window, cx| {
                this.open_dispatch_tab(&board_url, &item_id, window, cx);
            }),
        )
    });
    let open_pr = drawer.pull_request_url.clone().map(|url| {
        secondary_button(theme, "item-drawer-pr", "Open PR").on_mouse_down(
            MouseButton::Left,
            move |_, _, _| crate::sidebar::open_url_in_browser(&url),
        )
    });
    let links = div()
        .flex()
        .flex_row()
        .flex_wrap()
        .gap(px(8.0))
        .children(open_tab)
        .children(open_pr);
    Some(links.into_any_element())
}

fn secondary_button(
    theme: &Arc<Theme>,
    id: &'static str,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .px(px(12.0))
        .py(px(7.0))
        .rounded(px(6.0))
        .border_1()
        .border_color(theme::frost_20(theme))
        .text_size(px(13.0))
        .text_color(theme::ink(theme))
        .cursor_pointer()
        .hover(|s| s.border_color(theme::frost_50(theme)))
        .child(label)
}

fn section(heading: &'static str) -> gpui::Div {
    div().flex().flex_col().gap(px(8.0)).child(
        div()
            .text_size(px(11.0))
            .text_color(theme::text_faint())
            .child(heading),
    )
}

fn render_rows(theme: &Arc<Theme>, drawer: &ItemDrawer) -> impl IntoElement {
    let heading = if drawer.dispatched { "DISPATCH" } else { "ITEM" };
    section(heading)
        .children(drawer.rows.iter().map(|row| {
            let mono = matches!(row.label, "Worktree" | "Branch");
            let value = div()
                .min_w(px(0.0))
                .flex_1()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(6.0))
                .text_color(theme::ink(theme))
                .child(
                    div()
                        .min_w(px(0.0))
                        .when(mono, |s| s.font(theme::font_mono()))
                        .child(row.value.clone()),
                )
                .children(row.mark.as_ref().map(|(text, mark)| {
                    div().text_color(mark_color(theme, *mark)).child(text.clone())
                }));
            div()
                .flex()
                .flex_row()
                .gap(px(12.0))
                .text_size(px(12.0))
                .child(
                    div()
                        .w(px(LABEL_WIDTH))
                        .flex_shrink_0()
                        .text_color(theme::text_faint())
                        .child(row.label),
                )
                .child(value)
        }))
}

fn render_details(theme: &Arc<Theme>, details: &DetailsView) -> Vec<AnyElement> {
    let line = |text: String, color: Hsla| {
        div().text_size(px(12.0)).text_color(color).child(text).into_any_element()
    };
    match details {
        DetailsView::None => Vec::new(),
        DetailsView::Reading => {
            vec![line("Reading its findings and its moves…".into(), theme::text_faint())]
        }
        DetailsView::Failed(message) => vec![line(
            format!("Couldn't read its findings and its moves: {message}"),
            theme::danger(),
        )],
        DetailsView::Read(read) => read
            .findings
            .iter()
            .map(|findings| render_findings(theme, findings).into_any_element())
            .chain([render_moves(theme, &read.moves).into_any_element()])
            .collect(),
    }
}

fn render_findings(theme: &Arc<Theme>, findings: &FindingsView) -> impl IntoElement {
    let accent = theme::accent(theme);
    let url = findings.url.clone();
    let open = div()
        .id("item-drawer-findings")
        .text_size(px(11.0))
        .text_color(theme::ink_muted(theme))
        .cursor_pointer()
        .hover(move |s| s.text_color(accent))
        .on_mouse_down(MouseButton::Left, move |_, _, _| {
            crate::sidebar::open_url_in_browser(&url);
        })
        .child("Open ↗");
    let heading = div()
        .flex()
        .flex_row()
        .items_center()
        .child(
            div()
                .text_size(px(11.0))
                .text_color(theme::text_faint())
                .child("FINDINGS"),
        )
        .child(div().flex_grow())
        .child(open);
    let summary = match &findings.summary {
        Some((cause, approach)) => vec![
            labeled("Cause", render_paragraphs(theme, cause)).into_any_element(),
            labeled("Approach", render_paragraphs(theme, approach)).into_any_element(),
        ],
        None => vec![
            div()
                .text_size(px(12.0))
                .text_color(theme::text_faint())
                .child("Posted before the last 50 comments; open them on GitHub.")
                .into_any_element(),
        ],
    };
    div()
        .flex()
        .flex_col()
        .gap(px(10.0))
        .child(heading)
        .children(summary)
        .children(findings.points.iter().enumerate().map(|(index, point)| {
            let by = match &point.answered {
                Some((who, url)) => {
                    let url = url.clone();
                    div()
                        .id(("item-drawer-answer", index))
                        .text_color(theme::ink_muted(theme))
                        .cursor_pointer()
                        .hover(move |s| s.text_color(accent))
                        .on_mouse_down(MouseButton::Left, move |_, _, _| {
                            crate::sidebar::open_url_in_browser(&url);
                        })
                        .child(format!("chosen by {who} ↗"))
                        .into_any_element()
                }
                None => div()
                    .text_color(theme::text_faint())
                    .child("advised, not answered yet")
                    .into_any_element(),
            };
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .text_size(px(12.0))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::ink(theme))
                        .child(point.question.clone()),
                )
                .child(div().text_color(theme::ink(theme)).child(point.choice.clone()))
                .child(by)
        }))
}

fn render_moves(theme: &Arc<Theme>, moves: &[MoveRow]) -> impl IntoElement {
    let none = moves.is_empty().then(|| {
        div()
            .text_size(px(12.0))
            .text_color(theme::text_faint())
            .child("It hasn't moved between columns.")
    });
    section("ON THE BOARD")
        .children(none)
        .children(moves.iter().map(|row| {
            div()
                .flex()
                .flex_row()
                .flex_wrap()
                .gap(px(8.0))
                .text_size(px(12.0))
                .child(div().text_color(theme::ink(theme)).child(format!("→ {}", row.column)))
                .child(div().text_color(theme::text_faint()).child(row.by.clone()))
        }))
}

fn labeled(label: &'static str, value: impl IntoElement) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap(px(12.0))
        .text_size(px(12.0))
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_shrink_0()
                .text_color(theme::text_faint())
                .child(label),
        )
        .child(div().min_w(px(0.0)).flex_1().child(value))
}

fn render_paragraphs(theme: &Arc<Theme>, paragraphs: &[Paragraph]) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(6.0))
        .text_size(px(12.0))
        .text_color(theme::ink(theme))
        .children(paragraphs.chunk_by(|a, b| a.code && b.code).map(|run| {
            if !run[0].code {
                return div().child(run[0].text.clone()).into_any_element();
            }
            div()
                .px(px(8.0))
                .py(px(6.0))
                .rounded(px(4.0))
                .bg(theme::frost_10(theme))
                .font(theme::font_mono())
                .text_size(px(11.0))
                .text_color(theme::ink_muted(theme))
                .children(run.iter().map(|line| match line.text.as_str() {
                    "" => div().child(" "),
                    text => div().child(text.to_string()),
                }))
                .into_any_element()
        }))
}

fn mark_color(theme: &Arc<Theme>, mark: Mark) -> Hsla {
    match mark {
        Mark::Good => theme::signal_ok(),
        Mark::Bad => theme::danger(),
        Mark::Busy => theme::signal_review(),
        Mark::Plain => theme::ink_muted(theme),
    }
}

fn tone_color(theme: &Arc<Theme>, tone: NoteTone) -> Hsla {
    match tone {
        NoteTone::Working => theme::accent(theme),
        NoteTone::Waiting => theme::ink_muted(theme),
        NoteTone::NeedsYou => theme::signal_review(),
        NoteTone::Failed => theme::danger(),
    }
}
