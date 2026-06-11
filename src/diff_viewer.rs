//! Diff viewer — full-pane working-tree diff for one worktree.
//!
//! Toggled via `Ctrl+Shift+D`, the command palette ("View diff"), or
//! the sidebar worktree menu's "View changes" row. While visible it
//! replaces the work area (per-group tab strip + terminal grid) the
//! same way the Overview panel does, so the sidebar + status bar stay
//! anchored and the dismiss affordances stay where the user expects
//! them.
//!
//! Data comes from [`codescope_core::diff::worktree_diff`] — tracked
//! changes against `HEAD` plus untracked files — computed on the
//! background executor so a large diff never stalls the UI thread.
//! The layout is a master/detail split: file list with status badges
//! and per-file +/− counts on the left, the selected file's hunks on
//! the right with old/new line-number gutters, tinted added/removed
//! rows, and intraline emphasis on paired changes.

use std::path::PathBuf;
use std::sync::Arc;

use codescope_core::Theme;
use codescope_core::diff::{DiffFile, DiffLine, FileStatus, LineKind};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext as _, Context, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, px,
};

use crate::app::{AppShell, ToastKind};
use crate::theme;

/// Hard cap on rendered diff lines for the selected file. Beyond this
/// the detail pane appends a truncation notice instead — a 50k-line
/// generated-file diff must not turn into 50k gpui elements.
const MAX_RENDERED_LINES: usize = 4_000;

/// Everything the diff viewer needs to render. `Some` on
/// [`AppShell::diff_viewer`] = panel visible (mirrors how
/// `show_overview` gates the Overview).
pub(crate) struct DiffViewerState {
    /// Worktree the diff was computed for.
    pub worktree: PathBuf,
    pub files: Vec<DiffFile>,
    /// Index into `files` of the file shown in the detail pane.
    pub selected: usize,
    /// True while the background computation is in flight.
    pub loading: bool,
    pub error: Option<String>,
    /// Sequence stamp of the request that produced (or will produce)
    /// `files`. A stale background result — the user hit refresh, or
    /// re-opened for another worktree, before the previous `git diff`
    /// returned — compares unequal and is dropped.
    pub request_id: u64,
}

impl AppShell {
    /// `Ctrl+Shift+D` / palette entry point: close if open, otherwise
    /// open for the focused tab's worktree.
    pub(crate) fn toggle_diff_viewer(&mut self, cx: &mut Context<Self>) {
        if self.diff_viewer.is_some() {
            self.close_diff_viewer(cx);
        } else {
            self.open_diff_viewer(None, cx);
        }
    }

    /// Open the diff viewer for `worktree`, or for the focused tab's
    /// worktree when `None`. No-ops with a toast when neither resolves
    /// — a diff viewer with no repo to diff is dead UI.
    pub(crate) fn open_diff_viewer(
        &mut self,
        worktree: Option<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        // NB: `focused_tab_working_directory`, not the canonicalised
        // `focused_tab_worktree_path` — the latter is a comparison key
        // (lowercased, colon-stripped), not a path git can run in.
        let Some(worktree) = worktree.or_else(|| self.focused_tab_working_directory()) else {
            self.push_toast(
                ToastKind::Info,
                SharedString::new_static("No worktree to diff"),
                Some(SharedString::new_static(
                    "Focus a session tab first, or use the worktree menu's View changes.",
                )),
                cx,
            );
            return;
        };

        // The diff viewer and the Overview occupy the same work-area
        // slot; opening one dismisses the other.
        self.set_show_overview(false, cx);

        self.diff_request_seq += 1;
        let request_id = self.diff_request_seq;
        self.diff_viewer = Some(DiffViewerState {
            worktree: worktree.clone(),
            files: Vec::new(),
            selected: 0,
            loading: true,
            error: None,
            request_id,
        });
        cx.notify();
        self.spawn_diff_request(worktree, request_id, cx);
    }

    pub(crate) fn close_diff_viewer(&mut self, cx: &mut Context<Self>) {
        if self.diff_viewer.take().is_some() {
            cx.notify();
        }
    }

    /// Re-run the diff for the currently shown worktree, keeping the
    /// existing content on screen until the fresh result lands (no
    /// flash-to-empty on refresh).
    pub(crate) fn refresh_diff_viewer(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.diff_viewer.as_mut() else {
            return;
        };
        self.diff_request_seq += 1;
        let request_id = self.diff_request_seq;
        state.request_id = request_id;
        state.loading = true;
        state.error = None;
        let worktree = state.worktree.clone();
        cx.notify();
        self.spawn_diff_request(worktree, request_id, cx);
    }

    /// Compute `worktree_diff` on the background executor and fold the
    /// result back into `diff_viewer` — unless the panel was closed or
    /// a newer request superseded this one in the meantime.
    fn spawn_diff_request(
        &mut self,
        worktree: PathBuf,
        request_id: u64,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    codescope_core::diff::worktree_diff(&worktree)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(state) = this.diff_viewer.as_mut() else {
                    return;
                };
                if state.request_id != request_id {
                    return;
                }
                state.loading = false;
                match result {
                    Ok(files) => {
                        // Keep the selection pinned to the same file
                        // across a refresh when it still exists;
                        // otherwise snap back to the top.
                        let prev_path = state.files.get(state.selected).map(|f| f.path.clone());
                        state.selected = prev_path
                            .and_then(|p| files.iter().position(|f| f.path == p))
                            .unwrap_or(0);
                        state.files = files;
                        state.error = None;
                    }
                    Err(err) => {
                        // The header subtitle is a single-line row;
                        // anyhow's `:#` chain is one line, but wrapped
                        // git stderr can smuggle newlines into a
                        // message — flatten all whitespace runs.
                        let flat = format!("{err:#}")
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ");
                        state.error = Some(flat);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Render the full-pane diff viewer. Wired in by `render` when
    /// `diff_viewer.is_some()`; the caller substitutes this element
    /// for the work-area cluster, same as the Overview swap.
    pub(crate) fn render_diff_viewer(
        &self,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let canvas = theme::canvas(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let accent = theme::accent(theme);

        // SAFETY-of-unwrap: only called when `diff_viewer.is_some()`
        // (the render swap checks), but degrade gracefully anyway.
        let Some(state) = self.diff_viewer.as_ref() else {
            return div().into_any_element();
        };

        let total_added: u32 = state.files.iter().map(|f| f.added).sum();
        let total_removed: u32 = state.files.iter().map(|f| f.removed).sum();
        let subtitle: SharedString = if state.loading && state.files.is_empty() {
            SharedString::new_static("computing…")
        } else if let Some(err) = &state.error {
            SharedString::from(format!("error: {err}"))
        } else {
            let n = state.files.len();
            let files_part = if n == 1 {
                "1 file".to_string()
            } else {
                format!("{n} files")
            };
            SharedString::from(format!("{files_part} · +{total_added} −{total_removed}"))
        };

        // ── Header strip — mirrors the Overview header layout ────
        let eyebrow = div()
            .px(px(7.0))
            .py(px(3.0))
            .border_1()
            .border_color(accent)
            .rounded(px(3.0))
            .text_size(px(10.0))
            .text_color(accent)
            .font(theme::font_mono())
            .child("DIFF");

        let worktree_label = div()
            .ml(px(16.0))
            .text_size(px(12.0))
            .text_color(ink)
            .font(theme::font_mono())
            .truncate()
            .child(SharedString::from(
                state.worktree.to_string_lossy().into_owned(),
            ));

        let subtitle_el = div()
            .ml(px(12.0))
            .text_size(px(12.0))
            .text_color(if state.error.is_some() {
                theme::danger()
            } else {
                theme::ink_dim(theme)
            })
            .font(theme::font_sans())
            .flex_shrink_0()
            .child(subtitle);

        let refresh_button = div()
            .id("diff-refresh")
            .px(px(10.0))
            .py(px(5.0))
            .rounded(px(4.0))
            .text_size(px(13.0))
            .text_color(theme::ink_dim(theme))
            .cursor_pointer()
            .hover(move |s| s.bg(theme::frost_10(theme)).text_color(ink))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.refresh_diff_viewer(cx);
                }),
            )
            .child(if state.loading { "↻ Refreshing…" } else { "↻ Refresh" });

        let back_button = div()
            .id("diff-back")
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
                    this.close_diff_viewer(cx);
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
            .child(worktree_label)
            .child(subtitle_el)
            .child(div().flex_grow())
            .child(refresh_button)
            .child(back_button);

        // ── Empty / placeholder states ───────────────────────────
        if state.files.is_empty() {
            let (line_a, line_b): (SharedString, SharedString) = if state.loading {
                (
                    SharedString::new_static("Computing diff…"),
                    SharedString::new_static(""),
                )
            } else if state.error.is_some() {
                (
                    SharedString::new_static("Couldn't compute the diff"),
                    SharedString::new_static("Is this folder a git worktree?"),
                )
            } else {
                (
                    SharedString::new_static("Working tree clean"),
                    SharedString::new_static("No changes against HEAD, and nothing untracked."),
                )
            };
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

        let selected = state.selected.min(state.files.len() - 1);

        // ── File list (master) ───────────────────────────────────
        let mut file_rows: Vec<gpui::AnyElement> = Vec::with_capacity(state.files.len());
        for (ix, file) in state.files.iter().enumerate() {
            file_rows.push(
                self.render_diff_file_row(theme, file, ix, ix == selected, cx)
                    .into_any_element(),
            );
        }
        let mut file_list = div()
            .id("diff-file-list")
            .w(px(300.0))
            .flex_shrink_0()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .border_r_1()
            .border_color(divider)
            .py(px(6.0))
            .children(file_rows);
        file_list.style().min_size.height = Some(gpui::Length::Definite(px(0.0).into()));

        // ── Detail pane ──────────────────────────────────────────
        let mut detail = div()
            .id("diff-detail")
            .flex_grow()
            .flex()
            .flex_col()
            .overflow_y_scroll()
            .child(self.render_diff_detail(theme, &state.files[selected]));
        detail.style().min_size.height = Some(gpui::Length::Definite(px(0.0).into()));

        let mut body = div()
            .flex()
            .flex_row()
            .flex_grow()
            .child(file_list)
            .child(detail);
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

    /// One row in the file list: status badge, path, +/− counts.
    fn render_diff_file_row(
        &self,
        theme: &Arc<Theme>,
        file: &DiffFile,
        ix: usize,
        is_selected: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let badge_color = status_color(file.status);
        let row_bg = theme::active_context_wash(theme);
        let hover_bg = theme::frost_10(theme);

        let counts: SharedString = if file.binary {
            SharedString::new_static("bin")
        } else {
            SharedString::from(format!("+{} −{}", file.added, file.removed))
        };

        div()
            .id(SharedString::from(format!("diff-file-{ix}")))
            .px(px(12.0))
            .py(px(5.0))
            .mx(px(4.0))
            .rounded(px(4.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .when(is_selected, |s| s.bg(row_bg))
            .when(!is_selected, |s| s.hover(move |s| s.bg(hover_bg)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if let Some(state) = this.diff_viewer.as_mut()
                        && state.selected != ix
                    {
                        state.selected = ix;
                        cx.notify();
                    }
                }),
            )
            .child(
                div()
                    .w(px(14.0))
                    .flex_shrink_0()
                    .text_size(px(11.0))
                    .text_color(badge_color)
                    .font(theme::font_mono())
                    .child(file.status.badge()),
            )
            .child(
                div()
                    .flex_grow()
                    .text_size(px(11.5))
                    .text_color(theme::ink_dim(theme))
                    .font(theme::font_mono())
                    .truncate()
                    .child(SharedString::from(file.path.clone())),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(10.0))
                    .text_color(theme::text_faint())
                    .font(theme::font_mono())
                    .child(counts),
            )
    }

    /// The selected file's hunks: a file header bar, then hunk header
    /// + line rows, capped at [`MAX_RENDERED_LINES`].
    fn render_diff_detail(&self, theme: &Arc<Theme>, file: &DiffFile) -> impl IntoElement {
        let divider = theme::divider(theme);

        let title: SharedString = match (&file.old_path, file.status) {
            (Some(old), FileStatus::Renamed) => {
                SharedString::from(format!("{old} → {}", file.path))
            }
            _ => SharedString::from(file.path.clone()),
        };

        let file_header = div()
            .px(px(16.0))
            .py(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .border_b_1()
            .border_color(divider)
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(status_color(file.status))
                    .font(theme::font_mono())
                    .child(file.status.badge()),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(theme::ink(theme))
                    .font(theme::font_mono())
                    .truncate()
                    .child(title),
            )
            .child(div().flex_grow())
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px(11.0))
                    .text_color(theme::text_faint())
                    .font(theme::font_mono())
                    // "+0 −0" on a binary diff would read as "no
                    // changes" — mirror the file list's "bin" badge.
                    .child(if file.binary {
                        SharedString::new_static("bin")
                    } else {
                        SharedString::from(format!("+{} −{}", file.added, file.removed))
                    }),
            );

        let mut sections: Vec<gpui::AnyElement> = Vec::new();

        if file.binary {
            sections.push(centered_note("Binary file — no preview.").into_any_element());
        } else if file.hunks.is_empty() {
            // Empty hunks mean different things per status: an empty
            // new/untracked file has no content to show, a rename can
            // be content-identical, and a tracked file can change
            // mode-only. Don't claim "mode only" for the others.
            let note = match file.status {
                FileStatus::Added | FileStatus::Untracked => "Empty file.",
                FileStatus::Renamed => "Renamed — content unchanged.",
                FileStatus::Modified | FileStatus::Deleted => {
                    "No textual changes (mode or metadata only)."
                }
            };
            sections.push(centered_note(note).into_any_element());
        } else {
            let mut rendered = 0usize;
            'hunks: for hunk in &file.hunks {
                sections.push(
                    div()
                        .px(px(16.0))
                        .py(px(4.0))
                        .my(px(4.0))
                        .bg(theme::frost_10(theme))
                        .text_size(px(11.0))
                        .text_color(theme::accent(theme))
                        .font(theme::font_mono())
                        .child(SharedString::from(hunk.header.clone()))
                        .into_any_element(),
                );
                for line in &hunk.lines {
                    if rendered >= MAX_RENDERED_LINES {
                        sections.push(
                            centered_note(
                                "Diff truncated for display — open the file to see the rest.",
                            )
                            .into_any_element(),
                        );
                        break 'hunks;
                    }
                    sections.push(render_diff_line(theme, line).into_any_element());
                    rendered += 1;
                }
            }
        }

        if file.truncated {
            sections.push(
                centered_note("Large untracked file — preview capped.").into_any_element(),
            );
        }

        div()
            .flex()
            .flex_col()
            .pb(px(24.0))
            .child(file_header)
            .children(sections)
    }
}

/// Status-letter colour: additions green, deletions red, everything
/// else accent-ish / faint. Matches the +/− count colouring intuition.
fn status_color(status: FileStatus) -> Hsla {
    match status {
        FileStatus::Added | FileStatus::Untracked => theme::signal_ok(),
        FileStatus::Deleted => theme::signal_warn(),
        FileStatus::Modified | FileStatus::Renamed => theme::text_faint(),
    }
}

/// Muted full-width notice row used for binary / truncation / empty
/// placeholders inside the detail pane.
fn centered_note(text: &'static str) -> impl IntoElement {
    div()
        .px(px(16.0))
        .py(px(12.0))
        .text_size(px(11.5))
        .text_color(theme::text_faint())
        .font(theme::font_sans())
        .child(SharedString::new_static(text))
}

/// One diff line: old/new line-number gutters, marker, content with
/// optional intraline emphasis. Added/removed rows get a translucent
/// green/red wash; the emphasised span gets a stronger one.
fn render_diff_line(theme: &Arc<Theme>, line: &DiffLine) -> impl IntoElement {
    let (marker, row_bg, span_bg): (&'static str, Option<Hsla>, Option<Hsla>) = match line.kind {
        LineKind::Added => (
            "+",
            Some(alpha(theme::signal_ok(), 0.10)),
            Some(alpha(theme::signal_ok(), 0.28)),
        ),
        LineKind::Removed => (
            "−",
            Some(alpha(theme::signal_warn(), 0.10)),
            Some(alpha(theme::signal_warn(), 0.28)),
        ),
        LineKind::Context => (" ", None, None),
    };

    let num = |n: Option<u32>| -> SharedString {
        match n {
            Some(n) => SharedString::from(n.to_string()),
            None => SharedString::new_static(""),
        }
    };

    let text_color = match line.kind {
        LineKind::Context => theme::ink_dim(theme),
        _ => theme::ink(theme),
    };

    // Intraline emphasis: split the content into pre / changed / post
    // spans. The span boundaries are char indices from the core layer.
    let mut content = div()
        .flex()
        .flex_row()
        .text_size(px(12.0))
        .text_color(text_color)
        .font(theme::font_mono());
    match line.emphasis {
        Some((start, end)) if start < end && line.kind != LineKind::Context => {
            // Map the char-index span to byte offsets in one pass so the
            // three spans can be sliced as `&str` — no per-line
            // `Vec<char>` + intermediate `String`s on the UI thread.
            let mut start_b = line.text.len();
            let mut end_b = line.text.len();
            for (count, (b, _)) in line.text.char_indices().enumerate() {
                if count == start {
                    start_b = b;
                }
                if count == end {
                    end_b = b;
                    break;
                }
            }
            let pre = &line.text[..start_b];
            let mid = &line.text[start_b..end_b];
            let post = &line.text[end_b..];
            if !pre.is_empty() {
                content = content.child(SharedString::from(pre.to_owned()));
            }
            let mut mid_el = div().child(SharedString::from(mid.to_owned()));
            if let Some(bg) = span_bg {
                mid_el = mid_el.bg(bg).rounded(px(2.0));
            }
            content = content.child(mid_el);
            if !post.is_empty() {
                content = content.child(SharedString::from(post.to_owned()));
            }
        }
        _ => {
            content = content.child(SharedString::from(line.text.clone()));
        }
    }

    let mut row = div()
        .px(px(16.0))
        .flex()
        .flex_row()
        .items_start()
        .gap(px(0.0))
        .child(gutter_cell(num(line.old_no)))
        .child(gutter_cell(num(line.new_no)))
        .child(
            div()
                .w(px(18.0))
                .flex_shrink_0()
                .text_size(px(12.0))
                .text_color(text_color)
                .font(theme::font_mono())
                .child(marker),
        )
        .child(content);
    if let Some(bg) = row_bg {
        row = row.bg(bg);
    }
    row
}

/// Right-aligned fixed-width line-number cell.
fn gutter_cell(n: SharedString) -> impl IntoElement {
    div()
        .w(px(44.0))
        .flex_shrink_0()
        .flex()
        .justify_end()
        .pr(px(8.0))
        .text_size(px(10.5))
        .text_color(theme::text_faint())
        .font(theme::font_mono())
        .child(n)
}

/// `color` with its alpha replaced — gpui washes are how the rest of
/// the chrome derives tints (cf. `theme::active_context_wash`).
fn alpha(mut color: Hsla, a: f32) -> Hsla {
    color.a = a;
    color
}
