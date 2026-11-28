//! `settings.json` — user-editable app config.
//!
//! Lives at [`AppPaths::settings_file`]. JSON because it round-trips
//! cleanly with the C# build's existing config files (`projects.json`)
//! and because it's what the user already knows from VS Code.
//!
//! Conservative defaults: every field is optional in serde, so a
//! half-written or empty `settings.json` still loads. Unknown keys at
//! the root, on each board and under `notify` are kept through a save,
//! so an older build doesn't drop what a newer one wrote.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::agent_registry::AgentProfile;
use crate::board::BoardSettings;
use crate::board::attention::Attention;
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

    /// Boards the board panel reads and dispatch takes work from (ADR-0027).
    pub boards: Vec<BoardSettings>,

    /// How many of the user's dispatches run at once; more wait in their trigger column.
    #[serde(rename = "dispatchLimit")]
    pub dispatch_limit: usize,
    /// Which board events ring the bell and show the OS toast.
    pub notify: NotifySettings,
    /// Keys this build doesn't know, kept so a save doesn't drop what a newer build wrote.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
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
            boards: Vec::new(),
            dispatch_limit: crate::board::dispatch::DEFAULT_DISPATCH_LIMIT,
            notify: NotifySettings::default(),
            extra: serde_json::Map::new(),
        }
    }
}

/// Which board events notify the user (ADR-0027); each is on unless turned off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NotifySettings {
    pub findings: bool,
    pub needs_you: bool,
    pub may_be_waiting: bool,
    /// Keys this build doesn't know, kept so a save doesn't drop what a newer build wrote.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl Default for NotifySettings {
    fn default() -> Self {
        Self {
            findings: true,
            needs_you: true,
            may_be_waiting: true,
            extra: serde_json::Map::new(),
        }
    }
}

impl NotifySettings {
    /// Whether `attention` rings the bell and shows the toast.
    pub fn wants(&self, attention: Attention) -> bool {
        match attention {
            Attention::Findings => self.findings,
            Attention::NeedsYou => self.needs_you,
            Attention::MayBeWaiting => self.may_be_waiting,
        }
    }

    /// Turn notifying about `attention` on or off.
    pub fn set(&mut self, attention: Attention, on: bool) {
        match attention {
            Attention::Findings => self.findings = on,
            Attention::NeedsYou => self.needs_you = on,
            Attention::MayBeWaiting => self.may_be_waiting = on,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Cursor shape enumerated explicitly. Serialises as kebab-case
/// (`"beam"`, `"block"`, `"underline"`, `"hollow-block"`) — what
/// hand-edited `settings.json` uses. Deserialisation is tolerant: an
/// unknown / mis-cased / hand-edited value collapses to the
/// [`CursorShape::Beam`] default rather than erroring out, matching
/// the legacy permissive behaviour. The terminal crate's own
/// `alacritty_terminal::vte::ansi::CursorShape` is the renderer-side
/// type; this one is the persistence-side type, kept independent so
/// `settings.json` doesn't depend on alacritty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CursorShape {
    Block,
    #[default]
    Beam,
    Underline,
    HollowBlock,
}

impl<'de> Deserialize<'de> for CursorShape {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        // Tolerant parse: any unknown spelling becomes the default,
        // matching the legacy String-typed setting's behaviour.
        let s: String = String::deserialize(d).unwrap_or_default();
        Ok(match s.as_str() {
            "block" => CursorShape::Block,
            "underline" | "underscore" => CursorShape::Underline,
            "hollow-block" | "hollow_block" => CursorShape::HollowBlock,
            _ => CursorShape::Beam,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct CursorSettings {
    pub shape: CursorShape,
    /// Whether the default cursor blinks. TUIs override.
    pub blinking: bool,
}

impl Default for CursorSettings {
    fn default() -> Self {
        Self {
            // Beam + blink matches Windows Terminal's default and is
            // what users see when they spawn pwsh anywhere else.
            shape: CursorShape::Beam,
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

    /// Load the file, apply `change` and save it, so edits made on disk since the last load
    /// survive; a file that doesn't load is left alone and the error returned.
    pub fn update(paths: &AppPaths, change: impl FnOnce(&mut Settings)) -> Result<Settings> {
        paths.ensure_dirs()?;
        Self::update_at(&paths.settings_file(), change)
    }

    /// [`Settings::update`] on an explicit path.
    pub fn update_at(path: &Path, change: impl FnOnce(&mut Settings)) -> Result<Settings> {
        let mut settings = Self::load_from(path)?;
        change(&mut settings);
        settings.save_to(path)?;
        Ok(settings)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut json = serde_json::to_string_pretty(self)
            .context("serialise settings")?;
        json.push('\n');
        crate::atomic_write::write_atomic(path, json.as_bytes())
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
        let settings = Settings {
            theme: "tokyo-night".into(),
            scrollback: 50_000,
            ..Default::default()
        };
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
        let settings = Settings { default_agent: "codex".into(), ..Default::default() };
        settings.save_to(&path).unwrap();

        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.default_agent, "codex");
    }

    #[test]
    fn default_agent_serialises_as_camel_case() {
        let settings = Settings { default_agent: "codex".into(), ..Default::default() };
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
    fn unknown_keys_survive_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"theme":"tokyo-night","later":{"a":1}}"#).unwrap();
        Settings::load_from(&path).unwrap().save_to(&path).unwrap();
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(saved.contains(r#""later""#), "{saved}");
    }

    #[test]
    fn an_unknown_board_role_does_not_fail_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(
            &path,
            r#"{"theme":"tokyo-night","boards":[{"url":"u","columns":{"inreview":"Review"}}]}"#,
        )
        .unwrap();
        let settings = Settings::load_from(&path).unwrap();
        assert_eq!(settings.theme, "tokyo-night");
        assert_eq!(settings.boards.len(), 1);
    }

    #[test]
    fn update_keeps_what_changed_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"theme":"tokyo-night"}"#).unwrap();
        let updated = Settings::update_at(&path, |s| s.scrollback = 5).unwrap();
        assert_eq!(updated.theme, "tokyo-night");
        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.theme, "tokyo-night");
        assert_eq!(loaded.scrollback, 5);
    }

    #[test]
    fn update_leaves_a_file_it_cannot_read_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ invalid").unwrap();
        assert!(Settings::update_at(&path, |s| s.scrollback = 5).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{ invalid");
    }

    #[test]
    fn malformed_json_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ invalid").unwrap();
        assert!(Settings::load_from(&path).is_err());
    }

    /// Sanity check that the full surface area the Settings dialog
    /// can edit (theme, font family + size + line-height, scrollback,
    /// cursor shape + blink, default agent) all round-trips through
    /// serde. Catches accidental schema breaks if any of the fields
    /// ever swaps serde shape under the dialog.
    #[test]
    fn full_dialog_surface_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let settings = Settings {
            theme: "tokyo-night".into(),
            font: FontSettings {
                family: "JetBrains Mono".into(),
                size: 15.0,
                line_height_multiplier: 1.2,
                ..Default::default()
            },
            scrollback: 25_000,
            cursor: CursorSettings { shape: CursorShape::Underline, blinking: false },
            default_agent: "codex".into(),
            ..Default::default()
        };
        settings.save_to(&path).unwrap();

        let loaded = Settings::load_from(&path).unwrap();
        assert_eq!(loaded.theme, "tokyo-night");
        assert_eq!(loaded.font.family, "JetBrains Mono");
        assert_eq!(loaded.font.size, 15.0);
        assert_eq!(loaded.font.line_height_multiplier, 1.2);
        assert_eq!(loaded.scrollback, 25_000);
        assert_eq!(loaded.cursor.shape, CursorShape::Underline);
        assert!(!loaded.cursor.blinking);
        assert_eq!(loaded.default_agent, "codex");
    }

    #[test]
    fn every_board_event_notifies_unless_turned_off() {
        use crate::board::attention::Attention;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{}").unwrap();
        let notify = Settings::load_from(&path).unwrap().notify;
        assert!(Attention::ALL.into_iter().all(|a| notify.wants(a)));

        std::fs::write(&path, r#"{"notify": {"mayBeWaiting": false, "later": 1}}"#).unwrap();
        let mut settings = Settings::load_from(&path).unwrap();
        assert!(settings.notify.wants(Attention::Findings));
        assert!(settings.notify.wants(Attention::NeedsYou));
        assert!(!settings.notify.wants(Attention::MayBeWaiting));

        settings.notify.set(Attention::Findings, false);
        settings.save_to(&path).unwrap();
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved["notify"],
            serde_json::json!({
                "findings": false, "needsYou": true, "mayBeWaiting": false, "later": 1
            })
        );
    }
}
