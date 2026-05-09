//! gpui-side theme helpers.
//!
//! `codescope-core` owns the theme *data* (serializable, UI-free).
//! This module is the bridge: it reads the active `core::Theme` and
//! hands gpui-flavoured `Hsla` values back to the renderer. When we
//! later add a theme picker / live reload, the `Theme` reference in
//! `AppShell` gets swapped and every accessor here returns the new
//! values on the next render — no touch required in the call sites.

use codescope_core::{Rgb, Theme};
use gpui::{Hsla, hsla};

/// Convert an 8-bit-per-channel RGB triplet to gpui's `Hsla`. Direct
/// translation, no gamma — gpui handles colour-space matters
/// internally.
pub fn rgb_to_hsla(rgb: Rgb) -> Hsla {
    let r = rgb.r as f32 / 255.0;
    let g = rgb.g as f32 / 255.0;
    let b = rgb.b as f32 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    let l = (max + min) / 2.0;
    let s = if delta == 0.0 {
        0.0
    } else {
        delta / (1.0 - (2.0 * l - 1.0).abs())
    };
    let h = if delta == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / delta).rem_euclid(6.0))
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h } / 360.0;
    hsla(h, s, l, 1.0)
}

/// Same as [`rgb_to_hsla`] but with an explicit alpha (0.0–1.0). The
/// frosted-glass surfaces in the chrome are derived this way:
/// `with_alpha(theme.chrome.ink, 0.10)` = `frost_10`.
pub fn with_alpha(rgb: Rgb, alpha: f32) -> Hsla {
    let mut color = rgb_to_hsla(rgb);
    color.a = alpha.clamp(0.0, 1.0);
    color
}

// ─── Chrome accessors ───────────────────────────────────────────────
//
// Tiny wrappers around the theme's `chrome` block. Renderers call
// `theme::canvas(theme)` instead of reaching into `theme.chrome.canvas`
// directly so we can swap out the lookup later (e.g. when a tab can
// override its accent without mutating the global theme).

pub fn canvas(theme: &Theme) -> Hsla { rgb_to_hsla(theme.chrome.canvas) }
pub fn elevated(theme: &Theme) -> Hsla { rgb_to_hsla(theme.chrome.elevated) }
pub fn ink(theme: &Theme) -> Hsla { rgb_to_hsla(theme.chrome.ink) }
pub fn ink_muted(theme: &Theme) -> Hsla { rgb_to_hsla(theme.chrome.ink_muted) }
pub fn divider(theme: &Theme) -> Hsla { rgb_to_hsla(theme.chrome.divider) }
pub fn accent(theme: &Theme) -> Hsla { rgb_to_hsla(theme.chrome.accent) }

/// Frosted-glass overlays — ink at varying alpha, the same trick the
/// C# build uses for hover states and button surfaces.
pub fn frost_10(theme: &Theme) -> Hsla { with_alpha(theme.chrome.ink, 0.10) }
pub fn frost_20(theme: &Theme) -> Hsla { with_alpha(theme.chrome.ink, 0.20) }
#[allow(dead_code)]
pub fn frost_50(theme: &Theme) -> Hsla { with_alpha(theme.chrome.ink, 0.50) }

/// Dim variants of the ink for placeholders / inactive labels.
pub fn ink_dim(theme: &Theme) -> Hsla { with_alpha(theme.chrome.ink, 0.60) }
pub fn ink_ghost(theme: &Theme) -> Hsla { with_alpha(theme.chrome.ink, 0.40) }

/// Status-dot colour. Hard-coded across themes for now — a green dot
/// reads as "running" in every dark theme we ship. Themable later.
pub fn status_running() -> Hsla { rgb_to_hsla(Rgb::from_hex(0x22c55e)) }

/// Foreground for destructive context-menu entries ("Remove project",
/// "Discard changes…"). Mirrors `Ctx.Color.Danger` from the C# build's
/// `ContextMenuStyles.xaml`. Hard-coded — a danger red that reads
/// across every theme. Themable later.
pub fn danger(_theme: &Theme) -> Hsla { rgb_to_hsla(Rgb::from_hex(0xff8a8a)) }
