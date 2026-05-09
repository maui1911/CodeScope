//! Theme model.
//!
//! A `Theme` is a named bundle of colours that the terminal palette,
//! the app chrome, and any future viewers consume. Stored in this
//! UI-free crate so the same struct round-trips through `settings.json`
//! (serde) and the gpui paint code (the `app` crate maps `Rgb` →
//! `gpui::Hsla`).
//!
//! Terms borrowed from `src/CodeScope.App/Styles/DesignTokens.xaml`:
//!
//! * `canvas` / `ink` — pure surface + foreground.
//! * `accent` — the single decorated colour. Framer Blue in our default;
//!   themes can override but should still respect "one accent only".
//! * `frost_*` — translucent overlays for buttons / hover states.
//! * `terminal.*` — ANSI 16 + extended-256 + named slots, for the
//!   terminal renderer to resolve cell colours against.

use serde::{Deserialize, Serialize};

mod builtin_impl;

pub mod builtin {
    //! Built-in themes shipped with the binary. Names are stable
    //! identifiers that `settings.json` references.
    pub use super::builtin_impl::*;
}

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
