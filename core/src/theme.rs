//! Theme model.
//!
//! A `Theme` is a named bundle of colours that the terminal palette,
//! the app chrome, and any future viewers consume. Stored in this
//! UI-free crate so the same struct round-trips through `settings.json`
//! (serde) and the gpui paint code (the `app` crate maps `Rgb` →
//! `gpui::Hsla`).
//!
//! Terms borrowed from `legacy:CodeScope.App/Styles/DesignTokens.xaml`:
//!
//! * `canvas` / `ink` — pure surface + foreground.
//! * `accent` — the single decorated colour. Framer Blue in our default;
//!   themes can override but should still respect "one accent only".
//! * `frost_*` — translucent overlays for buttons / hover states.
//! * `terminal.*` — ANSI 16 + extended-256 + named slots, for the
//!   terminal renderer to resolve cell colours against.

use serde::{Deserialize, Serialize};

/// Built-in themes shipped with the binary. Names are stable
/// identifiers that `settings.json` references.
pub mod builtin;

/// 8-bit-per-channel colour, serde-friendly. We deliberately don't
/// pull `gpui::Hsla` into core — the UI crate converts at the edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(from = "RgbWire", into = "RgbWire")]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// Construct from a packed `0xRRGGBB` int — convenient when
    /// transcribing tokens from `DesignTokens.xaml`.
    pub const fn from_hex(hex: u32) -> Self {
        Self {
            r: ((hex >> 16) & 0xff) as u8,
            g: ((hex >> 8) & 0xff) as u8,
            b: (hex & 0xff) as u8,
        }
    }
}

/// `#rrggbb` string when serialised — what you'd type in
/// `settings.json`. Accepts `#rgb`, `#rrggbb`, and `0xRRGGBB`.
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum RgbWire {
    Hex(String),
    Triplet { r: u8, g: u8, b: u8 },
}

impl From<Rgb> for RgbWire {
    fn from(rgb: Rgb) -> Self {
        Self::Hex(format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b))
    }
}

impl From<RgbWire> for Rgb {
    fn from(wire: RgbWire) -> Self {
        match wire {
            RgbWire::Triplet { r, g, b } => Rgb { r, g, b },
            RgbWire::Hex(s) => parse_hex_or_default(&s),
        }
    }
}

fn parse_hex_or_default(s: &str) -> Rgb {
    let trimmed = s.trim_start_matches('#').trim_start_matches("0x");
    let parsed = match trimmed.len() {
        // `fff` → `ffffff`
        3 => u32::from_str_radix(trimmed, 16).ok().map(|v| {
            let r = ((v >> 8) & 0xf) as u8;
            let g = ((v >> 4) & 0xf) as u8;
            let b = (v & 0xf) as u8;
            Rgb { r: r * 0x11, g: g * 0x11, b: b * 0x11 }
        }),
        6 => u32::from_str_radix(trimmed, 16).ok().map(Rgb::from_hex),
        _ => None,
    };
    parsed.unwrap_or(Rgb { r: 0, g: 0, b: 0 })
}

/// Terminal-side palette. Mirrors alacritty's colour-table layout so
/// the UI crate can resolve `vte::ansi::Color` against a `Theme`
/// without an extra mapping layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemePalette {
    /// 16 ANSI colours (0..16). Indexed: 0 black, 1 red, …,
    /// 7 white, 8..16 = bright variants.
    pub ansi: [Rgb; 16],
    /// 256-colour extended palette (16..256: 6×6×6 cube + grayscale
    /// ramp). 0..16 mirror `ansi`. We keep the full table here so
    /// themes can override individual cube cells if they ever want to.
    pub extended: Vec<Rgb>,
    /// Default foreground / background / cursor for cells that don't
    /// specify their own.
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
}

/// App-chrome palette. Used by the AppShell, sidebar, status bar etc.
/// Kept separate from `ThemePalette` so a theme can have a different
/// chrome look from its terminal look (e.g. light app chrome with a
/// dark terminal — common in IDE-style products).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThemeChrome {
    /// Base canvas (window background behind everything).
    pub canvas: Rgb,
    /// Slightly elevated surface — tab strip, sidebar, status bar.
    pub elevated: Rgb,
    /// Even-more-elevated surface — used for sidebar row hover /
    /// selection fill. Mirrors `Surface.Color.Elev = #FF141414` from
    /// `DesignTokens.xaml`; semantically the "card" / "active row"
    /// surface that sits one tier above the panel `elevated` colour.
    ///
    /// `#[serde(default)]` keeps older serialised theme JSON
    /// deserialising cleanly when the field is absent — important for
    /// any external theme bundle that pre-dates this field. The
    /// shipped `settings.json` only references themes by `name` so it
    /// is unaffected.
    #[serde(default = "default_surface_elev")]
    pub surface_elev: Rgb,
    /// Primary text on canvas / elevated.
    pub ink: Rgb,
    /// Muted text (timestamps, hints).
    pub ink_muted: Rgb,
    /// 1px divider lines.
    pub divider: Rgb,
    /// Single accent. Used sparingly — focus rings, active tab top
    /// border. DESIGN.md §7 explicitly forbids a second accent.
    pub accent: Rgb,
}

/// `Surface.Color.Elev` from `DesignTokens.xaml` (`#FF141414`).
/// Serde default for `ThemeChrome::surface_elev` so external theme
/// bundles serialised before this field existed still deserialise.
fn default_surface_elev() -> Rgb { Rgb::from_hex(0x141414) }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    /// Stable identifier. `settings.json` references themes by this.
    pub name: String,
    /// Display name for theme pickers (later).
    pub display_name: String,
    /// Whether this is a dark-on-light or light-on-dark theme. Used to
    /// pick a sensible window backdrop when the OS appearance changes.
    pub dark: bool,
    pub chrome: ThemeChrome,
    pub palette: ThemePalette,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgb_from_hex_packs_components() {
        let c = Rgb::from_hex(0xa1_b2_c3);
        assert_eq!(c.r, 0xa1);
        assert_eq!(c.g, 0xb2);
        assert_eq!(c.b, 0xc3);
    }

    #[test]
    fn rgb_round_trips_via_hex_wire() {
        let original = Rgb::new(0x12, 0x34, 0x56);
        let json = serde_json::to_string(&original).unwrap();
        assert_eq!(json, "\"#123456\"");
        let back: Rgb = serde_json::from_str(&json).unwrap();
        assert_eq!(back, original);
    }

    #[test]
    fn rgb_accepts_short_hex_form() {
        // `"#abc"` should round-trip to `0xaa, 0xbb, 0xcc`.
        let parsed: Rgb = serde_json::from_str("\"#abc\"").unwrap();
        assert_eq!(parsed, Rgb { r: 0xaa, g: 0xbb, b: 0xcc });
    }

    #[test]
    fn rgb_accepts_0x_prefix() {
        let parsed: Rgb = serde_json::from_str("\"0x102030\"").unwrap();
        assert_eq!(parsed, Rgb { r: 0x10, g: 0x20, b: 0x30 });
    }

    #[test]
    fn rgb_falls_back_to_black_on_garbage() {
        let parsed: Rgb = serde_json::from_str("\"not-a-color\"").unwrap();
        assert_eq!(parsed, Rgb { r: 0, g: 0, b: 0 });
    }

    #[test]
    fn rgb_accepts_triplet_form() {
        let parsed: Rgb = serde_json::from_str("{\"r\":1,\"g\":2,\"b\":3}").unwrap();
        assert_eq!(parsed, Rgb { r: 1, g: 2, b: 3 });
    }

    #[test]
    fn builtin_by_name_falls_back_to_default() {
        let theme = builtin::by_name("does-not-exist");
        assert_eq!(theme.name, builtin::DEFAULT_NAME);
    }

    #[test]
    fn builtin_all_names_are_unique_and_resolvable() {
        let themes = builtin::all();
        assert!(!themes.is_empty(), "at least one built-in theme must ship");
        // No duplicate stable ids.
        let mut names: Vec<&str> = themes.iter().map(|t| t.name.as_str()).collect();
        names.sort();
        let len_before = names.len();
        names.dedup();
        assert_eq!(names.len(), len_before, "duplicate built-in theme name");
        // by_name resolves every shipped theme without fallback.
        for theme in &themes {
            let resolved = builtin::by_name(&theme.name);
            assert_eq!(resolved.name, theme.name);
        }
    }

    #[test]
    fn theme_round_trips_through_json() {
        let theme = builtin::codescope_default();
        let json = serde_json::to_string(&theme).unwrap();
        let back: Theme = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, theme.name);
        assert_eq!(back.dark, theme.dark);
        assert_eq!(back.chrome.canvas, theme.chrome.canvas);
        assert_eq!(back.palette.ansi[0], theme.palette.ansi[0]);
    }

    #[test]
    fn theme_chrome_surface_elev_defaults_when_absent() {
        // Hand-rolled JSON omitting surface_elev should deserialize
        // (older external theme bundles pre-date that field).
        let json = "{\"canvas\":\"#000000\",\"elevated\":\"#111111\",\
            \"ink\":\"#ffffff\",\"ink_muted\":\"#808080\",\
            \"divider\":\"#222222\",\"accent\":\"#0066ff\"}";
        let chrome: ThemeChrome = serde_json::from_str(json).unwrap();
        assert_eq!(chrome.surface_elev, Rgb::from_hex(0x141414));
    }
}
