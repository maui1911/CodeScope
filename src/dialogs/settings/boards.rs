//! The Boards page of the Settings dialog (ADR-0027): the boards CodeScope reads, how many
//! dispatches run at once, and which board events notify the user.

use std::collections::BTreeSet;
use std::sync::Arc;

use codescope_core::Theme;
use codescope_core::board::BoardRef;
use codescope_core::board::attention::Attention;
use codescope_core::board::github::GhSignIn;
use gpui::{
    AnyElement, AppContext as _, Context, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, Styled, div, px,
};

use super::SettingsDialogState;
use crate::app::AppShell;
use crate::board_panel::{BOARD_READ_INTERVAL, board_label, read_age};
use crate::theme;

/// The most dispatches the dialog lets run at once.
pub(super) const MAX_DISPATCH_LIMIT: usize = 20;

/// What the Boards page knows of how `gh` is signed in.
pub enum SignInRead {
    /// The page hasn't been shown yet, so nothing was asked.
    NotAsked,
    /// `gh auth status` runs.
    Reading,
    /// Its answer; `None` when this `gh` can't answer as JSON.
    Read(Option<GhSignIn>),
}

/// Parse the *Dispatches at the same time* field.
pub(super) fn parse_dispatch_limit(text: &str) -> Result<usize, String> {
    let message = || {
        format!(
            "Dispatches at the same time must be a whole number from 1 to {MAX_DISPATCH_LIMIT}."
        )
    };
    let limit: usize = text.trim().parse().map_err(|_| message())?;
    if !(1..=MAX_DISPATCH_LIMIT).contains(&limit) {
        return Err(message());
    }
    Ok(limit)
}

/// The limit one step up or down from `text`, kept from 1 to [`MAX_DISPATCH_LIMIT`]; text that
/// isn't a number steps from `fallback`.
pub(super) fn step_limit(text: &str, fallback: usize, up: bool) -> usize {
    let current: usize = text.trim().parse().unwrap_or(fallback);
    let next = if up { current.saturating_add(1) } else { current.saturating_sub(1) };
    next.clamp(1, MAX_DISPATCH_LIMIT)
}

/// GitHub API points an hour that reading these boards costs: a point per 100 items of a board,
/// at least one, every [`BOARD_READ_INTERVAL`]; a board not read yet counts one.
fn points_an_hour(item_counts: &[Option<usize>]) -> usize {
    let reads_an_hour = 3600 / BOARD_READ_INTERVAL.as_secs() as usize;
    let per_read: usize = item_counts.iter().map(|n| n.unwrap_or(0).div_ceil(100).max(1)).sum();
    per_read * reads_an_hour
}

/// The banner over the boards: whether it is good news, and what it says.
fn sign_in_banner(sign_in: &GhSignIn) -> (bool, String) {
    let has = |scopes: &[String], scope: &str| scopes.iter().any(|s| s == scope);
    match sign_in {
        GhSignIn::Active { login, scopes } if scopes.is_empty() => {
            (true, format!("GitHub CLI signed in as {login}."))
        }
        GhSignIn::Active { login, scopes } if has(scopes, "project") => (
            true,
            format!(
                "GitHub CLI signed in as {login}. The token has the project scope, so CodeScope \
                 can read and move items."
            ),
        ),
        GhSignIn::Active { login, scopes } => {
            let can = if has(scopes, "read:project") {
                "can read items but not move them"
            } else {
                "can't read or move items"
            };
            (
                false,
                format!(
                    "GitHub CLI signed in as {login}, but the token lacks the project scope, so \
                     CodeScope {can}. Run gh auth refresh -s project."
                ),
            )
        }
        GhSignIn::BadToken => {
            (false, "The GitHub CLI's token doesn't work. Run gh auth login.".into())
        }
        GhSignIn::SignedOut => {
            (false, "The GitHub CLI isn't signed in to github.com. Run gh auth login.".into())
        }
    }
}

/// The label and hint of each *Notify me when* box.
fn notify_text(attention: Attention) -> (&'static str, &'static str) {
    match attention {
        Attention::Findings => (
            "Findings are ready",
            "Research posted its findings; the item is in Needs decision.",
        ),
        Attention::NeedsYou => (
            "A dispatch needs you",
            "A choice before a round can start, a round stopped without its result, a draft \
             pull request to test by hand, or a repair that needs you.",
        ),
        Attention::MayBeWaiting => (
            "A session may be waiting for you",
            "Nothing new in its session for 3 minutes: often a question or a permission prompt.",
        ),
    }
}

fn counted(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

impl AppShell {
    /// Ask `gh` how it is signed in, the first time the Boards page shows.
    pub(super) fn read_sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.settings_dialog.as_mut() else { return };
        if !matches!(state.sign_in, SignInRead::NotAsked) {
            return;
        }
        state.sign_in = SignInRead::Reading;
        cx.spawn(async move |this, cx| {
            let read = cx
                .background_spawn(async move { codescope_core::board::github::fetch_sign_in() })
                .await;
            let _ = this.update(cx, |this, cx| {
                if let Some(state) = this.settings_dialog.as_mut() {
                    state.sign_in = SignInRead::Read(read);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub(super) fn render_boards_page(
        &self,
        state: &SettingsDialogState,
        limit_input: impl IntoElement,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let ink = theme::ink(theme);
        let muted = theme::ink_muted(theme);
        let ghost = theme::ink_ghost(theme);
        let divider = theme::divider(theme);
        let canvas = theme::canvas(theme);
        let accent = theme::accent(theme);
        let frost = theme::frost_10(theme);

        let heading = |text: &'static str| {
            div()
                .text_size(px(13.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(ink)
                .child(text)
        };
        let note = |text: SharedString| div().text_size(px(12.0)).text_color(ghost).child(text);
        let button = |id: (&'static str, usize), label: &'static str, color: Hsla| {
            div()
                .id(id)
                .flex_shrink_0()
                .px(px(10.0))
                .py(px(6.0))
                .border_1()
                .border_color(divider)
                .rounded(px(6.0))
                .text_size(px(12.0))
                .text_color(color)
                .cursor_pointer()
                .hover(move |s| s.bg(frost))
                .child(label)
        };

        let banner = match &state.sign_in {
            SignInRead::Read(Some(sign_in)) => {
                let (good, text) = sign_in_banner(sign_in);
                let color = if good { theme::signal_ok() } else { theme::signal_review() };
                Some(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .px_3()
                        .py(px(10.0))
                        .bg(Hsla { a: 0.08, ..color })
                        .border_1()
                        .border_color(Hsla { a: 0.35, ..color })
                        .rounded(px(8.0))
                        .text_size(px(12.0))
                        .text_color(ink)
                        .child(div().text_color(color).child(if good { "✓" } else { "!" }))
                        .child(div().flex_1().min_w(px(0.0)).child(text)),
                )
            }
            SignInRead::NotAsked | SignInRead::Reading | SignInRead::Read(None) => None,
        };

        let boards = &self.settings_ref().boards;
        let mut item_counts = Vec::with_capacity(boards.len());
        let rows = boards.iter().enumerate().map(|(i, settings)| {
            let read = self.board.reads_by_url.get(&settings.url);
            let board = read.and_then(|r| r.last_good.as_ref());
            item_counts.push(board.map(|b| b.items.len()));
            let owner = BoardRef::parse_url(&settings.url).map(|r| r.owner).unwrap_or_default();
            let summary: SharedString = match board {
                Some(board) => {
                    let repositories: BTreeSet<&str> =
                        board.items.iter().filter_map(|item| item.repository.as_deref()).collect();
                    format!(
                        "{} · {} · {}",
                        counted(board.items.len(), "item", "items"),
                        counted(repositories.len(), "repository", "repositories"),
                        read_age(read.and_then(|r| r.read_at)),
                    )
                    .into()
                }
                None => match read.and_then(|r| r.error.as_deref()) {
                    Some(error) => format!("couldn't read it: {error}").into(),
                    None => "not read yet".into(),
                },
            };
            let map_url = settings.url.clone();
            let remove_url = settings.url.clone();
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .p_3()
                .bg(canvas)
                .border_1()
                .border_color(divider)
                .rounded(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(3.0))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(
                                    div()
                                        .min_w(px(0.0))
                                        .truncate()
                                        .text_size(px(13.0))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(ink)
                                        .child(board_label(settings, read)),
                                )
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_size(px(13.0))
                                        .text_color(muted)
                                        .child(owner),
                                ),
                        )
                        .child(
                            div()
                                .truncate()
                                .font(theme::font_mono())
                                .text_size(px(11.0))
                                .text_color(ghost)
                                .child(summary),
                        ),
                )
                .children(board.is_some().then(|| {
                    button(("settings-board-map", i), "Map columns", ink).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_board_settings_dialog(&map_url, window, cx);
                        }),
                    )
                }))
                .child(
                    button(("settings-board-remove", i), "Remove", theme::danger()).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.remove_board(remove_url.clone(), window, cx);
                        }),
                    ),
                )
        });
        let rows: Vec<_> = rows.collect();
        let cost = (!boards.is_empty()).then(|| {
            note(
                format!(
                    "Each board is read every {} s while CodeScope runs: about {} of the 5,000 \
                     GitHub API points an hour.",
                    BOARD_READ_INTERVAL.as_secs(),
                    points_an_hour(&item_counts),
                )
                .into(),
            )
        });
        let boards_section = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .child(div().flex_1().child(heading("Boards")))
                    .child(button(("settings-board-add", 0), "+ Add board", ink).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            cx.stop_propagation();
                            this.open_add_board_dialog(window, cx);
                        }),
                    )),
            )
            .children(rows)
            .children(boards.is_empty().then(|| {
                note("No boards yet. Add one by pasting its URL from GitHub.".into())
            }))
            .children(cost);

        let step = |id: &'static str, label: &'static str, up: bool| {
            div()
                .id(id)
                .w(px(32.0))
                .h(px(32.0))
                .flex()
                .items_center()
                .justify_center()
                .bg(canvas)
                .border_1()
                .border_color(divider)
                .rounded(px(6.0))
                .text_size(px(15.0))
                .text_color(ink)
                .cursor_pointer()
                .hover(move |s| s.bg(frost))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.settings_step_limit(up, cx);
                    }),
                )
                .child(label)
        };
        let limit_section = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(heading("Dispatches at the same time"))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(step("settings-limit-fewer", "−", false))
                    .child(div().w(px(56.0)).child(limit_input))
                    .child(step("settings-limit-more", "+", true)),
            )
            .child(note(
                "Items above the limit wait in their trigger column until a dispatch finishes."
                    .into(),
            ));

        let notify_rows = Attention::ALL.into_iter().map(|attention| {
            let (label, hint) = notify_text(attention);
            let on = state.draft.notify.wants(attention);
            div()
                .id(("settings-notify", attention as usize))
                .flex()
                .flex_row()
                .items_start()
                .gap(px(10.0))
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.settings_toggle_notify(attention, cx);
                    }),
                )
                .child(
                    div()
                        .mt(px(2.0))
                        .w(px(16.0))
                        .h(px(16.0))
                        .flex_shrink_0()
                        .rounded(px(3.0))
                        .border_1()
                        .border_color(if on { accent } else { divider })
                        .bg(if on { accent } else { canvas })
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(11.0))
                        .text_color(if on { canvas } else { gpui::transparent_black() })
                        .child("✓"),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(div().text_size(px(13.0)).text_color(ink).child(label))
                        .child(div().text_size(px(12.0)).text_color(ghost).child(hint)),
                )
        });
        let delivery = if cfg!(windows) {
            "As a Windows toast and in the bell. The toast can't open a tab; the bell entry does."
        } else {
            "As a desktop notification and in the bell. The bell entry opens the tab."
        };
        let notify_section = div()
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(heading("Notify me when"))
            .children(notify_rows)
            .child(note(delivery.into()));

        div()
            .flex()
            .flex_col()
            .gap_6()
            .children(banner)
            .child(boards_section)
            .child(limit_section)
            .child(notify_section)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_limit_is_a_whole_number_from_one_to_the_maximum() {
        assert_eq!(parse_dispatch_limit(" 4 "), Ok(4));
        assert_eq!(parse_dispatch_limit("20"), Ok(20));
        for wrong in ["0", "21", "three", "2.5", ""] {
            let err = parse_dispatch_limit(wrong).unwrap_err();
            assert!(err.contains("Dispatches at the same time"), "{wrong}: {err}");
        }
    }

    #[test]
    fn a_step_stays_within_the_limits() {
        assert_eq!(step_limit("3", 3, true), 4);
        assert_eq!(step_limit("1", 3, false), 1);
        assert_eq!(step_limit("20", 3, true), 20);
        assert_eq!(step_limit("", 5, false), 4, "text that isn't a number steps from the setting");
        assert_eq!(step_limit("50", 3, false), 20);
    }

    #[test]
    fn a_board_costs_a_point_per_hundred_items_on_every_read() {
        assert_eq!(points_an_hour(&[Some(14)]), 120);
        assert_eq!(points_an_hour(&[Some(250), None]), 480);
        assert_eq!(points_an_hour(&[Some(0)]), 120);
        assert_eq!(points_an_hour(&[]), 0);
    }

    #[test]
    fn the_banner_says_what_the_token_lets_codescope_do() {
        let signed_in = |scopes: &[&str]| GhSignIn::Active {
            login: "maui1911".into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
        };
        let (good, text) = sign_in_banner(&signed_in(&["repo", "project"]));
        assert!(good && text.contains("can read and move items"), "{text}");
        let (good, text) = sign_in_banner(&signed_in(&["repo", "read:project"]));
        assert!(!good && text.contains("can read items but not move them"), "{text}");
        assert!(text.contains("gh auth refresh -s project"), "{text}");
        let (good, text) = sign_in_banner(&signed_in(&["repo"]));
        assert!(!good && text.contains("can't read or move items"), "{text}");
        let (good, text) = sign_in_banner(&signed_in(&[]));
        assert!(good && text == "GitHub CLI signed in as maui1911.", "{text}");
        assert!(!sign_in_banner(&GhSignIn::BadToken).0);
        let (good, text) = sign_in_banner(&GhSignIn::SignedOut);
        assert!(!good && text.contains("gh auth login"), "{text}");
    }
}
