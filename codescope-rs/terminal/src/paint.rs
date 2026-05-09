#![allow(clippy::too_many_arguments)]
//! Pixel-accurate paint pass for [`TerminalSnapshot`].
//!
//! The snapshot is rendered in three layers, in order:
//!
//! 1. **Default background.** One quad covering the entire bounds —
//!    cheap, and means runs with the default bg can skip emitting a
//!    quad at all.
//! 2. **Non-default backgrounds.** For each row, adjacent runs with the
//!    same non-default bg are merged into a single quad spanning their
//!    combined cell range. This keeps the GPU draw count down on long
//!    selection ranges and prompt-blocks.
//! 3. **Text + cursor.** Each [`StyledRun`] is shape-and-painted at
//!    `origin.x + start_col × cell_width`. The cursor (if visible) is
//!    drawn last, on top, with its own quad and re-shaped glyph so the
//!    block fill / beam / underline shapes look right.
//!
//! Compared to the previous flex-of-divs view, this:
//!
//! * positions every cell at exact pixel offsets — no sub-cell drift
//!   when shaping merges glyphs;
//! * supports proper block / beam / underline / hollow cursor shapes;
//! * leaves room for box-drawing overlays and selection underlays
//!   without restructuring the code path.

use alacritty_terminal::vte::ansi::CursorShape;
use gpui::{
    App, Bounds, Edges, Font, FontStyle, FontWeight, Hsla, Pixels, Point, SharedString, Size,
    TextRun, UnderlineStyle, Window, px, quad, transparent_black,
};

use crate::backend::{CursorInfo, StyledRun, TerminalSnapshot};

/// Paint the snapshot inside `bounds` using the given font metrics.
///
/// `cell_width` and `line_height` must be the actual measured values
/// from `window.text_system().shape_line(...)` — using a guess will
/// shift columns out of alignment.
///
/// `cursor_visible_now` is the blink-phase boolean: `true` paints the
/// cursor, `false` skips it. Steady-cursor snapshots ignore this and
/// always paint (see [`CursorInfo::blinking`]).
pub fn paint_snapshot(
    bounds: Bounds<Pixels>,
    snapshot: &TerminalSnapshot,
    font: &Font,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
    cursor_visible_now: bool,
    window: &mut Window,
    cx: &mut App,
) {
    // 1) default background — covers gaps between styled runs and the
    //    empty area below the last row of grid content.
    window.paint_quad(quad(
        bounds,
        px(0.0),
        snapshot.default_bg,
        Edges::<Pixels>::default(),
        transparent_black(),
        Default::default(),
    ));

    let origin = bounds.origin;
    let cell_w_f32: f32 = cell_width.into();
    let line_h_f32: f32 = line_height.into();
    if cell_w_f32 <= 0.0 || line_h_f32 <= 0.0 {
        return;
    }

    // 2) non-default backgrounds, merged across adjacent runs.
    for (row_idx, line) in snapshot.lines.iter().enumerate() {
        let y = origin.y + line_height * (row_idx as f32);
        let mut bg_start: Option<(usize, usize, Hsla)> = None; // start_col, end_col, color
        for run in line {
            if run.bg == snapshot.default_bg {
                if let Some((start, end, color)) = bg_start.take() {
                    paint_bg_rect(window, origin, y, start, end, color, cell_width, line_height);
                }
                continue;
            }
            match bg_start {
                Some((start, end, color)) if color == run.bg && end == run.start_col => {
                    bg_start = Some((start, run.start_col + run.len_cols, color));
                }
                Some((start, end, color)) => {
                    paint_bg_rect(window, origin, y, start, end, color, cell_width, line_height);
                    bg_start = Some((run.start_col, run.start_col + run.len_cols, run.bg));
                }
                None => {
                    bg_start = Some((run.start_col, run.start_col + run.len_cols, run.bg));
                }
            }
        }
        if let Some((start, end, color)) = bg_start {
            paint_bg_rect(window, origin, y, start, end, color, cell_width, line_height);
        }
    }

    // 3) text — shape and paint each run at its exact column.
    for (row_idx, line) in snapshot.lines.iter().enumerate() {
        let y = origin.y + line_height * (row_idx as f32);
        for run in line {
            paint_run(
                window,
                cx,
                origin,
                y,
                run,
                font,
                font_size,
                cell_width,
                line_height,
            );
        }
    }

    // 4) cursor on top — but only when the blink phase is on.
    //    Steady cursors (`blinking = false`) always paint.
    if let Some(cursor) = snapshot.cursor.as_ref() {
        if !cursor.blinking || cursor_visible_now {
            paint_cursor(
                window,
                cx,
                origin,
                cursor,
                font,
                font_size,
                cell_width,
                line_height,
            );
        }
    }
}

fn paint_bg_rect(
    window: &mut Window,
    origin: Point<Pixels>,
    y: Pixels,
    start_col: usize,
    end_col: usize,
    color: Hsla,
    cell_width: Pixels,
    line_height: Pixels,
) {
    let x = origin.x + cell_width * (start_col as f32);
    let width = cell_width * ((end_col - start_col) as f32);
    let bounds = Bounds {
        origin: Point { x, y },
        size: Size {
            width,
            height: line_height,
        },
    };
    window.paint_quad(quad(
        bounds,
        px(0.0),
        color,
        Edges::<Pixels>::default(),
        transparent_black(),
        Default::default(),
    ));
}

fn run_font(base: &Font, bold: bool, italic: bool) -> Font {
    let mut font = base.clone();
    font.weight = if bold { FontWeight::BOLD } else { FontWeight::NORMAL };
    font.style = if italic { FontStyle::Italic } else { FontStyle::Normal };
    font
}

fn paint_run(
    window: &mut Window,
    cx: &mut App,
    origin: Point<Pixels>,
    y: Pixels,
    run: &StyledRun,
    base_font: &Font,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
) {
    if run.text.is_empty() {
        return;
    }
    let font = run_font(base_font, run.bold, run.italic);
    let underline = run.underline.then(|| UnderlineStyle {
        thickness: px(1.0),
        color: Some(run.fg),
        wavy: false,
    });

    // Paint per-cell rather than as one batched `shape_line`. Box
    // drawing characters (`╭ ╮ ╰ ╯ ─ │` etc.) often shape through a
    // font-fallback whose advance differs slightly from the primary
    // font; batching lets that drift accumulate across the run, so
    // rounded boxes (claude-code's banner, vim's borders) end up
    // visually broken. Shaping each glyph at its exact `col ×
    // cell_width` position keeps every cell aligned no matter what
    // fallback served the glyph. Cost: one `shape_line` per cell
    // instead of per run; gpui caches glyph shaping internally so
    // the difference at a 100×30 grid is negligible.
    let mut col = run.start_col;
    for ch in run.text.chars() {
        // Wide chars (CJK, emoji) advance two cells; the snapshot has
        // already filtered the spacer cell, so we just bump the
        // column counter by 2 here.
        let cell_span = if is_wide_char(ch) { 2 } else { 1 };
        if ch != ' ' && ch != '\0' {
            let glyph: SharedString = ch.to_string().into();
            let text_run = TextRun {
                len: glyph.len(),
                font: font.clone(),
                color: run.fg,
                background_color: None,
                underline,
                strikethrough: None,
            };
            let shaped = window
                .text_system()
                .shape_line(glyph, font_size, &[text_run], None);
            let x = origin.x + cell_width * (col as f32);
            let _ = shaped.paint(Point { x, y }, line_height, window, cx);
        }
        col += cell_span;
    }
}

/// East-Asian-Wide / emoji probe matching what alacritty marks as a
/// `WIDE_CHAR`. Conservative: only the BMP ranges everyone agrees on
/// plus the supplementary symbol/emoji range. The snapshot is the
/// authoritative source for cell width — this is just a fallback
/// when iterating chars in paint.
fn is_wide_char(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x115F   // Hangul Jamo
        | 0x2E80..=0x303E // CJK Radicals
        | 0x3041..=0x33FF // Hiragana / Katakana / CJK Symbols
        | 0x3400..=0x4DBF // CJK Ext A
        | 0x4E00..=0x9FFF // CJK Unified
        | 0xA000..=0xA4CF // Yi
        | 0xAC00..=0xD7A3 // Hangul Syllables
        | 0xF900..=0xFAFF // CJK Compat
        | 0xFE30..=0xFE4F // CJK Compat Forms
        | 0xFF00..=0xFF60 // Fullwidth Forms
        | 0xFFE0..=0xFFE6
        | 0x1F300..=0x1FAFF // Emoji + symbols
        | 0x20000..=0x2FFFD
        | 0x30000..=0x3FFFD)
}

fn paint_cursor(
    window: &mut Window,
    cx: &mut App,
    origin: Point<Pixels>,
    cursor: &CursorInfo,
    base_font: &Font,
    font_size: Pixels,
    cell_width: Pixels,
    line_height: Pixels,
) {
    if cursor.row < 0 {
        return;
    }
    let x = origin.x + cell_width * (cursor.col as f32);
    let y = origin.y + line_height * (cursor.row as f32);

    match cursor.shape {
        CursorShape::Block => {
            let bounds = Bounds {
                origin: Point { x, y },
                size: Size {
                    width: cell_width,
                    height: line_height,
                },
            };
            window.paint_quad(quad(
                bounds,
                px(0.0),
                cursor.cursor_color,
                Edges::<Pixels>::default(),
                transparent_black(),
                Default::default(),
            ));
            // Re-paint the cell glyph in the cell's bg colour so it
            // stays readable on top of the cursor fill.
            paint_cursor_glyph(
                window,
                cx,
                Point { x, y },
                cursor,
                cursor.cell_bg,
                base_font,
                font_size,
                line_height,
            );
        }
        CursorShape::HollowBlock => {
            let bounds = Bounds {
                origin: Point { x, y },
                size: Size {
                    width: cell_width,
                    height: line_height,
                },
            };
            window.paint_quad(quad(
                bounds,
                px(0.0),
                transparent_black(),
                Edges::all(px(1.0)),
                cursor.cursor_color,
                Default::default(),
            ));
        }
        CursorShape::Beam => {
            let bounds = Bounds {
                origin: Point { x, y },
                size: Size {
                    width: px(1.5),
                    height: line_height,
                },
            };
            window.paint_quad(quad(
                bounds,
                px(0.0),
                cursor.cursor_color,
                Edges::<Pixels>::default(),
                transparent_black(),
                Default::default(),
            ));
        }
        CursorShape::Underline => {
            let height = px(1.5);
            let bounds = Bounds {
                origin: Point { x, y: y + line_height - height },
                size: Size {
                    width: cell_width,
                    height,
                },
            };
            window.paint_quad(quad(
                bounds,
                px(0.0),
                cursor.cursor_color,
                Edges::<Pixels>::default(),
                transparent_black(),
                Default::default(),
            ));
        }
        CursorShape::Hidden => {}
    }
}

fn paint_cursor_glyph(
    window: &mut Window,
    cx: &mut App,
    origin: Point<Pixels>,
    cursor: &CursorInfo,
    color: Hsla,
    base_font: &Font,
    font_size: Pixels,
    line_height: Pixels,
) {
    if cursor.character == ' ' || cursor.character == '\0' {
        return;
    }
    let font = run_font(base_font, cursor.bold, cursor.italic);
    let underline = cursor.underline.then(|| UnderlineStyle {
        thickness: px(1.0),
        color: Some(color),
        wavy: false,
    });
    let text: SharedString = cursor.character.to_string().into();
    let run = TextRun {
        len: text.len(),
        font,
        color,
        background_color: None,
        underline,
        strikethrough: None,
    };
    let shaped = window
        .text_system()
        .shape_line(text, font_size, &[run], None);
    let _ = shaped.paint(origin, line_height, window, cx);
}
