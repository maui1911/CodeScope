//! gpui-side theme helpers.
//!
//! `codescope-core` owns the theme *data* (serializable, UI-free).
//! This module is the bridge: it reads the active `core::Theme` and
//! hands gpui-flavoured `Hsla` values back to the renderer. When we
//! later add a theme picker / live reload, the `Theme` reference in
//! `AppShell` gets swapped and every accessor here returns the new
//! values on the next render — no touch required in the call sites.

#[cfg(windows)]
use std::collections::HashSet;
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
/// `Surface.Color.Elev` from `DesignTokens.xaml` (`#FF141414` on the
/// default theme). Used as the sidebar row hover / selection fill —
/// see `SidebarView.xaml` `IsMouseOver` / `IsSelected` triggers, which
/// hard-set the row background to `#141414`. The frosted-glass
/// `frost_10` overlay was the previous Rust approximation but renders
/// noticeably lighter than C#; using the canonical elev colour gives
/// pixel-level parity with the C# build.
pub fn surface_elev(theme: &Theme) -> Hsla { rgb_to_hsla(theme.chrome.surface_elev) }
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

/// Worktree clean indicator. Reuses the accent colour so a fully-
/// clean worktree visually rhymes with the focus / accent rail
/// elsewhere in the chrome.
pub fn status_clean(theme: &Theme) -> Hsla { accent(theme) }

/// `Text.Color.Faint` from `DesignTokens.xaml` (`#FF606060`). Used for
/// the sidebar "PROJECTS" caption, status-slug labels, history
/// timestamps, the Overview keycap, and any other tertiary-ink slot
/// the C# build paints with `Text.Faint`. Hard-coded — themable later
/// when a theme picker actually wants to override it; in the meantime
/// every shipped chrome tier (default + the four guest themes) reads
/// fine at this grey.
pub fn text_faint() -> Hsla { rgb_to_hsla(Rgb::from_hex(0x606060)) }

/// Foreground for destructive context-menu entries ("Remove project",
/// "Discard changes…"). Mirrors `Ctx.Color.Danger` from the C# build's
/// `ContextMenuStyles.xaml`. Hard-coded — a danger red that reads
/// across every theme. Themable later — keep the call sites theme-aware
/// at that point.
pub fn danger() -> Hsla { rgb_to_hsla(Rgb::from_hex(0xff8a8a)) }

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
// unlike WPF, a missing primary can produce ugly platform fallback on
// some machines, so we resolve the primary to the first installed
// family in the C# chain and keep the rest as per-glyph fallbacks.

const FIG_FONT_SANS: &[&str] = &[
    "Segoe UI Variable Display",
    "Segoe UI Variable",
    "Segoe UI",
    "Inter",
    "system-ui",
];

const FIG_FONT_MONO: &[&str] = &[
    "FiraCode Nerd Font Mono",
    "Cascadia Mono",
    "Cascadia Code",
    "Consolas",
    "Azeret Mono",
    "menlo",
];

/// Resolve a font stack to the first family that appears installed on
/// this machine. If probing is unavailable or nothing matches, return
/// the first entry so non-Windows platforms keep their normal gpui / OS
/// fallback behaviour.
pub fn resolve_font_family(candidates: &[String]) -> Option<SharedString> {
    let borrowed: Vec<&str> = candidates.iter().map(String::as_str).collect();
    resolve_font_family_names(&borrowed)
}

fn resolve_font_family_names(candidates: &[&str]) -> Option<SharedString> {
    let family = candidates
        .iter()
        .copied()
        .find(|candidate| font_family_is_installed(candidate))
        .or_else(|| candidates.first().copied())?;
    Some(family.to_string().into())
}

#[cfg(windows)]
fn font_family_is_installed(family: &str) -> bool {
    installed_font_entries()
        .iter()
        .any(|entry| font_entry_matches_family(entry, family))
}

#[cfg(not(windows))]
fn font_family_is_installed(_family: &str) -> bool { false }

#[cfg(windows)]
fn installed_font_entries() -> &'static HashSet<String> {
    static FONTS: OnceLock<HashSet<String>> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts = HashSet::new();
        collect_windows_font_entries(winreg::enums::HKEY_LOCAL_MACHINE, &mut fonts);
        collect_windows_font_entries(winreg::enums::HKEY_CURRENT_USER, &mut fonts);
        fonts
    })
}

#[cfg(windows)]
fn collect_windows_font_entries(root: winreg::HKEY, fonts: &mut HashSet<String>) {
    use winreg::RegKey;

    let root = RegKey::predef(root);
    let Ok(key) = root.open_subkey("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts") else {
        return;
    };

    for value in key.enum_values().filter_map(Result::ok) {
        fonts.insert(value.0.to_ascii_lowercase());
    }
}

#[cfg(windows)]
fn font_entry_matches_family(entry: &str, family: &str) -> bool {
    let family = family.to_ascii_lowercase();
    let Some(rest) = entry.strip_prefix(&family) else {
        return false;
    };

    if rest.is_empty() || rest.starts_with(" (") || rest.starts_with('(') {
        return true;
    }

    const STYLE_SUFFIXES: &[&str] = &[
        "regular", "bold", "italic", "bold italic", "black", "black italic", "light",
        "light italic", "semibold", "semibold italic", "semilight", "semilight italic",
    ];

    STYLE_SUFFIXES.iter().any(|style| {
        let suffix = format!(" {style}");
        rest == suffix
            || rest.starts_with(&format!("{suffix} "))
            || rest.starts_with(&format!("{suffix}("))
    })
}

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
        family: resolve_font_family_names(FIG_FONT_SANS)
            .unwrap_or_else(|| SharedString::new_static("Segoe UI")),
        features: FontFeatures::default(),
        // Fallback chain copied verbatim (case included) from
        // `Fig.Font.Sans` so a `cargo test` failure on the ordered-
        // list assertion catches accidental drift.
        fallbacks: Some(FontFallbacks::from_fonts(
            FIG_FONT_SANS.iter().map(|family| (*family).to_string()).collect(),
        )),
        weight: FontWeight::default(),
        style: FontStyle::default(),
    })
    .clone()
}

/// Monospace `Font` for chrome data — pass to `.font(...)` on a
/// gpui element. Equivalent to applying `Fig.Font.Mono` in the C#
/// build's XAML. The preferred family is `FiraCode Nerd Font Mono`
/// when installed; otherwise Windows machines fall back to the first
/// available built-in mono face from the C# stack (`Cascadia Mono`,
/// then `Cascadia Code`, then `Consolas`). Cached + cloned the same
/// way as `font_sans`.
pub fn font_mono() -> Font {
    static FONT: OnceLock<Font> = OnceLock::new();
    FONT.get_or_init(|| Font {
        family: resolve_font_family_names(FIG_FONT_MONO)
            .unwrap_or_else(|| SharedString::new_static("Cascadia Mono")),
        features: FontFeatures::default(),
        // Fallback chain copied verbatim (case included) from
        // `Fig.Font.Mono` — note `menlo` is intentionally lower-
        // case to match the XAML token character-for-character.
        fallbacks: Some(FontFallbacks::from_fonts(
            FIG_FONT_MONO.iter().map(|family| (*family).to_string()).collect(),
        )),
        weight: FontWeight::default(),
        style: FontStyle::default(),
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lock the canonical signal / text-faint hex values from
    /// `DesignTokens.xaml`. `signal_ok` / `signal_warn` / `text_faint`
    /// are hard-coded (themable later), so a regression here would
    /// silently drift the chrome away from the C# build:
    ///
    /// * `Signal.Color.Ok`   = `#FF4BD87B`
    /// * `Signal.Color.Warn` = `#FFFF5A5A`
    /// * `Text.Color.Faint`  = `#FF606060`
    #[test]
    fn signal_and_text_faint_match_design_tokens() {
        assert_eq!(signal_ok(),   rgb_to_hsla(Rgb::from_hex(0x4BD87B)));
        assert_eq!(signal_warn(), rgb_to_hsla(Rgb::from_hex(0xFF5A5A)));
        assert_eq!(text_faint(),  rgb_to_hsla(Rgb::from_hex(0x606060)));
    }

    /// Lock the full `Fig.Font.Sans` ordering. Asserting the exact
    /// list (not just `contains`) catches accidental reorders /
    /// omissions / additions that would silently drift the chrome
    /// away from the C# build's typography.
    #[test]
    fn font_sans_chain_matches_fig_font_sans() {
        let f = font_sans();
        assert!(FIG_FONT_SANS.contains(&f.family.as_ref()));
        let fallbacks = f.fallbacks.expect("sans fallbacks present");
        assert_eq!(
            fallbacks.fallback_list(),
            &FIG_FONT_SANS.iter().map(|family| (*family).to_string()).collect::<Vec<_>>()
        );
    }

    /// Lock the full `Fig.Font.Mono` ordering — same reasoning as
    /// the sans test: a parity helper is only useful if drift gets
    /// caught by `cargo test` / CI. `menlo` is lowercase on purpose
    /// to match the XAML token character-for-character.
    #[test]
    fn font_mono_chain_matches_fig_font_mono() {
        let f = font_mono();
        assert!(FIG_FONT_MONO.contains(&f.family.as_ref()));
        let fallbacks = f.fallbacks.expect("mono fallbacks present");
        assert_eq!(
            fallbacks.fallback_list(),
            &FIG_FONT_MONO.iter().map(|family| (*family).to_string()).collect::<Vec<_>>()
        );
    }

    /// Both helpers cache their `Font` in a `OnceLock`, so repeated
    /// calls share the same backing storage. We assert that via the
    /// public `fallback_list()` API rather than reaching into
    /// `FontFallbacks`' internal `Arc`: equal `as_ptr()` proves the
    /// returned slices point at the same allocation, which is only
    /// possible if both calls received the cached `Font`.
    #[cfg(windows)]
    #[test]
    fn windows_font_entry_matching_handles_registry_names_without_overmatching() {
        assert!(font_entry_matches_family(
            "cascadia mono regular (truetype)",
            "Cascadia Mono"
        ));
        assert!(font_entry_matches_family("consolas (truetype)", "Consolas"));
        assert!(font_entry_matches_family(
            "segoe ui variable (truetype)",
            "Segoe UI Variable"
        ));
        assert!(!font_entry_matches_family(
            "segoe ui variable (truetype)",
            "Segoe UI"
        ));
    }

    #[test]
    fn font_helpers_share_cached_fallbacks_across_calls() {
        let a = font_sans().fallbacks.expect("sans fallbacks");
        let b = font_sans().fallbacks.expect("sans fallbacks");
        assert_eq!(a.fallback_list().as_ptr(), b.fallback_list().as_ptr());

        let m1 = font_mono().fallbacks.expect("mono fallbacks");
        let m2 = font_mono().fallbacks.expect("mono fallbacks");
        assert_eq!(m1.fallback_list().as_ptr(), m2.fallback_list().as_ptr());
    }
}
