//! First-run / no-projects empty state.
//!
//! Port of `src/CodeScope.Ui/Views/EmptyStateView.xaml` — the
//! "Add a project." hero that replaces the workspace area whenever
//! [`crate::sidebar::SidebarView::projects`] is empty. Same gate the
//! C# build uses (`Sidebar.IsEmpty` DataTrigger in `MainWindow.xaml`).
//!
//! # Differences from the C# build
//!
//! - **Chord chips reflect what actually works in Rust.** The C# XAML
//!   labels the primary CTA with `⌃ N` and the palette tile with
//!   `⌃ K`; both were spec values, not bound. The Rust port has no
//!   shortcut for "new project" (palette-only) and uses `Ctrl+Shift+P`
//!   for the palette, so we drop the CTA chord and label the palette
//!   tile `⌃⇧ P`. Documented per `docs/DECISIONS.md` guidance on
//!   intentional UX deviations.
//! - **Behind-CTA glow is approximated.** The C# build uses a WPF
//!   `RadialGradientBrush` + `BlurEffect`. gpui has no radial
//!   gradient primitive; we render a soft accent-tinted rounded box
//!   underneath the CTA stack with low opacity, which reads as the
//!   same atmospheric accent without the per-frame blur cost.

use std::sync::Arc;

use codescope_core::Theme;
use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Styled, Window, div, px,
};

type TileHandler = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App) + 'static>;

use crate::app::AppShell;
use crate::theme;

/// Render the empty-state hero. Inserted in place of the group panes
/// when no projects are registered. Returns an `AnyElement` so the
/// caller can swap it directly into the work-area slot.
///
/// All click handlers are wired via `cx.listener(...)` against
/// `AppShell` so the entry points (open the New Project dialog, open
/// the command palette, toggle Overview) reuse the same code paths
/// the sidebar / palette already hit.
pub fn render(theme: &Arc<Theme>, cx: &mut Context<AppShell>) -> AnyElement {
    let ink = theme::ink(theme);
    let ink_dim = theme::ink_dim(theme);
    let ink_muted = theme::ink_muted(theme);
    let ink_ghost = theme::ink_ghost(theme);
    let canvas = theme::canvas(theme);
    let accent = theme::accent(theme);
    let divider = theme::divider(theme);
    let frost_10 = theme::frost_10(theme);
    // Soft accent wash for the behind-CTA glow. We can't do a true
    // radial gradient in gpui, so we approximate with a translucent
    // accent-tinted rounded box. ~20% alpha reads as a glow without
    // competing with the wordmark for visual weight.
    let accent_glow = gpui::Hsla { a: 0.18, ..accent };
    let accent_hover = gpui::Hsla {
        l: (accent.l + 0.08).min(1.0),
        ..accent
    };

    // ─── Hero stack content ───────────────────────────────────────
    let eyebrow = div()
        .text_size(px(11.0))
        .text_color(ink_muted)
        .font(theme::font_mono())
        .child("NO PROJECTS");

    // Wordmark: "Add a project" in ink + "." in accent. gpui composes
    // colours per-child element rather than via per-run runs, so we
    // build the wordmark as a flex row of two text spans.
    let wordmark = div()
        .flex()
        .flex_row()
        .items_baseline()
        .font(theme::font_sans())
        .text_size(px(64.0))
        .line_height(px(64.0))
        .child(div().text_color(ink).child("Add a project"))
        .child(div().text_color(accent).child("."));

    let tagline = div()
        .max_w(px(480.0))
        .text_size(px(14.0))
        .line_height(px(21.0))
        .text_color(ink_dim)
        .font(theme::font_sans())
        .child(
            "CodeScope groups Git worktrees under projects and attaches an agent \
             session to each one. Point it at a repo to get started.",
        );

    // ─── Primary CTA ──────────────────────────────────────────────
    let primary_cta = div()
        .id("empty-state-add-project")
        .h(px(44.0))
        .px(px(18.0))
        .flex()
        .flex_row()
        .items_center()
        .rounded(px(6.0))
        .bg(accent)
        .text_color(gpui::black())
        .font(theme::font_sans())
        .text_size(px(14.0))
        .cursor_pointer()
        .hover(move |s| s.bg(accent_hover))
        .child("Add your first project")
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _: &MouseDownEvent, window, cx| {
                this.open_new_project_dialog(window, cx);
            }),
        );

    // Disabled "Clone from Git URL" ghost button. Matches the C# build —
    // the affordance is documented but the actual clone flow isn't
    // wired yet on either platform. We keep it disabled-looking so
    // users see the planned shape without thinking it's broken.
    let clone_ghost = div()
        .h(px(44.0))
        .px(px(14.0))
        .flex()
        .flex_row()
        .items_center()
        .rounded(px(6.0))
        .text_color(ink_ghost)
        .font(theme::font_sans())
        .text_size(px(14.0))
        .child("Clone from Git URL");

    let cta_row = div()
        .flex()
        .flex_row()
        .gap(px(10.0))
        .child(primary_cta)
        .child(clone_ghost);

    // ─── Quick-row tiles ──────────────────────────────────────────
    //
    // Three tiles, separated by 1px vertical dividers. Each tile is
    // a caret + label + chord-chip row.
    let drop_tile = quick_tile(
        ink_dim,
        ink_muted,
        accent,
        divider,
        frost_10,
        "Drop a folder to begin",
        "drag",
        None,
    );
    let palette_handler: TileHandler = Box::new(cx.listener(
        |this, _: &MouseDownEvent, window, cx| {
            this.open_command_palette(window, cx);
        },
    ));
    let palette_tile = quick_tile(
        ink_dim,
        ink_muted,
        accent,
        divider,
        frost_10,
        "Open command palette",
        "\u{2303}\u{21E7} P",
        Some(palette_handler),
    );
    let overview_handler: TileHandler = Box::new(cx.listener(
        |this, _: &MouseDownEvent, _window, cx| {
            this.set_show_overview(true, cx);
        },
    ));
    let overview_tile = quick_tile(
        ink_dim,
        ink_muted,
        accent,
        divider,
        frost_10,
        "Session overview",
        "\u{2303}\u{21E7} O",
        Some(overview_handler),
    );

    let separator = || div().w_px().bg(divider);

    // gpui flex children stretch by default, so no explicit
    // `items_stretch` is needed — the 1px vertical separators fill
    // the row height implicitly.
    let quick_row = div()
        .flex()
        .flex_row()
        .rounded(px(8.0))
        .border_1()
        .border_color(divider)
        .child(drop_tile.into_any_element())
        .child(separator())
        .child(palette_tile.into_any_element())
        .child(separator())
        .child(overview_tile.into_any_element());

    // ─── Compose ──────────────────────────────────────────────────
    let inner = div()
        .flex()
        .flex_col()
        .items_center()
        .w(px(760.0))
        .child(eyebrow)
        .child(div().h(px(20.0)))
        .child(wordmark)
        .child(div().h(px(16.0)))
        .child(div().flex().justify_center().child(tagline))
        .child(div().h(px(28.0)))
        .child(cta_row)
        .child(div().h(px(32.0)))
        .child(quick_row);

    // Behind-CTA accent glow — a soft tinted box, absolutely
    // positioned so it doesn't take part in the flex flow but still
    // tracks the hero stack via the parent's `flex justify_center`.
    // The hero stack sits inside a `relative` container so the
    // absolute child anchors against its bounds.
    let glow = div()
        .absolute()
        .w(px(480.0))
        .h(px(180.0))
        .rounded(px(90.0))
        .bg(accent_glow);

    div()
        .flex_grow()
        .flex()
        .items_center()
        .justify_center()
        .bg(canvas)
        .child(
            div()
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .child(glow)
                .child(inner),
        )
        .into_any_element()
}

/// Render one quick-row tile (caret + label + chord chip). The
/// optional `on_click` is wired via `on_mouse_down`; tiles without a
/// click handler (the "Drop a folder" affordance, since the drop
/// target is the whole window) render inert.
#[allow(clippy::too_many_arguments)]
fn quick_tile(
    label_color: gpui::Hsla,
    chip_color: gpui::Hsla,
    accent: gpui::Hsla,
    divider: gpui::Hsla,
    hover_bg: gpui::Hsla,
    label: &'static str,
    chord: &'static str,
    // Boxed so callers can mix interactive and inert tiles in one
    // `quick_tile` signature — generic `impl Fn` would force a
    // turbofish for the `None` case.
    on_click: Option<TileHandler>,
) -> impl IntoElement {
    let base = div()
        .id(label)
        .flex_grow()
        .flex()
        .flex_row()
        .items_center()
        .px(px(14.0))
        .py(px(12.0))
        .gap(px(8.0))
        // Caret glyph — mono font matches the chord chip / label so
        // the row reads as a single mono-spec line.
        .child(
            div()
                .text_color(accent)
                .opacity(0.9)
                .font(theme::font_mono())
                .text_size(px(12.0))
                .child("\u{25B8}"),
        )
        .child(
            div()
                .flex_grow()
                .font(theme::font_mono())
                .text_size(px(12.0))
                .text_color(label_color)
                .child(label),
        )
        .child(
            div()
                .px(px(6.0))
                .py(px(2.0))
                .rounded(px(3.0))
                .border_1()
                .border_color(divider)
                .font(theme::font_mono())
                .text_size(px(10.0))
                .text_color(chip_color)
                .child(chord),
        );

    if let Some(handler) = on_click {
        base.cursor_pointer()
            .hover(move |s| s.bg(hover_bg))
            .on_mouse_down(MouseButton::Left, handler)
            .into_any_element()
    } else {
        base.into_any_element()
    }
}
