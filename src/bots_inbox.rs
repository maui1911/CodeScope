//! Bots inbox — a read-only view of the selected project's bot control
//! plane (`labs/agent-bots/.state`, see `labs/agent-bots/README.md`
//! §7.4 "Stage 2 — the inbox").
//!
//! Toggled via the sidebar footer "Bots" button (shown only when the
//! selected project has a control plane), `Ctrl+Shift+I`, or the
//! command palette. While visible it takes the same work-area slot as
//! the Overview and the diff viewer; opening one closes the others.
//!
//! Data comes from [`codescope_core::bots`], read on the background
//! executor by a poll loop that runs whether or not the panel is on
//! stage, so the footer badge can say how many tasks need a human.
//! The panel only navigates — open a handoff, reveal a worktree, copy
//! a branch. Dispatching, approving and stopping stay with the lab's
//! runner scripts; nothing here writes to the control plane.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use codescope_core::Theme;
use codescope_core::bots::{BoardEvent, InboxItem, TaskStatus};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, ClipboardItem, Context, Hsla, InteractiveElement, IntoElement, MouseButton,
    ParentElement, SharedString, StatefulInteractiveElement, Styled, div, px,
};

use crate::app::AppShell;
use crate::sidebar::BotsFooter;
use crate::theme;

/// Re-read cadence while the inbox is on stage: a running bot's status
/// should move without a manual refresh.
const BOTS_POLL_VISIBLE: Duration = Duration::from_secs(2);
/// Re-read cadence while it is not: only the footer badge depends on
/// it, and a project without a control plane costs one `is_dir` stat.
const BOTS_POLL_HIDDEN: Duration = Duration::from_secs(5);

/// Width of the task list column.
const LIST_WIDTH: f32 = 340.0;

/// What the inbox last read. `plane == None` means the selected project
/// has no control plane (or no project is selected).
#[derive(Default)]
pub(crate) struct BotsInboxState {
    pub plane: Option<PathBuf>,
    pub items: Vec<InboxItem>,
    /// Every board row, for the selected task's timeline.
    pub events: Vec<BoardEvent>,
    /// Id of the task shown in the detail pane.
    pub selected: Option<String>,
    pub error: Option<String>,
    /// Sequence stamp of the newest read started.
    pub request_id: u64,
    /// Stamp of the read currently shown; an older result that lands
    /// late is dropped, but a slow read is never starved by newer ones
    /// still in flight.
    pub applied_id: u64,
}

/// One background read of a control plane.
struct BotsSnapshot {
    /// The project root the read was for.
    root: Option<String>,
    plane: Option<PathBuf>,
    items: Result<Vec<InboxItem>, String>,
    events: Vec<BoardEvent>,
}

fn read_snapshot(project_root: Option<String>) -> BotsSnapshot {
    let plane = project_root
        .as_deref()
        .and_then(|root| codescope_core::bots::lab_control_plane(Path::new(root)));
    let Some(dir) = plane.as_deref() else {
        return BotsSnapshot {
            root: project_root,
            plane,
            items: Ok(Vec::new()),
            events: Vec::new(),
        };
    };
    let items = codescope_core::bots::load_inbox(dir).map_err(|e| e.to_string());
    let board = std::fs::read_to_string(dir.join("board.md")).unwrap_or_default();
    let events = codescope_core::bots::parse_board(&board);
    BotsSnapshot { root: project_root, plane, items, events }
}

/// Tasks that are blocked or waiting for review.
fn attention_count(items: &[InboxItem]) -> usize {
    items.iter().filter(|i| i.status.needs_attention()).count()
}

/// Keep the selection on the same task when it is still listed,
/// otherwise fall back to the first item.
fn keep_selection(previous: Option<&str>, items: &[InboxItem]) -> Option<String> {
    previous
        .filter(|id| items.iter().any(|i| i.id == *id))
        .map(str::to_string)
        .or_else(|| items.first().map(|i| i.id.clone()))
}

/// `2026-09-13T14:55:01Z` → `09-13 14:55`. Anything shaped differently
/// is shown as written.
fn short_time(at: &str) -> String {
    match (at.get(5..10), at.get(11..16)) {
        (Some(date), Some(time)) if at.len() >= 16 && at.as_bytes()[10] == b'T' => {
            format!("{date} {time}")
        }
        _ => at.to_string(),
    }
}

fn status_label(status: &TaskStatus) -> SharedString {
    match status {
        TaskStatus::Todo => "todo".into(),
        TaskStatus::Dispatched => "running".into(),
        TaskStatus::Blocked => "blocked".into(),
        TaskStatus::NeedsReview => "needs review".into(),
        TaskStatus::Done => "done".into(),
        TaskStatus::Other(raw) if raw.is_empty() => "no status".into(),
        TaskStatus::Other(raw) => SharedString::from(raw.clone()),
    }
}

fn status_color(status: &TaskStatus, theme: &Theme) -> Hsla {
    match status {
        TaskStatus::Blocked => theme::signal_warn(),
        TaskStatus::NeedsReview => theme::signal_review(),
        TaskStatus::Dispatched => theme::accent(theme),
        TaskStatus::Done => theme::signal_ok(),
        TaskStatus::Todo | TaskStatus::Other(_) => theme::text_faint(),
    }
}

/// Open a file with the platform's default handler.
fn open_with_default_app(path: &Path) {
    let path = path.to_string_lossy();
    #[cfg(target_os = "windows")]
    {
        crate::win32_titlebar::shell_open_url(&path.replace('/', "\\"));
    }
    #[cfg(target_os = "macos")]
    {
        if let Err(err) = std::process::Command::new("open").arg(path.as_ref()).spawn() {
            eprintln!("warning: failed to open {path}: {err:#}");
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Err(err) = std::process::Command::new("xdg-open").arg(path.as_ref()).spawn() {
            eprintln!("warning: failed to open {path}: {err:#}");
        }
    }
}

impl AppShell {
    /// Start the control-plane poll. Called once from the constructor.
    ///
    /// The loop waits for each read before scheduling the next, so a
    /// slow disk slows the poll down instead of stacking reads.
    pub(crate) fn start_bots_poll(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                let Ok((request_id, root)) = this.update(cx, |this, cx| this.begin_bots_read(cx))
                else {
                    break;
                };
                let snapshot = cx.background_spawn(async move { read_snapshot(root) }).await;
                let Ok(visible) = this.update(cx, |this, cx| {
                    this.finish_bots_read(request_id, snapshot, cx);
                    this.show_bots
                }) else {
                    break;
                };
                let interval = if visible { BOTS_POLL_VISIBLE } else { BOTS_POLL_HIDDEN };
                cx.background_executor().timer(interval).await;
            }
        })
        .detach();
    }

    /// Read the selected project's control plane once, now, outside
    /// the poll (opening the panel).
    pub(crate) fn refresh_bots(&mut self, cx: &mut Context<Self>) {
        let (request_id, root) = self.begin_bots_read(cx);
        cx.spawn(async move |this, cx| {
            let snapshot = cx.background_spawn(async move { read_snapshot(root) }).await;
            let _ = this.update(cx, |this, cx| this.finish_bots_read(request_id, snapshot, cx));
        })
        .detach();
    }

    fn begin_bots_read(&mut self, cx: &mut Context<Self>) -> (u64, Option<String>) {
        self.bots.request_id += 1;
        (self.bots.request_id, self.active_project_path(cx))
    }

    /// Apply a finished read unless something newer is already shown,
    /// or the selected project changed while it ran (that project's
    /// result would otherwise fill the panel and badge until the next
    /// read).
    fn finish_bots_read(&mut self, request_id: u64, snapshot: BotsSnapshot, cx: &mut Context<Self>) {
        if request_id <= self.bots.applied_id || snapshot.root != self.active_project_path(cx) {
            return;
        }
        self.bots.applied_id = request_id;
        self.apply_bots_snapshot(snapshot, cx);
    }

    fn apply_bots_snapshot(&mut self, snapshot: BotsSnapshot, cx: &mut Context<Self>) {
        let (items, error) = match snapshot.items {
            Ok(items) => (items, None),
            Err(err) => (Vec::new(), Some(err)),
        };
        let state = &mut self.bots;
        let changed = state.plane != snapshot.plane
            || state.items != items
            || state.events != snapshot.events
            || state.error != error;
        if changed {
            state.selected = keep_selection(state.selected.as_deref(), &items);
            state.plane = snapshot.plane;
            state.items = items;
            state.events = snapshot.events;
            state.error = error;
            if self.show_bots {
                cx.notify();
            }
        }
        self.push_bots_footer(cx);
    }

    fn push_bots_footer(&mut self, cx: &mut Context<Self>) {
        let footer = BotsFooter {
            available: self.bots.plane.is_some(),
            visible: self.show_bots,
            attention: attention_count(&self.bots.items),
        };
        self.set_sidebar_bots_footer(footer, cx);
    }

    /// Show or hide the inbox. Opening it closes the Overview and the
    /// diff viewer, and reads the control plane right away rather than
    /// waiting for the next poll.
    pub(crate) fn set_show_bots(&mut self, value: bool, cx: &mut Context<Self>) {
        if self.show_bots == value {
            return;
        }
        if value {
            self.close_diff_viewer(cx);
            self.set_show_overview(false, cx);
        }
        self.show_bots = value;
        self.push_bots_footer(cx);
        if value {
            self.refresh_bots(cx);
        }
        cx.notify();
    }

    fn select_bot_task(&mut self, id: String, cx: &mut Context<Self>) {
        if self.bots.selected.as_deref() != Some(id.as_str()) {
            self.bots.selected = Some(id);
            cx.notify();
        }
    }

    /// Render the full-pane inbox. Wired in by `render` when
    /// `show_bots == true`, in place of the work-area cluster.
    pub(crate) fn render_bots_inbox(
        &self,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let canvas = theme::canvas(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let accent = theme::accent(theme);
        let state = &self.bots;

        let attention = attention_count(&state.items);
        let subtitle: SharedString = match (&state.plane, &state.error) {
            (None, _) => "no control plane in the selected project".into(),
            (Some(_), Some(err)) => SharedString::from(format!("error: {err}")),
            (Some(_), None) => {
                let tasks = match state.items.len() {
                    1 => "1 task".to_string(),
                    n => format!("{n} tasks"),
                };
                if attention > 0 {
                    SharedString::from(format!("{tasks} · {attention} need attention"))
                } else {
                    SharedString::from(tasks)
                }
            }
        };

        let eyebrow = div()
            .px(px(7.0))
            .py(px(3.0))
            .border_1()
            .border_color(accent)
            .rounded(px(3.0))
            .text_size(px(10.0))
            .text_color(accent)
            .font(theme::font_mono())
            .child("BOTS");

        let subtitle_el = div()
            .ml(px(16.0))
            .text_size(px(13.0))
            .text_color(if state.error.is_some() { theme::danger() } else { theme::ink_dim(theme) })
            .font(theme::font_sans())
            .flex_shrink_0()
            .child(subtitle);

        let plane_el = div()
            .ml(px(12.0))
            .min_w(px(0.0))
            .text_size(px(11.0))
            .text_color(theme::text_faint())
            .font(theme::font_mono())
            .truncate()
            .children(
                state
                    .plane
                    .as_ref()
                    .map(|p| SharedString::from(p.to_string_lossy().into_owned())),
            );

        let back_button = div()
            .id("bots-back")
            .flex_shrink_0()
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(4.0))
            .text_size(px(13.0))
            .text_color(accent)
            .cursor_pointer()
            .hover(move |s| s.bg(theme::frost_10(theme)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.set_show_bots(false, cx);
                }),
            )
            .child("← Back to workspace");

        let header = div()
            .h(px(56.0))
            .px(px(24.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .border_b_1()
            .border_color(divider)
            .bg(canvas)
            .child(eyebrow)
            .child(subtitle_el)
            .child(plane_el)
            .child(div().flex_grow())
            .child(back_button);

        let empty = match (&state.plane, &state.error) {
            (None, _) => Some((
                "No bots in this project",
                "The inbox reads labs/agent-bots/.state under the selected project.",
            )),
            (Some(_), Some(_)) => Some((
                "Couldn't read the control plane",
                "The next read retries automatically.",
            )),
            (Some(_), None) if state.items.is_empty() => {
                Some(("No tasks yet", "Tasks appear here once the runner has a task file."))
            }
            _ => None,
        };
        if let Some((line_a, line_b)) = empty {
            return div()
                .flex()
                .flex_col()
                .flex_grow()
                .size_full()
                .bg(canvas)
                .text_color(ink)
                .child(header)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_grow()
                        .items_center()
                        .justify_center()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_size(px(18.0))
                                .text_color(theme::ink_dim(theme))
                                .font(theme::font_sans())
                                .child(line_a),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .text_color(theme::text_faint())
                                .font(theme::font_sans())
                                .child(line_b),
                        ),
                )
                .into_any_element();
        }

        let selected = state
            .selected
            .as_deref()
            .and_then(|id| state.items.iter().find(|i| i.id == id))
            .unwrap_or(&state.items[0]);

        // ── Task list ────────────────────────────────────────────
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        let (needs, rest): (Vec<&InboxItem>, Vec<&InboxItem>) =
            state.items.iter().partition(|i| i.status.needs_attention());
        for (label, group) in [("NEEDS ATTENTION", needs), ("RECENT", rest)] {
            if group.is_empty() {
                continue;
            }
            rows.push(
                div()
                    .px(px(16.0))
                    .pt(px(12.0))
                    .pb(px(6.0))
                    .text_size(px(10.0))
                    .text_color(theme::text_faint())
                    .font(theme::font_mono())
                    .child(label)
                    .into_any_element(),
            );
            for item in group {
                rows.push(
                    self.render_bot_row(theme, item, item.id == selected.id, cx)
                        .into_any_element(),
                );
            }
        }
        let mut list = div()
            .id("bots-list")
            .w(px(LIST_WIDTH))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .pb(px(8.0))
            .border_r_1()
            .border_color(divider)
            .children(rows);
        list.style().min_size.height = Some(gpui::Length::Definite(px(0.0).into()));

        let mut detail = div()
            .id("bots-detail")
            .flex_grow()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .child(self.render_bot_detail(theme, selected, cx));
        detail.style().min_size.height = Some(gpui::Length::Definite(px(0.0).into()));

        let mut body = div().flex().flex_row().flex_grow().child(list).child(detail);
        body.style().min_size.height = Some(gpui::Length::Definite(px(0.0).into()));

        div()
            .flex()
            .flex_col()
            .flex_grow()
            .size_full()
            .bg(canvas)
            .text_color(ink)
            .child(header)
            .child(body)
            .into_any_element()
    }

    /// One task in the list: a status stripe, id and status, title, and
    /// the last board event.
    fn render_bot_row(
        &self,
        theme: &Arc<Theme>,
        item: &InboxItem,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let color = status_color(&item.status, theme);
        let hover_bg = theme::frost_10(theme);
        let last: SharedString = match &item.last_event {
            Some(e) => SharedString::from(format!("{} · {}", e.event, short_time(&e.at))),
            None => "no board events".into(),
        };
        let id = item.id.clone();

        div()
            .id(SharedString::from(format!("bots-row-{}", item.id)))
            .mx(px(6.0))
            .px(px(10.0))
            .py(px(8.0))
            .rounded(px(4.0))
            .flex()
            .flex_row()
            .gap(px(10.0))
            .cursor_pointer()
            .when(is_selected, |s| s.bg(theme::active_context_wash(theme)))
            .when(!is_selected, |s| s.hover(move |s| s.bg(hover_bg)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.select_bot_task(id.clone(), cx);
                }),
            )
            .child(div().w(px(3.0)).flex_shrink_0().rounded(px(2.0)).bg(color))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_grow()
                    .min_w(px(0.0))
                    .gap(px(2.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(8.0))
                            .text_size(px(11.0))
                            .font(theme::font_mono())
                            .child(
                                div()
                                    .flex_grow()
                                    .text_color(theme::ink_dim(theme))
                                    .child(SharedString::from(item.id.clone())),
                            )
                            .child(div().text_color(color).child(status_label(&item.status))),
                    )
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(theme::ink(theme))
                            .font(theme::font_sans())
                            .truncate()
                            .child(SharedString::from(item.title.clone())),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(theme::text_faint())
                            .font(theme::font_mono())
                            .truncate()
                            .child(last),
                    ),
            )
    }

    /// The selected task: actions, facts, the handoff's blockers and
    /// next action, and its board timeline, newest first.
    fn render_bot_detail(
        &self,
        theme: &Arc<Theme>,
        item: &InboxItem,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let color = status_color(&item.status, theme);
        let divider = theme::divider(theme);

        let heading = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .text_size(px(11.0))
                    .font(theme::font_mono())
                    .child(
                        div()
                            .text_color(theme::ink_dim(theme))
                            .child(SharedString::from(item.id.clone())),
                    )
                    .child(
                        div()
                            .px(px(6.0))
                            .py(px(1.0))
                            .border_1()
                            .border_color(color)
                            .rounded(px(3.0))
                            .text_color(color)
                            .child(status_label(&item.status)),
                    ),
            )
            .child(
                div()
                    .text_size(px(18.0))
                    .text_color(theme::ink(theme))
                    .font(theme::font_sans())
                    .child(SharedString::from(item.title.clone())),
            );

        // ── Actions — navigation only ────────────────────────────
        let mut actions: Vec<gpui::AnyElement> = Vec::new();
        if let Some(handoff) = &item.handoff {
            let path = handoff.path.clone();
            actions.push(
                action_button(theme, "bots-open-handoff", "Open handoff")
                    .on_mouse_down(MouseButton::Left, move |_, _, _| open_with_default_app(&path))
                    .into_any_element(),
            );
        }
        if let Some(worktree) = item.worktree.as_ref().filter(|w| Path::new(w).is_dir()) {
            let worktree = worktree.clone();
            actions.push(
                action_button(theme, "bots-reveal-worktree", "Reveal worktree")
                    .on_mouse_down(MouseButton::Left, move |_, _, _| {
                        crate::sidebar::reveal_path_in_file_browser(&worktree)
                    })
                    .into_any_element(),
            );
        }
        if let Some(branch) = &item.branch {
            let branch = branch.clone();
            actions.push(
                action_button(theme, "bots-copy-branch", "Copy branch")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(branch.clone()));
                        }),
                    )
                    .into_any_element(),
            );
        }

        // ── Facts ────────────────────────────────────────────────
        let short_sha = |sha: &str| sha.get(..12).unwrap_or(sha).to_string();
        let mut facts: Vec<gpui::AnyElement> = Vec::new();
        for (label, value) in [
            ("owner", Some(item.owner.clone()).filter(|o| !o.is_empty())),
            ("branch", item.branch.clone()),
            ("base", item.base_sha.as_deref().map(short_sha)),
            ("worktree", item.worktree.clone()),
            ("handoff", item.handoff.as_ref().map(|h| format!("{} · {}", h.from, h.at))),
        ] {
            let Some(value) = value else { continue };
            facts.push(fact_line(theme, label, value).into_any_element());
        }

        let mut sections: Vec<gpui::AnyElement> = Vec::new();
        if let Some(handoff) = &item.handoff {
            let blockers = handoff.blockers.trim();
            if !blockers.is_empty() && blockers != "none" {
                sections.push(
                    text_section(theme, "BLOCKERS", blockers, Some(theme::signal_warn()))
                        .into_any_element(),
                );
            }
            if !handoff.next_action.is_empty() {
                sections.push(
                    text_section(theme, "NEXT ACTION", &handoff.next_action, None)
                        .into_any_element(),
                );
            }
        }

        // ── Timeline ─────────────────────────────────────────────
        let timeline_rows: Vec<gpui::AnyElement> = self
            .bots
            .events
            .iter()
            .rev()
            .filter(|e| e.task == item.id)
            .map(|e| {
                div()
                    .flex()
                    .flex_row()
                    .gap(px(12.0))
                    .py(px(3.0))
                    .text_size(px(11.5))
                    .font(theme::font_mono())
                    .child(
                        div()
                            .w(px(84.0))
                            .flex_shrink_0()
                            .text_color(theme::text_faint())
                            .child(SharedString::from(short_time(&e.at))),
                    )
                    .child(
                        div()
                            .w(px(110.0))
                            .flex_shrink_0()
                            .text_color(theme::ink(theme))
                            .child(SharedString::from(e.event.clone())),
                    )
                    .child(
                        div()
                            .min_w(px(0.0))
                            .text_color(theme::ink_dim(theme))
                            .truncate()
                            .child(SharedString::from(e.detail.clone())),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            .p(px(24.0))
            .flex()
            .flex_col()
            .gap(px(20.0))
            .child(heading)
            .when(!actions.is_empty(), |s| {
                s.child(div().flex().flex_row().flex_wrap().gap(px(8.0)).children(actions))
            })
            .when(!facts.is_empty(), |s| {
                s.child(div().flex().flex_col().gap(px(4.0)).children(facts))
            })
            .children(sections)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .pt(px(12.0))
                    .border_t_1()
                    .border_color(divider)
                    .child(section_label("BOARD"))
                    .when(timeline_rows.is_empty(), |s| {
                        s.child(
                            div()
                                .text_size(px(11.5))
                                .text_color(theme::text_faint())
                                .child("No board events for this task."),
                        )
                    })
                    .children(timeline_rows),
            )
    }
}

fn action_button(
    theme: &Arc<Theme>,
    id: &'static str,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    let ink = theme::ink(theme);
    let hover_bg = theme::frost_10(theme);
    div()
        .id(id)
        .px(px(10.0))
        .py(px(5.0))
        .border_1()
        .border_color(theme::divider(theme))
        .rounded(px(4.0))
        .text_size(px(12.0))
        .text_color(theme::ink_dim(theme))
        .font(theme::font_sans())
        .cursor_pointer()
        .hover(move |s| s.bg(hover_bg).text_color(ink))
        .child(label)
}

fn section_label(label: &'static str) -> impl IntoElement {
    div()
        .pb(px(6.0))
        .text_size(px(10.0))
        .text_color(theme::text_faint())
        .font(theme::font_mono())
        .child(label)
}

fn fact_line(theme: &Arc<Theme>, label: &'static str, value: String) -> impl IntoElement {
    div()
        .flex()
        .flex_row()
        .gap(px(12.0))
        .text_size(px(12.0))
        .font(theme::font_mono())
        .child(div().w(px(72.0)).flex_shrink_0().text_color(theme::text_faint()).child(label))
        .child(
            div()
                .min_w(px(0.0))
                .text_color(theme::ink_dim(theme))
                .truncate()
                .child(SharedString::from(value)),
        )
}

/// A labelled block of handoff text, multi-line kept. `rail` draws a
/// coloured left edge (blockers).
fn text_section(
    theme: &Arc<Theme>,
    label: &'static str,
    text: &str,
    rail: Option<Hsla>,
) -> impl IntoElement {
    let lines: Vec<gpui::AnyElement> = text
        .lines()
        .map(|line| div().child(SharedString::from(line.to_string())).into_any_element())
        .collect();
    div()
        .flex()
        .flex_col()
        .child(section_label(label))
        .child(
            div()
                .flex()
                .flex_col()
                .when_some(rail, |s, color| s.pl(px(10.0)).border_l_2().border_color(color))
                .text_size(px(12.5))
                .text_color(theme::ink(theme))
                .font(theme::font_sans())
                .children(lines),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, status: TaskStatus) -> InboxItem {
        InboxItem {
            id: id.to_string(),
            title: String::new(),
            owner: String::new(),
            status,
            branch: None,
            worktree: None,
            base_sha: None,
            last_event: None,
            handoff: None,
        }
    }

    #[test]
    fn attention_counts_blocked_and_needs_review() {
        let items = [
            item("T-1", TaskStatus::Blocked),
            item("T-2", TaskStatus::NeedsReview),
            item("T-3", TaskStatus::Done),
            item("T-4", TaskStatus::Dispatched),
        ];
        assert_eq!(attention_count(&items), 2);
    }

    #[test]
    fn selection_stays_on_a_task_that_is_still_listed() {
        let items = [item("T-1", TaskStatus::Done), item("T-2", TaskStatus::Done)];
        assert_eq!(keep_selection(Some("T-2"), &items).as_deref(), Some("T-2"));
    }

    #[test]
    fn selection_falls_back_to_the_first_task() {
        let items = [item("T-1", TaskStatus::Done), item("T-2", TaskStatus::Done)];
        assert_eq!(keep_selection(Some("T-9"), &items).as_deref(), Some("T-1"));
        assert_eq!(keep_selection(None, &items).as_deref(), Some("T-1"));
        assert_eq!(keep_selection(Some("T-1"), &[]), None);
    }

    #[test]
    fn short_time_trims_a_board_timestamp() {
        assert_eq!(short_time("2026-09-13T14:55:01Z"), "09-13 14:55");
        assert_eq!(short_time("yesterday"), "yesterday");
        assert_eq!(short_time(""), "");
    }

    #[test]
    fn a_project_without_a_control_plane_reads_as_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let snapshot = read_snapshot(Some(tmp.path().to_string_lossy().into_owned()));
        assert!(snapshot.plane.is_none());
        assert!(snapshot.items.unwrap().is_empty());
        assert!(read_snapshot(None).plane.is_none());
    }
}
