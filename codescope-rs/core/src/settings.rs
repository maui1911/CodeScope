//! `settings.json` — user-editable app config.
//!
//! Lives at [`AppPaths::settings_file`]. JSON because it round-trips
//! cleanly with the C# build's existing config files (`projects.json`)
//! and because it's what the user already knows from VS Code.
//!
//! Conservative defaults: every field is optional in serde, so a
//! half-written or empty `settings.json` still loads. Unknown keys
//! at the *root* are silently dropped on save — we don't currently
//! preserve them via `#[serde(flatten)]`, so a write-back cycle will
//! lose forward-compat fields. Re-add when the on-disk schema starts
//! seeing real version skew.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::agent_registry::AgentProfile;
use crate::paths::AppPaths;
use crate::theme::builtin::DEFAULT_NAME;

/// Default stable id for the user's preferred agent when no override
/// is set in `settings.json`. Matches the C# build's `IsDefault` flag
/// on Claude Code so cold-start picks the same agent in both ports.
pub const DEFAULT_AGENT_ID: &str = "claude";

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

    /// Stable id of the user's preferred agent — Phase 1 wires this
    /// at startup so the future new-session menu can default to
    /// whichever CLI the user actually uses. Defaults to `"claude"`
    /// to match the C# build's `AgentRegistry.BuildDefaults` flag.
    /// Lookup against `AgentRegistry` is case-insensitive, so a
    /// hand-edited `"Codex"` resolves to the `codex` profile.
    ///
    /// Serialised as `defaultAgent` to match the camelCase shape
    /// used by every other on-disk config in the app
    /// (`ProjectsConfig`, `AgentProfile`). The `default_agent` alias
    /// keeps any settings.json hand-rolled against an early build of
    /// this PR — when the field briefly serialised as snake_case —
    /// loading without surprise.
    #[serde(rename = "defaultAgent", alias = "default_agent")]
    pub default_agent: String,

    /// Optional user-defined agent overrides. When non-empty, the
    /// registry uses these instead of the built-in defaults — mirrors
    /// `ProjectsConfig.Agents` in the C# build, which lets power
    /// users hand-edit the on-disk config to add/replace agents
    /// without a code change. Empty (the default) keeps the shipped
    /// 5-agent defaults.
    pub agents: Vec<AgentProfile>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: DEFAULT_NAME.to_string(),
            font: FontSettings::default(),
            scrollback: 10_000,
            cursor: CursorSettings::default(),
            default_agent: DEFAULT_AGENT_ID.to_string(),
            agents: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FontSettings {
    /// Primary font family. Empty string falls back to the binary's
    /// built-in default chain (currently a Nerd-Font-first list, see
    /// `app::build_font_config`) — *not* an OS-resolved system
    /// monospace. True platform-default picking would need a per-OS
    /// font resolver and isn't wired yet.
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
    fn default_agent_defaults_to_claude() {
        let settings = Settings::default();
        assert_eq!(settings.default_agent, DEFAULT_AGENT_ID);
        assert_eq!(settings.default_agent, "claude");
        assert!(settings.agents.is_empty());
    }

    #[test]
    fn missing_default_agent_falls_back_to_built_in() {
        // Older settings.json files won't carry `defaultAgent`. The
        // `#[serde(default)]` on `Settings` must keep them loading
        // cleanly with the built-in default.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"theme":"tokyo-night"}"#).unwrap();
        let settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.default_agent, DEFAULT_AGENT_ID);
        assert!(settings.agents.is_empty());
    }

    #[test]
    fn default_agent_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let mut settings = Settings::default();
        settings.default_agent = "codex".into();
        settings.save_to(&path).unwrap();

        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.default_agent, "codex");
    }

    #[test]
    fn default_agent_serialises_as_camel_case() {
        let mut settings = Settings::default();
        settings.default_agent = "codex".into();
        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            json.contains("\"defaultAgent\""),
            "settings.json must use camelCase for defaultAgent: {json}"
        );
        assert!(
            !json.contains("\"default_agent\""),
            "settings.json must not emit snake_case key: {json}"
        );
    }

    #[test]
    fn default_agent_snake_case_alias_still_loads() {
        // Early builds of the agent-registry PR wrote `default_agent`
        // (snake_case). The alias keeps those files round-tripping
        // cleanly — they reload as if the key had been camelCase.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"default_agent":"codex"}"#).unwrap();
        let settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.default_agent, "codex");
    }

    #[test]
    fn malformed_json_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ invalid").unwrap();
        assert!(Settings::load_from(&path).is_err());
    }
}
