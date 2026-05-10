//! gpui-side theme helpers.
//!
//! `codescope-core` owns the theme *data* (serializable, UI-free).
//! This module is the bridge: it reads the active `core::Theme` and
//! hands gpui-flavoured `Hsla` values back to the renderer. When we
//! later add a theme picker / live reload, the `Theme` reference in
//! `AppShell` gets swapped and every accessor here returns the new
//! values on the next render — no touch required in the call sites.

use std::sync::OnceLock;

use codescope_core::{Rgb, Theme};
use gpui::{Font, FontFallbacks, FontFeatures, FontStyle, FontWeight, Hsla, SharedString, hsla};

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

/// Worktree dirty indicator. Amber, distinct enough from
/// `status_running` (green) and `danger` (red) that the user can
/// tell at a glance.
pub fn status_dirty(_theme: &Theme) -> Hsla { rgb_to_hsla(Rgb::from_hex(0xf5a623)) }

/// Worktree clean indicator. Reuses the accent colour so a fully-
/// clean worktree visually rhymes with the focus / accent rail
/// elsewhere in the chrome.
pub fn status_clean(theme: &Theme) -> Hsla { accent(theme) }

/// Foreground for destructive context-menu entries ("Remove project",
/// "Discard changes…"). Mirrors `Ctx.Color.Danger` from the C# build's
/// `ContextMenuStyles.xaml`. Hard-coded — a danger red that reads
/// across every theme. Themable later.
pub fn danger(_theme: &Theme) -> Hsla { rgb_to_hsla(Rgb::from_hex(0xff8a8a)) }

/// `Signal.Color.Ok` from `DesignTokens.xaml` (#FF4BD87B). Used for
/// the status-bar session dot when the focused Claude session is
/// idle, the agent rollup's idle counter dot, and any other
/// "everything is fine" green in the chrome.
pub fn signal_ok() -> Hsla { rgb_to_hsla(Rgb::from_hex(0x4BD87B)) }

/// `Signal.Color.Warn` from `DesignTokens.xaml` (#FFFF5A5A). Used for
/// the status-bar session dot when the focused Claude session is
/// busy / pending tool use, the agent rollup's busy counter dot,
/// and the notifications popover's `SessionWaiting` kind dot.
pub fn signal_warn() -> Hsla { rgb_to_hsla(Rgb::from_hex(0xFF5A5A)) }

// ─── Font accessors ─────────────────────────────────────────────────
//
// Mirror the `Fig.Font.Sans` / `Fig.Font.Mono` resources from the C#
// build's `DesignTokens.xaml`. Sidebar (and, by extension, the rest
// of the chrome) uses two families: a variable sans for prose / UI
// labels, and a monospace for branch names, status slugs, keymap
// hints — anything the user reads as "code-like" data. The C# values
// (Windows-native fallbacks for the Framer reference fonts) are:
//
//     Fig.Font.Sans  = Segoe UI Variable Display, Segoe UI Variable,
//                       Segoe UI, Inter, system-ui
//     Fig.Font.Mono  = FiraCode Nerd Font Mono, Cascadia Mono,
//                       Cascadia Code, Consolas, Azeret Mono, menlo
//
// gpui resolves a single primary family + an optional fallback list;
// we hand it the same chains so missing-on-this-machine families
// degrade the same way they do in WPF.

/// Sans-serif `Font` for chrome labels — pass to `.font(...)` on a
/// gpui element. Equivalent to applying `Fig.Font.Sans` in the C#
/// build's XAML. Both the primary family and the ordered fallback
/// list mirror `<FontFamily x:Key="Fig.Font.Sans">` from
/// `DesignTokens.xaml`. The result is built once and cached in a
/// `OnceLock`; callers get a cheap `Font` clone (the inner
/// `FontFallbacks` is `Arc<Vec<String>>`, so the clone is two
/// `Arc::clone`s, no per-render heap churn).
pub fn font_sans() -> Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| Font {
        family: SharedString::new_static("Segoe UI Variable Display"),
        features: FontFeatures::default(),
        // Fallback chain copied verbatim (case included) from
        // `Fig.Font.Sans` so a `cargo test` failure on the ordered-
        // list assertion catches accidental drift.
        fallbacks: Some(FontFallbacks::from_fonts(vec![
            "Segoe UI Variable".to_string(),
            "Segoe UI".to_string(),
            "Inter".to_string(),
            "system-ui".to_string(),
        ])),
        weight: FontWeight::default(),
        style: FontStyle::default(),
    })
    .clone()
}

/// Monospace `Font` for chrome data — pass to `.font(...)` on a
/// gpui element. Equivalent to applying `Fig.Font.Mono` in the C#
/// build's XAML. The primary family is `FiraCode Nerd Font Mono`,
/// which is also the embedded terminal's default
/// (`vendor/gpui-terminal`'s default config), so when that font is
/// installed sidebar branch labels and the shell render in the
/// same metal. Cached + cloned the same way as `font_sans`.
pub fn font_mono() -> Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| Font {
        family: SharedString::new_static("FiraCode Nerd Font Mono"),
        features: FontFeatures::default(),
        // Fallback chain copied verbatim (case included) from
        // `Fig.Font.Mono` — note `menlo` is intentionally lower-
        // case to match the XAML token character-for-character.
        fallbacks: Some(FontFallbacks::from_fonts(vec![
            "Cascadia Mono".to_string(),
            "Cascadia Code".to_string(),
            "Consolas".to_string(),
            "Azeret Mono".to_string(),
            "menlo".to_string(),
        ])),
        weight: FontWeight::default(),
        style: FontStyle::default(),
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lock the full `Fig.Font.Sans` ordering. Asserting the exact
    /// list (not just `contains`) catches accidental reorders /
    /// omissions / additions that would silently drift the chrome
    /// away from the C# build's typography.
    #[test]
    fn font_sans_chain_matches_fig_font_sans() {
        let f = font_sans();
        assert_eq!(f.family.as_ref(), "Segoe UI Variable Display");
        let fallbacks = f.fallbacks.expect("sans fallbacks present");
        assert_eq!(
            fallbacks.fallback_list(),
            &[
                "Segoe UI Variable".to_string(),
                "Segoe UI".to_string(),
                "Inter".to_string(),
                "system-ui".to_string(),
            ]
        );
    }

    /// Lock the full `Fig.Font.Mono` ordering — same reasoning as
    /// the sans test: a parity helper is only useful if drift gets
    /// caught at compile time. `menlo` is lowercase on purpose to
    /// match the XAML token character-for-character.
    #[test]
    fn font_mono_chain_matches_fig_font_mono() {
        let f = font_mono();
        assert_eq!(f.family.as_ref(), "FiraCode Nerd Font Mono");
        let fallbacks = f.fallbacks.expect("mono fallbacks present");
        assert_eq!(
            fallbacks.fallback_list(),
            &[
                "Cascadia Mono".to_string(),
                "Cascadia Code".to_string(),
                "Consolas".to_string(),
                "Azeret Mono".to_string(),
                "menlo".to_string(),
            ]
        );
    }

    /// Both helpers cache their `Font` in a `OnceLock`, so repeated
    /// calls return the same `Arc<Vec<String>>` underneath the
    /// `FontFallbacks` clone — no per-render allocation.
    #[test]
    fn font_helpers_share_cached_fallbacks_across_calls() {
        let a = font_sans().fallbacks.expect("sans fallbacks");
        let b = font_sans().fallbacks.expect("sans fallbacks");
        assert!(std::sync::Arc::ptr_eq(&a.0, &b.0));

        let m1 = font_mono().fallbacks.expect("mono fallbacks");
        let m2 = font_mono().fallbacks.expect("mono fallbacks");
        assert!(std::sync::Arc::ptr_eq(&m1.0, &m2.0));
    }
}
