//! Needs-you dialog for decision points (ADR-0027): one option per point with the advice
//! selected, an optional note, and *Move to Ready afterwards*. *Send answer* posts the choices
//! on the issue; a ticked box then moves the item to Ready, and the next board read starts
//! implementation.

use std::sync::Arc;

use codescope_core::Theme;
use codescope_core::board::answers::choices_text;
use codescope_core::board::findings::{DecisionPoint, plain_text};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, Context, FocusHandle, Hsla, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, SharedString, StatefulInteractiveElement, Styled,
    Window, anchored, deferred, div, point, px,
};

use crate::app::AppShell;
use crate::app::board_dispatch::{AnswerPost, Decision};
use crate::text_field::{TextField, focused_caret_style, render_input_content};
use crate::theme;

pub struct DecideDialogState {
    pub focus_handle: FocusHandle,
    pub decision: Decision,
    pub choices: Vec<char>,
    pub note: TextField,
    pub move_to_ready: bool,
    pub sending: bool,
    pub error: Option<String>,
}

impl DecideDialogState {
    fn chosen(&self) -> Vec<(String, char)> {
        self.decision
            .points
            .iter()
            .zip(&self.choices)
            .map(|(point, letter)| (point.id.clone(), *letter))
            .collect()
    }
}

impl AppShell {
    /// Open the dialog for a card whose decision points are known; else nothing happens.
    pub fn open_decide_dialog(
        &mut self,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.decide_dialog.is_some() {
            return;
        }
        let Some(decision) = self.decision(item_id, cx) else {
            return;
        };
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        self.decide_dialog = Some(DecideDialogState {
            focus_handle,
            choices: decision.choices.clone(),
            decision,
            note: TextField::new(),
            move_to_ready: true,
            sending: false,
            error: None,
        });
        cx.notify();
    }

    pub fn cancel_decide_dialog(&mut self, cx: &mut Context<Self>) {
        if self.decide_dialog.as_ref().is_some_and(|d| !d.sending) {
            self.decide_dialog = None;
            cx.notify();
        }
    }

    fn choose_option(&mut self, point: usize, letter: char, cx: &mut Context<Self>) {
        if let Some(dialog) = self.decide_dialog.as_mut().filter(|d| !d.sending)
            && let Some(choice) = dialog.choices.get_mut(point)
        {
            *choice = letter;
            cx.notify();
        }
    }

    fn toggle_move_to_ready(&mut self, cx: &mut Context<Self>) {
        if let Some(dialog) = self.decide_dialog.as_mut().filter(|d| !d.sending) {
            dialog.move_to_ready = !dialog.move_to_ready;
            cx.notify();
        }
    }

    pub fn submit_decide_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(dialog) = self.decide_dialog.as_mut() else {
            return;
        };
        if dialog.sending {
            return;
        }
        dialog.sending = true;
        dialog.error = None;
        let post = AnswerPost {
            item_id: dialog.decision.item_id.clone(),
            board_url: dialog.decision.board_url.clone(),
            content_id: dialog.decision.content_id.clone(),
            number: dialog.decision.number,
            choices: dialog.chosen(),
            note: dialog.note.text().to_string(),
            move_to_ready: dialog.move_to_ready,
        };
        cx.notify();
        self.send_answer(post, cx);
    }

    fn edit_decide_note(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut TextField) -> bool,
    ) {
        let Some(dialog) = self.decide_dialog.as_mut() else {
            return;
        };
        if dialog.sending || !edit(&mut dialog.note) {
            return;
        }
        dialog.error = None;
        self.wake_text_blink(cx);
        cx.notify();
    }

    pub fn render_decide_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.decide_dialog.as_ref()?;
        let decision = &dialog.decision;
        let viewport = window.viewport_size();
        let ink = theme::ink(theme);
        let muted = theme::ink_muted(theme);
        let faint = theme::text_faint();
        let divider = theme::divider(theme);
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);
        let frost = theme::frost_10(theme);

        let count = decision.points.len();
        let choices_word = if count == 1 { "one choice" } else { "choices" };
        let findings_url = decision.findings_url.clone();
        let header = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .px_5()
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
                        "#{} · {} · Needs decision",
                        decision.number, decision.repository
                    ))),
            )
            .child(
                div()
                    .text_size(px(18.0))
                    .text_color(ink)
                    .child(decision.title.clone()),
            )
            .child(div().text_size(px(12.0)).text_color(muted).child(format!(
                "Research found the cause and left {choices_word}. Your answer goes onto the \
                 issue and, when it moves to Ready, to the session."
            )))
            .child(
                div()
                    .id("decide-findings")
                    .text_size(px(12.0))
                    .text_color(accent)
                    .cursor_pointer()
                    .hover(|s| s.underline())
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        cx.stop_propagation();
                        crate::sidebar::open_url_in_browser(&findings_url);
                    })
                    .child("Read the findings on GitHub"),
            );

        let points = div()
            .id("decide-points")
            .flex()
            .flex_col()
            .gap_4()
            .px_5()
            .max_h(viewport.height * 0.55)
            .overflow_y_scroll()
            .children(decision.points.iter().enumerate().map(|(index, point)| {
                let selected = dialog.choices.get(index).copied();
                render_point(theme, index, point, selected, dialog.sending, cx).into_any_element()
            }));

        let note_field = div()
            .id("decide-note")
            .px_3()
            .h(px(34.0))
            .bg(canvas)
            .border_1()
            .border_color(accent)
            .rounded(px(6.0))
            .text_size(px(13.0))
            .flex()
            .items_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if let Some(dialog) = this.decide_dialog.as_mut()
                        && let Some(index) = dialog.note.index_for_window_point(event.position)
                    {
                        dialog.note.set_caret(index);
                        this.wake_text_blink(cx);
                        cx.notify();
                    }
                }),
            )
            .child(render_input_content(
                &dialog.note,
                SharedString::from("Anything the agent should know"),
                focused_caret_style(theme, self.text_blink_phase),
            ));
        let note = div()
            .px_5()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .text_size(px(12.0))
                    .child(div().text_color(ink).child("Note"))
                    .child(div().text_color(faint).child("optional")),
            )
            .child(note_field);

        let ready_box = div()
            .id("decide-ready")
            .px_5()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .text_size(px(12.0))
            .text_color(ink)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.toggle_move_to_ready(cx);
                }),
            )
            .child(checkbox(dialog.move_to_ready, accent, divider, canvas))
            .child("Move to Ready afterwards, so implementation starts");

        let message: Option<(SharedString, Hsla)> = match (&dialog.error, dialog.sending) {
            (_, true) => Some(("Posting the answer…".into(), theme::ink_dim(theme))),
            (Some(err), false) => Some((err.clone().into(), theme::danger())),
            (None, false) => None,
        };

        let later = div()
            .id("decide-later")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(theme::ink_dim(theme))
            .border_1()
            .border_color(divider)
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |s| s.bg(frost).text_color(ink))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.cancel_decide_dialog(cx);
                }),
            )
            .child("Later");
        let can_send = !dialog.sending;
        let mut send = div()
            .id("decide-send")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(if can_send { canvas } else { theme::ink_ghost(theme) })
            .bg(if can_send { accent } else { divider })
            .rounded(px(6.0))
            .child("Send answer");
        if can_send {
            send = send.cursor_pointer().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.submit_decide_dialog(cx);
                }),
            );
        }
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .px_5()
            .pb_5()
            .child(
                div()
                    .flex_1()
                    .text_size(px(11.0))
                    .text_color(faint)
                    .child(format!(
                        "Posts “{}” on #{}. Anyone with write access can answer there too.",
                        choices_text(&dialog.chosen()),
                        decision.number
                    )),
            )
            .child(later)
            .child(send);

        let card = div()
            .flex()
            .flex_col()
            .gap_4()
            .w(px(580.0))
            .bg(theme::elevated(theme))
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .track_focus(&dialog.focus_handle)
            .key_context("DecideDialog")
            .on_key_down(cx.listener(handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(points)
            .child(note)
            .child(ready_box)
            .children(message.map(|(text, color)| {
                div()
                    .px_5()
                    .text_size(px(12.0))
                    .text_color(color)
                    .child(text)
            }))
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
                cx.listener(|this, _, _, cx| this.cancel_decide_dialog(cx)),
            )
            .child(card);

        Some(
            deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
                .with_priority(10)
                .into_any_element(),
        )
    }
}

fn render_point(
    theme: &Arc<Theme>,
    index: usize,
    point: &DecisionPoint,
    selected: Option<char>,
    sending: bool,
    cx: &mut Context<AppShell>,
) -> impl IntoElement {
    let ink = theme::ink(theme);
    let muted = theme::ink_muted(theme);
    let accent = theme::accent(theme);
    let divider = theme::divider(theme);
    let frost = theme::frost_10(theme);
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_2()
                .child(
                    div()
                        .flex_shrink_0()
                        .px(px(6.0))
                        .rounded(px(4.0))
                        .bg(theme::with_alpha(theme.chrome.accent, 0.16))
                        .text_size(px(11.0))
                        .font(theme::font_mono())
                        .text_color(accent)
                        .child(point.id.clone()),
                )
                .child(
                    div()
                        .text_size(px(13.0))
                        .text_color(ink)
                        .child(plain_text(&point.question)),
                ),
        )
        .when(!point.context.is_empty(), |s| {
            s.child(
                div()
                    .text_size(px(12.0))
                    .text_color(muted)
                    .child(plain_text(&point.context)),
            )
        })
        .children(point.options.iter().enumerate().map(|(position, option)| {
            let letter = option.letter;
            let on = selected == Some(letter);
            let (label, detail) = option.label_and_detail();
            let mut row = div()
                .id(("decide-option", index * 32 + position))
                .flex()
                .flex_row()
                .items_start()
                .gap_2()
                .px_3()
                .py_2()
                .rounded(px(6.0))
                .border_1()
                .border_color(if on { accent } else { divider })
                .child(radio(on, accent, divider))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_size(px(12.5))
                                .text_color(ink)
                                .child(format!("{letter} · {label}"))
                                .when(letter == point.advice, |s| {
                                    s.child(
                                        div()
                                            .text_size(px(10.0))
                                            .font(theme::font_mono())
                                            .text_color(theme::signal_ok())
                                            .child("ADVICE"),
                                    )
                                }),
                        )
                        .when(!detail.is_empty(), |s| {
                            s.child(div().text_size(px(11.5)).text_color(muted).child(detail))
                        }),
                );
            if !sending {
                row = row
                    .cursor_pointer()
                    .hover(move |s| s.bg(frost))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.choose_option(index, letter, cx);
                        }),
                    );
            }
            row
        }))
}

fn radio(on: bool, accent: Hsla, divider: Hsla) -> impl IntoElement {
    div()
        .mt(px(2.0))
        .flex_shrink_0()
        .size(px(12.0))
        .rounded_full()
        .border_1()
        .border_color(if on { accent } else { divider })
        .flex()
        .items_center()
        .justify_center()
        .when(on, |s| {
            s.child(div().size(px(6.0)).rounded_full().bg(accent))
        })
}

fn checkbox(on: bool, accent: Hsla, divider: Hsla, canvas: Hsla) -> impl IntoElement {
    div()
        .flex_shrink_0()
        .size(px(14.0))
        .rounded(px(3.0))
        .border_1()
        .border_color(if on { accent } else { divider })
        .bg(if on { accent } else { canvas })
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(10.0))
        .text_color(canvas)
        .when(on, |s| s.child("✓"))
}

fn handle_key_down(
    shell: &mut AppShell,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) {
    cx.stop_propagation();
    let keystroke = &event.keystroke;
    if crate::text_field::is_paste_chord(keystroke) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let line = text.replace(['\r', '\n'], " ");
            shell.edit_decide_note(cx, |field| field.insert_str(&line));
        }
        return;
    }
    match keystroke.key.as_str() {
        "escape" => shell.cancel_decide_dialog(cx),
        "enter" => shell.submit_decide_dialog(cx),
        "backspace" => shell.edit_decide_note(cx, TextField::backspace),
        "delete" => shell.edit_decide_note(cx, TextField::delete_forward),
        "left" => shell.edit_decide_note(cx, TextField::move_left),
        "right" => shell.edit_decide_note(cx, TextField::move_right),
        "home" => shell.edit_decide_note(cx, TextField::move_home),
        "end" => shell.edit_decide_note(cx, TextField::move_end),
        "space" => shell.edit_decide_note(cx, |field| {
            field.insert_char(' ');
            true
        }),
        _ => {
            let typed: String = keystroke
                .key_char
                .as_deref()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            if !typed.is_empty() {
                shell.edit_decide_note(cx, |field| field.insert_str(&typed));
            }
        }
    }
}
