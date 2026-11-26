//! Add-board dialog: paste a GitHub Projects board URL; CodeScope reads the board once, maps
//! its columns to roles by name, and adds it to `settings.json` (ADR-0027).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use codescope_core::board::{Board, BoardRef, BoardSettings, github, suggest_mapping};
use codescope_core::{Settings, Theme};
use gpui::{
    AppContext as _, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, SharedString, Styled, Window, anchored, deferred,
    div, point, px,
};

use crate::app::AppShell;
use crate::text_field::{TextField, focused_caret_style, render_input_content};
use crate::theme;

static NEXT_READ: AtomicU64 = AtomicU64::new(1);

pub struct AddBoardDialogState {
    pub focus_handle: FocusHandle,
    pub url: TextField,
    pub error: Option<String>,
    /// The read in flight; a result for another read belongs to a dialog that was closed.
    pub reading: Option<u64>,
}

fn is_added(board: &BoardRef, existing: &[BoardSettings]) -> bool {
    existing
        .iter()
        .any(|b| BoardRef::parse_url(&b.url).is_some_and(|known| known.same_board(board)))
}

fn validate(input: &str, existing: &[BoardSettings]) -> Result<BoardRef, String> {
    let board = BoardRef::parse_url(input).ok_or_else(|| {
        "That isn't a board URL. It looks like https://github.com/users/<you>/projects/<number>."
            .to_string()
    })?;
    if is_added(&board, existing) {
        return Err("This board is already added.".to_string());
    }
    Ok(board)
}

impl AppShell {
    pub fn open_add_board_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.add_board_dialog.is_some() {
            return;
        }
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        self.add_board_dialog = Some(AddBoardDialogState {
            focus_handle,
            url: TextField::new(),
            error: None,
            reading: None,
        });
        cx.notify();
    }

    pub fn cancel_add_board_dialog(&mut self, cx: &mut Context<Self>) {
        if self.add_board_dialog.take().is_some() {
            cx.notify();
        }
    }

    pub fn submit_add_board_dialog(&mut self, cx: &mut Context<Self>) {
        let existing = self.settings.boards.clone();
        let Some(state) = self.add_board_dialog.as_mut() else {
            return;
        };
        if state.reading.is_some() {
            return;
        }
        let board = match validate(state.url.text(), &existing) {
            Ok(board) => board,
            Err(err) => {
                state.error = Some(err);
                cx.notify();
                return;
            }
        };
        let read = NEXT_READ.fetch_add(1, Ordering::Relaxed);
        state.reading = Some(read);
        state.error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let wanted = board.clone();
            let result = cx
                .background_spawn(async move {
                    github::fetch_board(&wanted).map_err(|err| format!("{err:#}"))
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_add_board(read, board, result, cx));
        })
        .detach();
    }

    fn finish_add_board(
        &mut self,
        read: u64,
        wanted: BoardRef,
        result: Result<Board, String>,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = self
            .add_board_dialog
            .as_mut()
            .filter(|state| state.reading == Some(read))
        else {
            return;
        };
        state.reading = None;
        let board = match result {
            Ok(board) => board,
            Err(err) => {
                state.error = Some(format!("Couldn't read the board: {err}"));
                cx.notify();
                return;
            }
        };
        let url = wanted.url();
        let saved = Settings::update(self.paths_ref(), |settings| {
            if !is_added(&wanted, &settings.boards) {
                settings.boards.push(BoardSettings {
                    url: url.clone(),
                    columns: suggest_mapping(&board.columns),
                    added_at: codescope_core::time::now_iso8601(),
                    ..BoardSettings::default()
                });
            }
        });
        let settings = match saved {
            Ok(settings) => settings,
            Err(err) => {
                if let Some(state) = self.add_board_dialog.as_mut() {
                    state.error = Some(format!("Couldn't save settings: {err:#}"));
                }
                cx.notify();
                return;
            }
        };
        self.add_board_dialog = None;
        self.apply_settings(settings, cx);
        self.accept_board_read(url, board);
        self.set_show_board(true, cx);
        cx.notify();
    }

    fn edit_add_board_url(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut TextField) -> bool,
    ) {
        let Some(state) = self.add_board_dialog.as_mut() else {
            return;
        };
        if state.reading.is_some() || !edit(&mut state.url) {
            return;
        }
        state.error = None;
        self.wake_text_blink(cx);
        cx.notify();
    }

    pub fn render_add_board_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.add_board_dialog.as_ref()?;
        let viewport = window.viewport_size();

        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let frost = theme::frost_10(theme);
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);
        let can_submit = state.reading.is_none() && !state.url.text().trim().is_empty();

        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_5()
            .pt_5()
            .child(div().text_size(px(11.0)).text_color(accent).child("BOARD"))
            .child(div().text_size(px(20.0)).text_color(ink).child("Add board"))
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::ink_muted(theme))
                    .child(
                        "Paste the URL of a GitHub Projects board. CodeScope reads it every 30 \
                     seconds and finds the Research, Ready and progress columns by name.",
                    ),
            );

        let textbox = div()
            .id("add-board-input")
            .px_3()
            .h(px(34.0))
            .bg(canvas)
            .border_1()
            .border_color(accent)
            .rounded(px(6.0))
            .text_size(px(13.0))
            .font(theme::font_mono())
            .flex()
            .items_center()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    cx.stop_propagation();
                    if let Some(state) = this.add_board_dialog.as_mut()
                        && let Some(index) = state.url.index_for_window_point(event.position)
                    {
                        state.url.set_caret(index);
                        this.wake_text_blink(cx);
                        cx.notify();
                    }
                }),
            )
            .child(render_input_content(
                &state.url,
                SharedString::from("https://github.com/users/<you>/projects/<number>"),
                focused_caret_style(theme, self.text_blink_phase),
            ));

        let body = div()
            .px_5()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(ink_ghost)
                    .child("Board URL"),
            )
            .child(textbox);

        let message: Option<(SharedString, gpui::Hsla)> =
            match (&state.error, state.reading.is_some()) {
                (_, true) => Some(("Reading the board…".into(), ink_dim)),
                (Some(err), false) => Some((err.clone().into(), theme::danger())),
                (None, false) => None,
            };

        let cancel_btn = div()
            .id("add-board-cancel")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(ink_dim)
            .border_1()
            .border_color(divider)
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |s| s.bg(frost).text_color(ink))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.cancel_add_board_dialog(cx);
                }),
            )
            .child("Cancel");

        let mut add_btn = div()
            .id("add-board-ok")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(if can_submit { canvas } else { ink_ghost })
            .bg(if can_submit { accent } else { divider })
            .rounded(px(6.0));
        if can_submit {
            add_btn = add_btn.cursor_pointer().on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.submit_add_board_dialog(cx);
                }),
            );
        }

        let card = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(520.0))
            .bg(theme::elevated(theme))
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .pb_2()
            .track_focus(&state.focus_handle)
            .key_context("AddBoardDialog")
            .on_key_down(cx.listener(handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(body)
            .children(message.map(|(text, color)| {
                div()
                    .px_5()
                    .text_size(px(12.0))
                    .text_color(color)
                    .child(text)
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .px_5()
                    .pb_5()
                    .child(cancel_btn)
                    .child(add_btn.child("Add board")),
            );

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
                cx.listener(|this, _, _, cx| this.cancel_add_board_dialog(cx)),
            )
            .child(card);

        Some(
            deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
                .with_priority(10)
                .into_any_element(),
        )
    }
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
            shell.edit_add_board_url(cx, |field| field.insert_str(text.trim()));
        }
        return;
    }
    match keystroke.key.as_str() {
        "escape" => shell.cancel_add_board_dialog(cx),
        "enter" => shell.submit_add_board_dialog(cx),
        "backspace" => shell.edit_add_board_url(cx, TextField::backspace),
        "delete" => shell.edit_add_board_url(cx, TextField::delete_forward),
        "left" => shell.edit_add_board_url(cx, TextField::move_left),
        "right" => shell.edit_add_board_url(cx, TextField::move_right),
        "home" => shell.edit_add_board_url(cx, TextField::move_home),
        "end" => shell.edit_add_board_url(cx, TextField::move_end),
        _ => {
            let typed: String = keystroke
                .key_char
                .as_deref()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control() && !c.is_whitespace())
                .collect();
            if !typed.is_empty() {
                shell.edit_add_board_url(cx, |field| field.insert_str(&typed));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_a_repository_url() {
        let err = validate("https://github.com/maui1911/CodeScope", &[]).unwrap_err();
        assert!(err.starts_with("That isn't a board URL"), "{err}");
    }

    #[test]
    fn validate_rejects_a_board_already_added_under_another_spelling() {
        let existing = [BoardSettings {
            url: "https://github.com/users/maui1911/projects/3".into(),
            ..BoardSettings::default()
        }];
        let err = validate("github.com/users/maui1911/projects/3/views/1", &existing).unwrap_err();
        assert_eq!(err, "This board is already added.");
    }

    #[test]
    fn validate_rejects_a_board_already_added_with_other_casing() {
        let existing = [BoardSettings {
            url: "https://github.com/users/maui1911/projects/3".into(),
            ..BoardSettings::default()
        }];
        let err = validate("https://github.com/users/Maui1911/projects/3", &existing).unwrap_err();
        assert_eq!(err, "This board is already added.");
    }

    #[test]
    fn validate_accepts_a_new_board() {
        let board = validate("https://github.com/orgs/acme/projects/7", &[]).unwrap();
        assert_eq!(board.url(), "https://github.com/orgs/acme/projects/7");
    }
}
