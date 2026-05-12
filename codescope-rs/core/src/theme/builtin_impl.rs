//! Built-in themes shipped with the binary.
//!
//! Add new themes by appending to [`all`] and following the same
//! shape: 16 ANSI colours + chrome tokens. The 256-colour extended
//! palette is filled in by [`build_extended`] (standard 6×6×6 cube +
//! grayscale ramp) so themes only have to provide 16 + named slots.
//!
//! Naming: stable kebab-case ids in `name`. `display_name` is what a
//! theme picker would show.

use super::{Rgb, Theme, ThemeChrome, ThemePalette};

/// Stable id of the default theme — what we fall back to if
/// `settings.json` references a theme we don't ship.
pub const DEFAULT_NAME: &str = "codescope-default";

/// Every theme bundled with the binary. Used by the settings loader
/// to resolve `theme: "<name>"` and by the (future) theme picker.
pub fn all() -> Vec<Theme> {
    vec![codescope_default(), vs_code_dark(), one_dark(), solarized_dark(), tokyo_night()]
}

/// Look up a theme by `name`. Returns `codescope-default` if the name
/// is unknown — better than crashing on a typo, and the user sees
/// their own name in the next save round-trip.
pub fn by_name(name: &str) -> Theme {
    all()
        .into_iter()
        .find(|t| t.name == name)
        .unwrap_or_else(codescope_default)
}

// ─── Theme constructors ──────────────────────────────────────────────

pub fn codescope_default() -> Theme {
    Theme {
        name: "codescope-default".into(),
        display_name: "CodeScope".into(),
        dark: true,
        chrome: ThemeChrome {
            // Canonical mapping from `DesignTokens.xaml`:
            //   canvas        = Fig.Color.Canvas         = #FF000000
            //   elevated      = Fig.Color.NearBlack      = #FF090909
            //   surface_elev  = Surface.Color.Elev       = #FF141414
            //   ink           = Fig.Color.Ink            = #FFFFFFFF
            //   ink_muted     = Fig.Color.InkMuted       = #FFA6A6A6
            //   divider       = Fig.Color.Divider (#22FFFFFF over #000000
            //                   ≈ #222222), kept hex-flattened so a
            //                   non-Hsla brush works the same.
            //   accent        = Framer.Color.Blue        = #FF0099FF
            canvas:       Rgb::from_hex(0x000000),
            elevated:     Rgb::from_hex(0x090909),
            surface_elev: Rgb::from_hex(0x141414),
            ink:          Rgb::from_hex(0xffffff),
            ink_muted:    Rgb::from_hex(0xa6a6a6),
            divider:      Rgb::from_hex(0x222222),
            accent:       Rgb::from_hex(0x0099ff),
        },
        palette: build_palette(
            // VS Code dark — same 16 colours as v0.x C# build.
            [
                Rgb::from_hex(0x000000), Rgb::from_hex(0xcd3131), Rgb::from_hex(0x0dbc79), Rgb::from_hex(0xe5e510),
                Rgb::from_hex(0x2472c8), Rgb::from_hex(0xbc3fbc), Rgb::from_hex(0x11a8cd), Rgb::from_hex(0xcccccc),
                Rgb::from_hex(0x666666), Rgb::from_hex(0xf14c4c), Rgb::from_hex(0x23d18b), Rgb::from_hex(0xf5f543),
                Rgb::from_hex(0x3b8eea), Rgb::from_hex(0xd670d6), Rgb::from_hex(0x29b8db), Rgb::from_hex(0xffffff),
            ],
            // Foreground / **terminal background** / cursor.
            //
            // `#0a0a0c` is the canonical terminal canvas for the
            // default theme — a near-black with a faint cool tint, so
            // the canvas reads as dark without going full `#000000`
            // (which can crush against IPS panel uniformity issues).
            // Reference points considered:
            //   * Windows Terminal default `#0c0c0c`,
            //   * Alacritty default      `#1d1f21`,
            //   * VS Code Dark+          `#1e1e1e` (too light for us).
            // The chrome `canvas` field stays `#000000` — only the
            // *terminal* canvas changed here.
            Rgb::from_hex(0xcccccc), Rgb::from_hex(0x0a0a0c), Rgb::from_hex(0xffffff),
        ),
    }
}

pub fn vs_code_dark() -> Theme {
    Theme {
        name: "vs-code-dark".into(),
        display_name: "VS Code Dark".into(),
        dark: true,
        chrome: ThemeChrome {
            canvas:       Rgb::from_hex(0x1e1e1e),
            elevated:     Rgb::from_hex(0x252526),
            surface_elev: Rgb::from_hex(0x2d2d30),
            ink:          Rgb::from_hex(0xcccccc),
            ink_muted:    Rgb::from_hex(0x9da5b4),
            divider:      Rgb::from_hex(0x3c3c3c),
            accent:       Rgb::from_hex(0x007acc),
        },
        palette: build_palette(
            [
                Rgb::from_hex(0x000000), Rgb::from_hex(0xcd3131), Rgb::from_hex(0x0dbc79), Rgb::from_hex(0xe5e510),
                Rgb::from_hex(0x2472c8), Rgb::from_hex(0xbc3fbc), Rgb::from_hex(0x11a8cd), Rgb::from_hex(0xcccccc),
                Rgb::from_hex(0x666666), Rgb::from_hex(0xf14c4c), Rgb::from_hex(0x23d18b), Rgb::from_hex(0xf5f543),
                Rgb::from_hex(0x3b8eea), Rgb::from_hex(0xd670d6), Rgb::from_hex(0x29b8db), Rgb::from_hex(0xffffff),
            ],
            Rgb::from_hex(0xcccccc), Rgb::from_hex(0x1e1e1e), Rgb::from_hex(0xaeafad),
        ),
    }
}

pub fn one_dark() -> Theme {
    // From github.com/atom/one-dark-syntax — the cult-classic Atom
    // theme, slightly tweaked for terminal use.
    Theme {
        name: "one-dark".into(),
        display_name: "One Dark".into(),
        dark: true,
        chrome: ThemeChrome {
            canvas:       Rgb::from_hex(0x282c34),
            elevated:     Rgb::from_hex(0x21252b),
            surface_elev: Rgb::from_hex(0x2c313c),
            ink:          Rgb::from_hex(0xabb2bf),
            ink_muted:    Rgb::from_hex(0x5c6370),
            divider:      Rgb::from_hex(0x3e4452),
            accent:       Rgb::from_hex(0x61afef),
        },
        palette: build_palette(
            [
                Rgb::from_hex(0x282c34), Rgb::from_hex(0xe06c75), Rgb::from_hex(0x98c379), Rgb::from_hex(0xe5c07b),
                Rgb::from_hex(0x61afef), Rgb::from_hex(0xc678dd), Rgb::from_hex(0x56b6c2), Rgb::from_hex(0xabb2bf),
                Rgb::from_hex(0x5c6370), Rgb::from_hex(0xe06c75), Rgb::from_hex(0x98c379), Rgb::from_hex(0xe5c07b),
                Rgb::from_hex(0x61afef), Rgb::from_hex(0xc678dd), Rgb::from_hex(0x56b6c2), Rgb::from_hex(0xffffff),
            ],
            Rgb::from_hex(0xabb2bf), Rgb::from_hex(0x282c34), Rgb::from_hex(0xabb2bf),
        ),
    }
}

pub fn solarized_dark() -> Theme {
    // Ethan Schoonover's classic — literally the test of time.
    Theme {
        name: "solarized-dark".into(),
        display_name: "Solarized Dark".into(),
        dark: true,
        chrome: ThemeChrome {
            canvas:       Rgb::from_hex(0x002b36),
            elevated:     Rgb::from_hex(0x073642),
            surface_elev: Rgb::from_hex(0x0a4452),
            ink:          Rgb::from_hex(0x839496),
            ink_muted:    Rgb::from_hex(0x586e75),
            divider:      Rgb::from_hex(0x094049),
            accent:       Rgb::from_hex(0x268bd2),
        },
        palette: build_palette(
            [
                Rgb::from_hex(0x073642), Rgb::from_hex(0xdc322f), Rgb::from_hex(0x859900), Rgb::from_hex(0xb58900),
                Rgb::from_hex(0x268bd2), Rgb::from_hex(0xd33682), Rgb::from_hex(0x2aa198), Rgb::from_hex(0xeee8d5),
                Rgb::from_hex(0x002b36), Rgb::from_hex(0xcb4b16), Rgb::from_hex(0x586e75), Rgb::from_hex(0x657b83),
                Rgb::from_hex(0x839496), Rgb::from_hex(0x6c71c4), Rgb::from_hex(0x93a1a1), Rgb::from_hex(0xfdf6e3),
            ],
            Rgb::from_hex(0x839496), Rgb::from_hex(0x002b36), Rgb::from_hex(0x93a1a1),
        ),
    }
}

pub fn tokyo_night() -> Theme {
    // Popular modern theme; rgb values from enkia/tokyo-night-vscode.
    Theme {
        name: "tokyo-night".into(),
        display_name: "Tokyo Night".into(),
        dark: true,
        chrome: ThemeChrome {
            canvas:       Rgb::from_hex(0x1a1b26),
            elevated:     Rgb::from_hex(0x16161e),
            surface_elev: Rgb::from_hex(0x24283b),
            ink:          Rgb::from_hex(0xc0caf5),
            ink_muted:    Rgb::from_hex(0x565f89),
            divider:      Rgb::from_hex(0x2a2e3f),
            accent:       Rgb::from_hex(0x7aa2f7),
        },
        palette: build_palette(
            [
                Rgb::from_hex(0x15161e), Rgb::from_hex(0xf7768e), Rgb::from_hex(0x9ece6a), Rgb::from_hex(0xe0af68),
                Rgb::from_hex(0x7aa2f7), Rgb::from_hex(0xbb9af7), Rgb::from_hex(0x7dcfff), Rgb::from_hex(0xa9b1d6),
                Rgb::from_hex(0x414868), Rgb::from_hex(0xf7768e), Rgb::from_hex(0x9ece6a), Rgb::from_hex(0xe0af68),
                Rgb::from_hex(0x7aa2f7), Rgb::from_hex(0xbb9af7), Rgb::from_hex(0x7dcfff), Rgb::from_hex(0xc0caf5),
            ],
            Rgb::from_hex(0xc0caf5), Rgb::from_hex(0x1a1b26), Rgb::from_hex(0xc0caf5),
        ),
    }
}

// ─── Palette construction helper ────────────────────────────────────

fn build_palette(
    ansi: [Rgb; 16],
    foreground: Rgb,
    background: Rgb,
    cursor: Rgb,
) -> ThemePalette {
    ThemePalette {
        ansi,
        extended: build_extended(&ansi),
        foreground,
        background,
        cursor,
    }
}

/// Standard 256-colour ramp:
/// * 0..16 — copy of the theme's ANSI palette.
/// * 16..232 — 6×6×6 RGB cube (`r g b ∈ {0, 95, 135, 175, 215, 255}`).
/// * 232..256 — 24-step grayscale (`8, 18, 28, …, 238`).
///
/// Themes only specify the 16 ANSI colours and we synthesise the rest.
/// Apps that override individual cube cells via OSC 4 still take
/// precedence; this is the *default* table.
fn build_extended(ansi: &[Rgb; 16]) -> Vec<Rgb> {
    let mut out = Vec::with_capacity(256);
    out.extend_from_slice(ansi);
    let levels = [0u8, 95, 135, 175, 215, 255];
    for &r in &levels {
        for &g in &levels {
            for &b in &levels {
                out.push(Rgb { r, g, b });
            }
        }
    }
    for i in 0..24u8 {
        let v = 8 + i * 10;
        out.push(Rgb { r: v, g: v, b: v });
    }
    debug_assert_eq!(out.len(), 256);
    out
}

#[cfg(test)]
mod tests {
    //! Lock the canonical chrome hex values shipped by the default
    //! theme. These come straight from
    //! `src/CodeScope.App/Styles/DesignTokens.xaml`:
    //!
    //! * `Fig.Color.Canvas`     = `#FF000000`
    //! * `Fig.Color.NearBlack`  = `#FF090909` (the `Surface.Panel` brush)
    //! * `Surface.Color.Elev`   = `#FF141414`
    //! * `Fig.Color.Ink`        = `#FFFFFFFF`
    //! * `Fig.Color.InkMuted`   = `#FFA6A6A6`
    //! * `Fig.Color.Divider`    = `#22FFFFFF` (alpha-blended white over
    //!    `Canvas` flattens to ≈ `#222222`)
    //! * `Framer.Color.Blue`    = `#FF0099FF`
    //!
    //! Any accidental drift in these values silently shifts the chrome
    //! away from the C# build — this test is the first thing that fails
    //! when someone edits the hex.
    use super::*;

    #[test]
    fn default_chrome_matches_design_tokens() {
        let t = codescope_default();
        assert_eq!(t.chrome.canvas,       Rgb::from_hex(0x000000));
        assert_eq!(t.chrome.elevated,     Rgb::from_hex(0x090909));
        assert_eq!(t.chrome.surface_elev, Rgb::from_hex(0x141414));
        assert_eq!(t.chrome.ink,          Rgb::from_hex(0xffffff));
        assert_eq!(t.chrome.ink_muted,    Rgb::from_hex(0xa6a6a6));
        assert_eq!(t.chrome.divider,      Rgb::from_hex(0x222222));
        assert_eq!(t.chrome.accent,       Rgb::from_hex(0x0099ff));
    }

    #[test]
    fn default_terminal_canvas_is_near_black() {
        // Lock the terminal background (`palette.background`) for the
        // default theme. `#0a0a0c` is intentionally darker than VS
        // Code Dark+ (`#1e1e1e`) and Alacritty default (`#1d1f21`),
        // closer to Windows Terminal (`#0c0c0c`) with a faint cool
        // tint. Any drift away from this value should be deliberate
        // and considered alongside the chrome canvas (#000000) — the
        // two reads together set the "depth" of the panel.
        let t = codescope_default();
        assert_eq!(t.palette.background, Rgb::from_hex(0x0a0a0c));
    }

    #[test]
    fn surface_elev_serde_default_applies_when_field_absent() {
        // External theme JSON predating `surface_elev` must still
        // deserialise cleanly. `#[serde(default)]` should fall back
        // to `default_surface_elev()` (= `#141414`) when the field is
        // missing rather than failing the parse. Encoded inline as
        // raw JSON so the test catches both the rename of the field
        // and the removal of the default attribute.
        let json = concat!(
            "{",
            "\"canvas\":\"#000000\",",
            "\"elevated\":\"#090909\",",
            "\"ink\":\"#ffffff\",",
            "\"ink_muted\":\"#a6a6a6\",",
            "\"divider\":\"#222222\",",
            "\"accent\":\"#0099ff\"",
            "}"
        );
        let chrome: ThemeChrome = serde_json::from_str(json)
            .expect("legacy chrome JSON without surface_elev should deserialise");
        assert_eq!(chrome.surface_elev, Rgb::from_hex(0x141414));
    }
}
