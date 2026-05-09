//! `settings.json` — user-editable app config.
//!
//! Lives at [`AppPaths::settings_file`]. JSON because it round-trips
//! cleanly with the C# build's existing config files (`projects.json`)
//! and because it's what the user already knows from VS Code.
//!
//! Conservative defaults: every field is optional in serde, so a
//! half-written or empty `settings.json` still loads. Unknown keys
//! survive a load/save round-trip too — nice for forward compat when
//! we add a field in vN+1 and the user's still on vN.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;
use crate::theme::builtin::DEFAULT_NAME;

/// Top-level config object. Every leaf is optional so missing pieces
/// fall back to defaults; this also means an empty `{}` is a valid
/// settings file.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Stable id of a built-in theme, or — later — a path to a custom
    /// theme file. Resolved against `theme::builtin::by_name`.
    pub theme: String,

    /// Font + size + line-height for the terminal grid.
    pub font: FontSettings,

    /// How many lines of scrollback the terminal keeps. Per-tab.
    pub scrollback: usize,

    /// Cursor look + blink. TUIs that emit DECSCUSR override this on
    /// the fly; the values here are the defaults that PSReadLine /
    /// cmd.exe / bash inherit before any TUI takes over.
    pub cursor: CursorSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: DEFAULT_NAME.to_string(),
            font: FontSettings::default(),
            scrollback: 10_000,
            cursor: CursorSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FontSettings {
    /// Primary family. Empty string = "let the platform pick".
    pub family: String,
    /// Glyph fallback chain. gpui falls back per-glyph, so installing
    /// any one of these is enough to pick up missing icons.
    pub fallbacks: Vec<String>,
    /// Em size in pixels.
    pub size: f32,
    /// Multiplier applied to the measured `(ascent + descent)`.
    /// 1.0 is "shaped tightly"; 1.1–1.2 gives a roomier line.
    pub line_height_multiplier: f32,
}

impl Default for FontSettings {
    fn default() -> Self {
        Self {
            family: "FiraCode Nerd Font".into(),
            fallbacks: vec![
                "FiraCode Nerd Font Mono".into(),
                "FiraCodeNerdFont".into(),
                "FiraCodeNerdFontMono".into(),
                "CaskaydiaCove Nerd Font".into(),
                "CaskaydiaCove Nerd Font Mono".into(),
                "MesloLGM Nerd Font".into(),
                "JetBrainsMono Nerd Font".into(),
                "Hack Nerd Font".into(),
                "Cascadia Mono".into(),
                "Consolas".into(),
            ],
            size: 13.0,
            line_height_multiplier: 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorSettings {
    /// `"block"`, `"beam"`, `"underline"`, or `"hollow-block"`.
    pub shape: String,
    /// Whether the default cursor blinks. TUIs override.
    pub blinking: bool,
}

impl Default for CursorSettings {
    fn default() -> Self {
        Self {
            // Beam + blink matches Windows Terminal's default and is
            // what users see when they spawn pwsh anywhere else.
            shape: "beam".into(),
            blinking: true,
        }
    }
}

impl Settings {
    /// Read [`AppPaths::settings_file`]. Missing file → defaults.
    /// Malformed JSON → propagated error so the user gets a clear
    /// message instead of silently losing their config.
    pub fn load(paths: &AppPaths) -> Result<Self> {
        Self::load_from(&paths.settings_file())
    }

    /// Variant that takes an explicit path — used by tests.
    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) if bytes.is_empty() => Ok(Self::default()),
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse {}", path.display())),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => {
                Err(err).with_context(|| format!("read {}", path.display()))
            }
        }
    }

    /// Write the current settings to disk (pretty-printed, two-space
    /// indent — matches VS Code's `settings.json` style and what a
    /// user expects to hand-edit).
    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        paths.ensure_dirs()?;
        self.save_to(&paths.settings_file())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut json = serde_json::to_string_pretty(self)
            .context("serialise settings")?;
        json.push('\n');
        std::fs::write(path, json)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");
        let settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.theme, DEFAULT_NAME);
    }

    #[test]
    fn empty_object_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        let settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.theme, DEFAULT_NAME);
        assert_eq!(settings.scrollback, 10_000);
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut settings = Settings::default();
        settings.theme = "tokyo-night".into();
        settings.scrollback = 50_000;
        settings.save_to(&path).unwrap();

        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.theme, "tokyo-night");
        assert_eq!(loaded.scrollback, 50_000);
    }

    #[test]
    fn malformed_json_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ invalid").unwrap();
        assert!(Settings::load_from(&path).is_err());
    }
}
