//! Overview panel — one card per currently-open session across every
//! project. Toggled on/off via the sidebar footer "Overview" button or
//! `Ctrl+Shift+O`; while visible, replaces the workspace area
//! (per-group tab strip + terminal grid) inside the main row so the
//! sidebar + status bar stay anchored.
//!
//! Mirrors `legacy:CodeScope.Ui/Views/OverviewView.xaml` /
//! `ViewModels/OverviewViewModel.cs` from the C# build. Like the C#
//! Overview, this view shows live sessions only — closed history is
//! filtered out at the core layer ([`codescope_core::build_overview_rows`])
//! and the reopen flow lives in the sidebar's per-project history
//! menu, not here.
//!
//! Each card surfaces telemetry-aware decoration (model name, tokens
//! used, last-turn duration, state dot) when the runtime is tailing
//! the agent — same data the status-bar cluster already reads from
//! [`crate::app::AppShell::telemetry_for`].
//!
//! Row data comes from [`codescope_core::build_overview_rows`] which
//! flattens the on-disk `ProjectsConfig` and sorts live sessions
//! newest-first by `last_opened`. Live-row decoration is folded in
//! here by joining on `session_id` against `self.groups`.

use std::sync::Arc;

use codescope_core::{
    OverviewLifecycle, OverviewRow, SessionState, Theme, build_overview_rows_for_live,
    format_context_pct, format_tokens, model_display_name, now_iso8601,
};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, InteractiveElement, IntoElement, MouseButton, ParentElement, SharedString,
    StatefulInteractiveElement, Styled, div, px,
};

use crate::app::AppShell;
use crate::theme;

/// Display name for one of the five supported agent ids. Mirrors
/// `OverviewViewModel.ResolveAgentDisplay`. The five built-in agent
/// ids are stable so a small lookup is enough for parity with the
/// C# build's `AgentRegistry`-threaded label resolution.
fn agent_display(agent_id: Option<&str>) -> SharedString {
    match agent_id {
        Some("claude") => SharedString::new_static("Claude Code"),
        Some("codex") => SharedString::new_static("Codex"),
        Some("copilot") => SharedString::new_static("Copilot CLI"),
        Some("opencode") => SharedString::new_static("OpenCode"),
        Some("pi") => SharedString::new_static("Pi"),
        Some(other) if !other.is_empty() => SharedString::from(other.to_string()),
        _ => SharedString::new_static("shell"),
    }
}

/// Per-row "Focus" coordinates — where the matching live tab lives in
/// the group/tab grid. `None` for closed rows (no live tab to focus)
/// and for live `Session` rows that don't currently have a matching
/// runtime tab (e.g. cold-start before session-restore lands).
#[derive(Debug, Clone, Copy)]
struct FocusTarget {
    group_idx: usize,
    tab_idx: usize,
}

impl AppShell {
    /// Render the full-pane Overview panel. Wired in by `render`
    /// when `show_overview == true`; the caller substitutes this
    /// element for the work-area cluster (group strips + terminal
    /// grid) so the sidebar and status bar stay anchored.
    pub(crate) fn render_overview(
        &self,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let projects = self.projects_snapshot();
        // Source the live session-id set from the running tab strip so
        // the Overview reflects what's actually open — persisted
        // `Session` records with `closed_at = None` can drift from
        // live state (crashes, layout-restored rows that were never
        // re-spawned, …). Mirrors C# `MainViewModel.OpenTabs`.
        let live_ids = self.live_session_ids();
        let rows = build_overview_rows_for_live(projects, &live_ids);
        let now = now_iso8601();

        let canvas = theme::canvas(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let accent = theme::accent(theme);

        // ── Header strip ─────────────────────────────────────────
        // 56 px tall, divider bottom border. Mirrors C# OverviewView's
        // header: OVERVIEW eyebrow chip + "N sessions across N
        // projects" subtitle + "← Back to workspace" link on the
        // right. Closed rows are filtered upstream so `rows.len()`
        // is the live-session count.
        let live_count = rows.len();
        let project_count = projects
            .projects
            .iter()
            .filter(|p| p.sessions.iter().any(|s| s.closed_at.is_none()))
            .count();
        let subtitle = subtitle_line(live_count, project_count);

        let eyebrow = div()
            .px(px(7.0))
            .py(px(3.0))
            .border_1()
            .border_color(accent)
            .rounded(px(3.0))
            .text_size(px(10.0))
            .text_color(accent)
            .font(theme::font_mono())
            .child("OVERVIEW");

        let subtitle_el = div()
            .ml(px(16.0))
            .text_size(px(13.0))
            .text_color(theme::ink_dim(theme))
            .font(theme::font_sans())
            .child(subtitle);

        let back_button = div()
            .id("overview-back")
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
                    this.set_show_overview(false, cx);
                }),
            )
            .child("← Back to workspace");

        let header = div()
            .h(px(56.0))
            .px(px(24.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(16.0))
            .border_b_1()
            .border_color(divider)
            .bg(canvas)
            .child(eyebrow)
            .child(subtitle_el)
            .child(div().flex_grow())
            .child(back_button);

        // ── Empty state ──────────────────────────────────────────
        if rows.is_empty() {
            return div()
                .flex()
                .flex_col()
                .flex_grow()
                .size_full()
                .bg(canvas)
                .child(header)
                .child(self.render_overview_empty(theme))
                .into_any_element();
        }

        // ── Build focus-target map for live rows ─────────────────
        // Join each live `OverviewRow.session_id` against the
        // runtime tab list so the "Focus" button knows where to
        // land. The lookup is by `Tab.session_id` (the persisted
        // CodeScope session id, same id `OverviewRow` carries).
        let focus_targets = self.focus_targets_for_overview();

        // ── Body: wrap of cards ──────────────────────────────────
        let mut cards: Vec<gpui::AnyElement> = Vec::with_capacity(rows.len());
        for row in rows.iter() {
            let entry = focus_targets.get(&row.session_id);
            let target = entry.map(|(t, _)| *t);
            let snapshot = entry
                .and_then(|(_, adopted)| adopted.as_deref())
                .and_then(|sid| self.telemetry_for(sid));
            cards.push(
                self.render_overview_card(theme, row, target, snapshot.as_ref(), &now, cx)
                    .into_any_element(),
            );
        }

        let mut body = div()
            .id("overview-body")
            .flex()
            .flex_col()
            .flex_grow()
            .overflow_y_scroll()
            .child(
                div()
                    .p(px(24.0))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap(px(16.0))
                    .children(cards),
            );
        // Mirror the sidebar's `min_h(0)` trick so the flex child
        // actually clips + scrolls instead of pushing siblings down.
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

    /// Empty-state filler. Mirrors the C# `IsEmpty` template's
    /// "No active sessions" + helper subline.
    fn render_overview_empty(&self, theme: &Arc<Theme>) -> impl IntoElement {
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
                    .child("No sessions yet"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(theme::text_faint())
                    .font(theme::font_sans())
                    .child(SharedString::new_static(
                        "Open one from the sidebar or press Ctrl+Shift+T — it will show up here.",
                    )),
            )
    }

    /// One overview card. 360x220 fixed size so the wrap panel lays
    /// out cleanly; uses the same `Surface.Panel`+`Surface.Border`
    /// pair the C# card frame paints (mapped to our `elevated`
    /// fill + `divider` outline).
    fn render_overview_card(
        &self,
        theme: &Arc<Theme>,
        row: &OverviewRow,
        target: Option<FocusTarget>,
        snapshot: Option<&codescope_core::TelemetrySnapshot>,
        now: &str,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let is_live = row.lifecycle == OverviewLifecycle::Live;
        let elev = theme::elevated(theme);
        let divider = theme::divider(theme);
        let accent = theme::accent(theme);

        // ── Status dot ──────────────────────────────────────────
        // Live + telemetry busy → warn (red).
        // Live + telemetry idle / no telemetry → ok (green).
        // Closed → faint grey pip.
        let (dot_color, status_label) = if is_live {
            match snapshot.map(|s| s.state).unwrap_or(SessionState::Unknown) {
                SessionState::Busy | SessionState::PendingToolUse => {
                    (theme::signal_warn(), SharedString::new_static("busy"))
                }
                SessionState::Idle => {
                    (theme::signal_ok(), SharedString::new_static("idle"))
                }
                SessionState::Unknown => {
                    (theme::signal_ok(), SharedString::new_static("live"))
                }
            }
        } else {
            let rel =
                codescope_core::session::format_closed_at_relative(row.closed_at.as_deref(), now);
            let label = if rel.is_empty() {
                SharedString::new_static("closed")
            } else {
                SharedString::from(format!("closed · {rel}"))
            };
            (theme::text_faint(), label)
        };

        // ── Head row ────────────────────────────────────────────
        // Status dot, project · branch title, agent type chip.
        let title_text = if row.branch_label.is_empty() {
            row.project_name.clone()
        } else {
            format!("{} · {}", row.project_name, row.branch_label)
        };
        let agent_chip = div()
            .px(px(7.0))
            .py(px(2.0))
            .border_1()
            .border_color(divider)
            .rounded(px(3.0))
            .text_size(px(10.0))
            .text_color(theme::text_faint())
            .font(theme::font_mono())
            .child(agent_display(row.agent_id.as_deref()));

        let head = div()
            .h(px(40.0))
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .child(
                div()
                    .w(px(8.0))
                    .h(px(8.0))
                    .rounded(px(4.0))
                    .bg(dot_color),
            )
            .child(
                div()
                    .flex_grow()
                    .text_size(px(13.0))
                    .text_color(theme::ink(theme))
                    .font(theme::font_sans())
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .truncate()
                    .child(SharedString::from(title_text)),
            )
            .child(agent_chip);

        // ── Body: status / tokens / duration ────────────────────
        let mut body_lines: Vec<gpui::AnyElement> = Vec::with_capacity(4);
        body_lines.push(
            self.render_overview_line(theme, "status", status_label, false)
                .into_any_element(),
        );
        if is_live {
            if let Some(snap) = snapshot {
                let model_label = snap
                    .model
                    .as_deref()
                    .map(model_display_name)
                    .unwrap_or_else(|| "—".to_string());
                body_lines.push(
                    self.render_overview_line(theme, "model", model_label.into(), false)
                        .into_any_element(),
                );
                let tokens = if snap.tokens_used == 0 {
                    SharedString::new_static("—")
                } else {
                    let mut s = format_tokens(snap.tokens_used);
                    if let Some(pct) = snap.context_pct {
                        s.push_str(&format!(" · {}", format_context_pct(pct)));
                    }
                    SharedString::from(s)
                };
                body_lines.push(
                    self.render_overview_line(theme, "tokens", tokens, false)
                        .into_any_element(),
                );
                let duration = match snap.last_turn_duration {
                    Some(d) => format_duration(d),
                    None => "—".to_string(),
                };
                body_lines.push(
                    self.render_overview_line(theme, "last turn", duration.into(), false)
                        .into_any_element(),
                );
            } else {
                body_lines.push(
                    self.render_overview_line(
                        theme,
                        "telemetry",
                        SharedString::new_static("not adopted"),
                        true,
                    )
                    .into_any_element(),
                );
            }
        } else {
            body_lines.push(
                self.render_overview_line(
                    theme,
                    "path",
                    SharedString::from(short_path(&row.working_directory)),
                    true,
                )
                .into_any_element(),
            );
        }

        let body = div()
            .mx(px(8.0))
            .px(px(12.0))
            .py(px(10.0))
            .rounded(px(4.0))
            .bg(theme::canvas(theme))
            .flex_grow()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .children(body_lines);

        // ── Footer: focus / reopen ──────────────────────────────
        // Live rows: "Focus" button → activate_tab(group, tab).
        // Closed rows: "Reopen" button → AppShell::reopen_session.
        let session_id = row.session_id.clone();
        let action = if is_live {
            let focus = target;
            div()
                .id(SharedString::from(format!(
                    "overview-focus-{}",
                    row.session_id
                )))
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(divider)
                .text_size(px(11.0))
                .text_color(theme::ink(theme))
                .when(focus.is_some(), |s| s.cursor_pointer())
                .when(focus.is_none(), |s| s.text_color(theme::text_faint()))
                .hover(move |s| {
                    if focus.is_some() {
                        s.border_color(accent).text_color(accent)
                    } else {
                        s
                    }
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        if let Some(target) = focus {
                            this.set_show_overview(false, cx);
                            this.activate_tab(target.group_idx, target.tab_idx, window, cx);
                        }
                    }),
                )
                .child(if focus.is_some() { "Focus" } else { "—" })
        } else {
            div()
                .id(SharedString::from(format!(
                    "overview-reopen-{}",
                    row.session_id
                )))
                .px(px(10.0))
                .py(px(4.0))
                .rounded(px(4.0))
                .border_1()
                .border_color(divider)
                .text_size(px(11.0))
                .text_color(accent)
                .cursor_pointer()
                .hover(move |s| s.border_color(accent))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.set_show_overview(false, cx);
                        this.reopen_session(session_id.clone(), window, cx);
                    }),
                )
                .child("Reopen")
        };

        let footer = div()
            .h(px(40.0))
            .px(px(12.0))
            .flex()
            .flex_row()
            .items_center()
            .child(
                div()
                    .flex_grow()
                    .text_size(px(10.0))
                    .text_color(theme::text_faint())
                    .font(theme::font_mono())
                    .truncate()
                    .child(SharedString::from(short_path(&row.working_directory))),
            )
            .child(action);

        div()
            .w(px(360.0))
            .h(px(220.0))
            .border_1()
            .border_color(divider)
            .rounded(px(6.0))
            .bg(elev)
            .flex()
            .flex_col()
            .child(head)
            .child(body)
            .child(footer)
    }

    /// One key/value line inside a card body. `faint = true` knocks
    /// the value foreground down to `text_faint`, matching the C#
    /// `Overview.PreviewLine` muted style.
    fn render_overview_line(
        &self,
        theme: &Arc<Theme>,
        label: &'static str,
        value: SharedString,
        faint: bool,
    ) -> impl IntoElement {
        let value_color = if faint {
            theme::text_faint()
        } else {
            theme::ink_dim(theme)
        };
        div()
            .flex()
            .flex_row()
            .gap(px(8.0))
            .text_size(px(11.0))
            .font(theme::font_mono())
            .child(
                div()
                    .w(px(72.0))
                    .text_color(theme::text_faint())
                    .child(label),
            )
            .child(
                div()
                    .flex_grow()
                    .text_color(value_color)
                    .truncate()
                    .child(value),
            )
    }

    /// Join `OverviewRow.session_id` to runtime tab coordinates +
    /// adopted agent session id (for live telemetry lookup).
    /// Tabs that haven't been opened in the current process (cold-
    /// start rehydrate hasn't happened, or the row was opened in
    /// another window) won't appear and their Focus button renders
    /// as a "—" placeholder.
    fn focus_targets_for_overview(
        &self,
    ) -> std::collections::HashMap<String, (FocusTarget, Option<String>)> {
        self.overview_tab_snapshot()
            .into_iter()
            .map(|(g_idx, t_idx, session_id, adopted_session_id)| {
                (
                    session_id,
                    (
                        FocusTarget {
                            group_idx: g_idx,
                            tab_idx: t_idx,
                        },
                        adopted_session_id,
                    ),
                )
            })
            .collect()
    }
}

/// "5 live across 3 projects" — drives the header subtitle. Mirrors
/// C# `OverviewViewModel.SubtitleBody`. The Rust port used to carry
/// an extra "N closed" term when Overview surfaced closed history;
/// that's been removed along with the closed-row branch — the panel
/// is now active-sessions-only.
fn subtitle_line(live: usize, project_count: usize) -> String {
    if live == 0 {
        return "no sessions yet".to_string();
    }
    let count_part = if live == 1 {
        "1 live".to_string()
    } else {
        format!("{live} live")
    };
    let project_part = if project_count == 1 {
        "1 project".to_string()
    } else {
        format!("{project_count} projects")
    };
    if project_count == 0 {
        count_part
    } else {
        format!("{count_part} across {project_part}")
    }
}

/// Render `Duration` as `1.2s` / `12s` / `2m 03s`. Mirrors the
/// shorthand the status bar's last-turn pill uses elsewhere in the
/// chrome.
fn format_duration(d: std::time::Duration) -> String {
    let total_ms = d.as_millis() as u64;
    if total_ms < 1_000 {
        return format!("{total_ms}ms");
    }
    let secs = total_ms / 1_000;
    if secs < 10 {
        let tenths = (total_ms % 1_000) / 100;
        return format!("{secs}.{tenths}s");
    }
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    let rem_secs = secs % 60;
    format!("{mins}m {rem_secs:02}s")
}

/// Strip the parent directories from a working-directory path,
/// keeping the last two segments so the user still sees enough to
/// disambiguate worktrees (`codescope.worktrees/feature-x` reads
/// better than just `feature-x`).
fn short_path(path: &str) -> String {
    // Split on both separators (a Windows-written projects.json can
    // surface on any platform) and echo the path's own separator back
    // so `/home/u/wt` shortens to `…/u/wt`, not `…\u\wt`. The old
    // `PathBuf::components()` version split on the *host* separator
    // only, so foreign paths never shortened and the label always
    // used `\`.
    let sep = if path.contains('/') { '/' } else { '\\' };
    let comps: Vec<&str> = path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && !s.ends_with(':'))
        .collect();
    if comps.len() <= 2 {
        return path.to_string();
    }
    let tail = &comps[comps.len() - 2..];
    format!("…{sep}{}{sep}{}", tail[0], tail[1])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn subtitle_line_handles_pluralization_and_zero_cases() {
        assert_eq!(subtitle_line(0, 0), "no sessions yet");
        assert_eq!(subtitle_line(1, 1), "1 live across 1 project");
        assert_eq!(subtitle_line(3, 2), "3 live across 2 projects");
        assert_eq!(subtitle_line(2, 3), "2 live across 3 projects");
    }

    #[test]
    fn agent_display_maps_known_ids_and_falls_back_to_shell() {
        assert_eq!(agent_display(Some("claude")), "Claude Code");
        assert_eq!(agent_display(Some("codex")), "Codex");
        assert_eq!(agent_display(Some("copilot")), "Copilot CLI");
        assert_eq!(agent_display(Some("opencode")), "OpenCode");
        assert_eq!(agent_display(Some("pi")), "Pi");
        assert_eq!(agent_display(None), "shell");
        assert_eq!(agent_display(Some("")), "shell");
        // Unknown ids round-trip verbatim so a future agent id without
        // a built-in label still reads as something instead of just
        // "shell".
        assert_eq!(agent_display(Some("gemini")), "gemini");
    }

    #[test]
    fn format_duration_covers_sub_second_to_minute_buckets() {
        assert_eq!(format_duration(Duration::from_millis(0)), "0ms");
        assert_eq!(format_duration(Duration::from_millis(750)), "750ms");
        assert_eq!(format_duration(Duration::from_millis(1_200)), "1.2s");
        assert_eq!(format_duration(Duration::from_secs(9)), "9.0s");
        assert_eq!(format_duration(Duration::from_secs(12)), "12s");
        assert_eq!(format_duration(Duration::from_secs(59)), "59s");
        assert_eq!(format_duration(Duration::from_secs(60)), "1m 00s");
        assert_eq!(format_duration(Duration::from_secs(125)), "2m 05s");
    }

    #[test]
    fn short_path_keeps_two_tail_segments() {
        assert_eq!(short_path("C:\\dev\\foo\\bar"), "…\\foo\\bar");
        assert_eq!(short_path("foo\\bar"), "foo\\bar");
        assert_eq!(short_path("foo"), "foo");
        // Unix-style paths shorten with their own separator.
        assert_eq!(short_path("/home/u/proj/wt"), "…/proj/wt");
        assert_eq!(short_path("/home/u"), "/home/u");
    }
}
