//! ANSI / 256-colour palette and resolver for alacritty cell colours.
//!
//! Lifted from `vendor/gpui-terminal/src/colors.rs` (MIT OR Apache-2.0)
//! and trimmed to what we need: a fixed default palette plus the
//! `resolve` step that turns a `Color` into a gpui `Hsla`. The builder
//! API is dropped — themes will come back as a single `Theme` struct
//! once we have one, not as ad-hoc setters.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
use gpui::Hsla;

/// Concrete colour palette used to resolve alacritty `Color` values.
/// Each colour is kept in both `Hsla` (for the gpui paint pipeline)
/// and `Rgb` (for OSC 4 / OSC 10-12 query responses, where the
/// terminal must return the underlying RGB triplet to the TUI).
#[derive(Debug, Clone)]
pub struct ColorPalette {
    ansi: [Hsla; 16],
    extended: [Hsla; 256],
    ansi_rgb: [Rgb; 16],
    extended_rgb: [Rgb; 256],
    pub foreground: Hsla,
    pub background: Hsla,
    pub cursor: Hsla,
    pub foreground_rgb: Rgb,
    pub background_rgb: Rgb,
    pub cursor_rgb: Rgb,
}

impl Default for ColorPalette {
    fn default() -> Self {
        // VS Code's default-dark palette — not an accident: the user is
        // already on it elsewhere, so the colours match.
        let ansi_rgb = [
            Rgb { r: 0x00, g: 0x00, b: 0x00 },
            Rgb { r: 0xcd, g: 0x31, b: 0x31 },
            Rgb { r: 0x0d, g: 0xbc, b: 0x79 },
            Rgb { r: 0xe5, g: 0xe5, b: 0x10 },
            Rgb { r: 0x24, g: 0x72, b: 0xc8 },
            Rgb { r: 0xbc, g: 0x3f, b: 0xbc },
            Rgb { r: 0x11, g: 0xa8, b: 0xcd },
            Rgb { r: 0xcc, g: 0xcc, b: 0xcc },
            Rgb { r: 0x66, g: 0x66, b: 0x66 },
            Rgb { r: 0xf1, g: 0x4c, b: 0x4c },
            Rgb { r: 0x23, g: 0xd1, b: 0x8b },
            Rgb { r: 0xf5, g: 0xf5, b: 0x43 },
            Rgb { r: 0x3b, g: 0x8e, b: 0xea },
            Rgb { r: 0xd6, g: 0x70, b: 0xd6 },
            Rgb { r: 0x29, g: 0xb8, b: 0xdb },
            Rgb { r: 0xff, g: 0xff, b: 0xff },
        ];
        let ansi = ansi_rgb.map(rgb_to_hsla);

        // 256-colour palette: 0..16 = ANSI; 16..232 = 6×6×6 cube;
        // 232..256 = grayscale ramp.
        let mut extended_rgb = [Rgb { r: 0, g: 0, b: 0 }; 256];
        extended_rgb[0..16].copy_from_slice(&ansi_rgb);
        let mut idx = 16;
        for r in 0..6u8 {
            for g in 0..6u8 {
                for b in 0..6u8 {
                    let comp = |c: u8| if c == 0 { 0 } else { 55 + c * 40 };
                    extended_rgb[idx] = Rgb {
                        r: comp(r),
                        g: comp(g),
                        b: comp(b),
                    };
                    idx += 1;
                }
            }
        }
        for i in 0..24u8 {
            let v = 8 + i * 10;
            extended_rgb[232 + i as usize] = Rgb { r: v, g: v, b: v };
        }
        let extended = extended_rgb.map(rgb_to_hsla);

        let foreground_rgb = Rgb { r: 0xcc, g: 0xcc, b: 0xcc };
        let background_rgb = Rgb { r: 0x1e, g: 0x1e, b: 0x1e };
        let cursor_rgb = Rgb { r: 0xff, g: 0xff, b: 0xff };

        Self {
            ansi,
            extended,
            ansi_rgb,
            extended_rgb,
            foreground: rgb_to_hsla(foreground_rgb),
            background: rgb_to_hsla(background_rgb),
            cursor: rgb_to_hsla(cursor_rgb),
            foreground_rgb,
            background_rgb,
            cursor_rgb,
        }
    }
}

impl ColorPalette {
    /// Map an alacritty cell colour to a gpui `Hsla`. `colors` is the
    /// per-terminal override table that themes write into; named colours
    /// consult it before falling back to our fixed palette.
    pub fn resolve(&self, color: Color, colors: &Colors) -> Hsla {
        match color {
            Color::Named(named) => {
                if let Some(rgb) = colors[named] {
                    return rgb_to_hsla(rgb);
                }
                let idx = named as usize;
                if idx < 16 {
                    return self.ansi[idx];
                }
                match named {
                    NamedColor::Foreground => self.foreground,
                    NamedColor::Background => self.background,
                    NamedColor::Cursor => self.cursor,
                    NamedColor::DimForeground => dim(self.foreground),
                    NamedColor::BrightForeground => bright(self.foreground),
                    NamedColor::DimBlack => dim(self.ansi[0]),
                    NamedColor::DimRed => dim(self.ansi[1]),
                    NamedColor::DimGreen => dim(self.ansi[2]),
                    NamedColor::DimYellow => dim(self.ansi[3]),
                    NamedColor::DimBlue => dim(self.ansi[4]),
                    NamedColor::DimMagenta => dim(self.ansi[5]),
                    NamedColor::DimCyan => dim(self.ansi[6]),
                    NamedColor::DimWhite => dim(self.ansi[7]),
                    _ => self.foreground,
                }
            }
            Color::Spec(rgb) => rgb_to_hsla(rgb),
            Color::Indexed(idx) => self.extended[idx as usize],
        }
    }

    /// Resolve an alacritty colour-table index back to an `Rgb` triplet
    /// for OSC 4 / OSC 10-12 query responses. The index follows
    /// alacritty's `Colors` layout: 0..16 = ANSI, 16..256 = 256-colour
    /// extended palette, 256+ = `NamedColor` slots (Foreground=256,
    /// Background=257, Cursor=258, …). Per-terminal overrides (set via
    /// OSC 4 / 10-12 setters) live in `colors[index]` and take
    /// precedence over our defaults.
    pub fn resolve_rgb(&self, index: usize, colors: &Colors) -> Rgb {
        if let Some(rgb) = colors[index] {
            return rgb;
        }
        self.resolve_rgb_no_overrides(index)
    }

    /// Resolve an alacritty colour-table index back to an `Rgb` from
    /// the *default* palette only — no `Colors` overrides consulted.
    /// Used by [`EventProxy`] from the event-loop thread, which can't
    /// safely lock the `Term` to read overrides.
    pub fn resolve_rgb_no_overrides(&self, index: usize) -> Rgb {
        match index {
            i if i < 16 => self.ansi_rgb[i],
            i if i < 256 => self.extended_rgb[i],
            i if i == NamedColor::Foreground as usize => self.foreground_rgb,
            i if i == NamedColor::Background as usize => self.background_rgb,
            i if i == NamedColor::Cursor as usize => self.cursor_rgb,
            _ => self.foreground_rgb,
        }
    }
}

fn dim(mut c: Hsla) -> Hsla {
    c.l *= 0.7;
    c
}

fn bright(mut c: Hsla) -> Hsla {
    c.l = (c.l * 1.2).min(1.0);
    c
}

fn rgb_to_hsla(rgb: Rgb) -> Hsla {
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
        60.0 * (((g - b) / delta) % 6.0)
    } else if max == g {
        60.0 * (((b - r) / delta) + 2.0)
    } else {
        60.0 * (((r - g) / delta) + 4.0)
    };
    let h = if h < 0.0 { h + 360.0 } else { h } / 360.0;
    Hsla { h, s, l, a: 1.0 }
}
