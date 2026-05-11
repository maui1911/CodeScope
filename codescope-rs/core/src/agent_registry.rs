//! Agent profile registry — Rust port of C# `AgentRegistry` + `AgentProfile`.
//!
//! Mirrors `src/CodeScope.Core/Services/AgentRegistry.cs` and
//! `src/CodeScope.Core/Models/AgentProfile.cs`. The C# build exposes
//! these via DI so views/view-models can list available agents, pick
//! the user-flagged default for "new tab", and look up by id when
//! restoring sessions. This port keeps the same shape so the eventual
//! Rust sidebar / new-session menu can consume it 1:1.
//!
//! The registry is read-only after construction. Built-in defaults
//! cover the five agents shipped in the C# build:
//! `claude`, `codex`, `opencode`, `copilot`, `pi`.
//!
//! Built defaults intentionally match the C# arrays — argv, session-id
//! flags, resume-by-id args, icons, and context-window tokens — byte
//! for byte so on-disk session records stay round-trippable between
//! the C# build and the Rust port.

use serde::{Deserialize, Serialize};

use crate::settings::Settings;

/// One coding-agent CLI profile.
///
/// Field order and naming mirrors the C# `AgentProfile` record. The
/// JSON shape on disk uses camelCase to match the C# build's
/// `JsonSerializerOptions` (and what `projects.json` already writes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentProfile {
    /// Stable id used in config and cross-references.
    pub id: String,

    /// Human-readable name (e.g. "Claude Code").
    pub display_name: String,

    /// Executable name resolved via PATH (e.g. "claude").
    pub command: String,

    /// Args passed when resuming an existing session (e.g. `["--continue"]`).
    #[serde(default)]
    pub resume_args: Vec<String>,

    /// Args passed when starting a brand-new session.
    #[serde(default)]
    pub new_session_args: Vec<String>,

    /// Optional CLI flag that accepts a caller-supplied session id on
    /// launch (e.g. `--session-id` for Claude Code). When `Some`,
    /// CodeScope generates a UUID per new session, passes it to the
    /// CLI with this flag, and persists it so later resumes can target
    /// that specific conversation. `None` for CLIs that don't support
    /// deterministic session ids.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id_flag: Option<String>,

    /// Args used to resume a specific session id (e.g. `["--resume"]`
    /// for Claude Code — the stored UUID is appended as the next
    /// token). Empty falls back to `resume_args`.
    #[serde(default)]
    pub resume_by_id_args: Vec<String>,

    /// True for the default agent picked when creating a tab without
    /// explicit selection.
    #[serde(default)]
    pub is_default: bool,

    /// Optional single-glyph icon (emoji or symbol) used by the UI to
    /// decorate sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,

    /// Nominal context-window capacity (in tokens) for the model this
    /// agent runs, or `0` when unknown. Drives the status-bar token
    /// progress read-out. Claude Code defaults to Opus 4.7 with the
    /// 1M-context variant in this setup, so the baked default there is
    /// 1_000_000. Users can override via settings.
    #[serde(default)]
    pub context_window_tokens: u32,
}

/// In-memory registry backed by a list passed at construction time.
///
/// Mirrors `AgentRegistry` from the C# build. The list is cheaply
/// cloneable but is normally held by `AppShell` once and consulted on
/// demand.
#[derive(Debug, Clone)]
pub struct AgentRegistry {
    agents: Vec<AgentProfile>,
}

impl AgentRegistry {
    /// Build a registry from an explicit profile list. An empty list
    /// is technically valid — the consumer just gets no agents — but
    /// most callers should prefer [`AgentRegistry::with_built_ins`] or
    /// [`AgentRegistry::from_settings`].
    pub fn new(agents: Vec<AgentProfile>) -> Self {
        Self { agents }
    }

    /// Construct the registry with the shipped built-in defaults
    /// (claude / codex / opencode / copilot / pi). Equivalent to the
    /// C# parameterless `new AgentRegistry()` constructor.
    pub fn with_built_ins() -> Self {
        Self::new(built_in_defaults())
    }

    /// Build a registry from the user's settings. If
    /// `settings.agents` is non-empty those overrides are used as-is;
    /// otherwise the built-in defaults are returned. Mirrors C#
    /// `AgentRegistry.FromConfig`.
    ///
    /// `settings.default_agent` is then reconciled onto the resulting
    /// list — the profile whose id matches (case-insensitive) is
    /// flagged `is_default = true` and every other profile is demoted
    /// to `false`. This keeps `get_default()` honest no matter which
    /// of the two settings surfaces the user touched. Empty or
    /// unknown ids leave the list untouched, so a typo in
    /// `settings.json` doesn't silently wipe the built-in default.
    pub fn from_settings(settings: &Settings) -> Self {
        let agents = if settings.agents.is_empty() {
            built_in_defaults()
        } else {
            settings.agents.clone()
        };
        let mut reg = Self::new(agents);
        reg.apply_default_agent(&settings.default_agent);
        reg
    }

    /// Apply a `settings.default_agent` id onto the in-memory list:
    /// the matching profile is flagged default and all others are
    /// demoted. No-op when the id is empty or not present in the
    /// registry — preserves whatever `is_default` flags were already
    /// baked in (built-in defaults flag Claude; custom overrides
    /// might flag something else).
    fn apply_default_agent(&mut self, default_agent_id: &str) {
        if default_agent_id.is_empty() {
            return;
        }
        let target_idx = self
            .agents
            .iter()
            .position(|a| a.id.eq_ignore_ascii_case(default_agent_id));
        let Some(idx) = target_idx else { return };
        for (i, agent) in self.agents.iter_mut().enumerate() {
            agent.is_default = i == idx;
        }
    }

    /// All registered profiles, in registration order.
    pub fn get_all(&self) -> &[AgentProfile] {
        &self.agents
    }

    /// The first profile flagged `is_default`. Falls back to the first
    /// overall profile when no explicit default is set — same fallback
    /// rule as the C# `GetDefault()`.
    pub fn get_default(&self) -> Option<&AgentProfile> {
        self.agents
            .iter()
            .find(|a| a.is_default)
            .or_else(|| self.agents.first())
    }

    /// Look up a profile by stable id. Case-insensitive to match the
    /// `StringComparison.OrdinalIgnoreCase` used in C#.
    pub fn get_by_id(&self, id: &str) -> Option<&AgentProfile> {
        self.agents
            .iter()
            .find(|a| a.id.eq_ignore_ascii_case(id))
    }
}

/// Built-in profiles shipped with the app. Mirrors the C#
/// `AgentRegistry.BuildDefaults()` list 1:1 — argv, flags, icons, and
/// context-window tokens all kept identical so the Rust port behaves
/// like the C# build for the same agent binaries.
pub fn built_in_defaults() -> Vec<AgentProfile> {
    vec![
        AgentProfile {
            id: "claude".into(),
            display_name: "Claude Code".into(),
            command: "claude".into(),
            // Fresh launches: bare `claude`; Claude Code mints its own UUID
            // and the discovery layer adopts it. When we already know the
            // session id (cross-group drag, layout hydrate) we resume via
            // `claude --resume <id>` so the conversation continues instead
            // of the user landing on a fresh agent.
            resume_args: vec![],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec!["--resume".into()],
            is_default: true,
            icon: Some("✶".into()),
            context_window_tokens: 1_000_000,
        },
        AgentProfile {
            id: "codex".into(),
            display_name: "Codex".into(),
            command: "codex".into(),
            resume_args: vec!["resume".into()],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec![],
            is_default: false,
            icon: Some("◇".into()),
            context_window_tokens: 0,
        },
        AgentProfile {
            id: "opencode".into(),
            display_name: "OpenCode".into(),
            // Windows ships the binary as `opencode-cli.exe` (npm
            // `opencode-ai` package installs that name on Win to dodge a
            // Windows reserved/conflicting name). macOS/Linux use
            // `opencode`; flip this default if we ever cross-compile.
            command: "opencode-cli".into(),
            // `opencode -c` (= `--continue`) resumes the most recent
            // session in the cwd. `opencode --session <id>` (= `-s <id>`)
            // resumes a specific id; we adopt that id from the
            // message-file watcher once OpenCode has written its first
            // assistant turn. SessionIdFlag stays None — OpenCode mints
            // its own ids and doesn't accept caller-minted.
            resume_args: vec!["--continue".into()],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec!["--session".into()],
            is_default: false,
            icon: Some("◈".into()),
            context_window_tokens: 0,
        },
        AgentProfile {
            id: "copilot".into(),
            display_name: "Copilot CLI".into(),
            command: "copilot".into(),
            // `copilot --continue` resumes the most recent session.
            // `copilot --resume=<id>` resumes a specific session by
            // UUID / name / id-prefix. The `=` suffix on the last
            // resume_by_id_args entry tells SessionManager to concat
            // the id directly (Copilot's optional-value flag requires
            // `=` syntax, not a space). SessionIdFlag stays None —
            // Copilot mints its own ids; callers don't supply them.
            resume_args: vec!["--continue".into()],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec!["--resume=".into()],
            is_default: false,
            icon: Some("⊛".into()),
            context_window_tokens: 0,
        },
        AgentProfile {
            id: "pi".into(),
            display_name: "Pi".into(),
            command: "pi".into(),
            // Fresh launches: bare `pi`. Pi mints its own UUID + writes
            // a session-jsonl whose header carries the cwd, which the
            // discovery layer matches to adopt the id. Resume-by-id:
            // `pi --session <uuid>` — Pi resolves the latest session
            // file with that UUID suffix anywhere under
            // `~/.pi/agent/sessions/`. Continue-most-recent: `pi -c`
            // (used as the bare resume_args fallback when we don't
            // have a stored id). SessionIdFlag stays None — Pi doesn't
            // accept caller-minted ids on launch.
            resume_args: vec!["-c".into()],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec!["--session".into()],
            is_default: false,
            icon: Some("π".into()),
            context_window_tokens: 0,
        },
    ]
}

// ─── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_defaults_cover_all_five_agents() {
        let defaults = built_in_defaults();
        let ids: Vec<&str> = defaults.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["claude", "codex", "opencode", "copilot", "pi"]);
    }

    #[test]
    fn get_default_returns_claude_for_built_ins() {
        let reg = AgentRegistry::with_built_ins();
        let def = reg.get_default().expect("default present");
        assert_eq!(def.id, "claude");
        assert!(def.is_default);
    }

    #[test]
    fn get_default_falls_back_to_first_when_no_is_default_flag() {
        let reg = AgentRegistry::new(vec![
            AgentProfile {
                id: "alpha".into(),
                display_name: "Alpha".into(),
                command: "alpha".into(),
                resume_args: vec![],
                new_session_args: vec![],
                session_id_flag: None,
                resume_by_id_args: vec![],
                is_default: false,
                icon: None,
                context_window_tokens: 0,
            },
            AgentProfile {
                id: "beta".into(),
                display_name: "Beta".into(),
                command: "beta".into(),
                resume_args: vec![],
                new_session_args: vec![],
                session_id_flag: None,
                resume_by_id_args: vec![],
                is_default: false,
                icon: None,
                context_window_tokens: 0,
            },
        ]);
        assert_eq!(reg.get_default().unwrap().id, "alpha");
    }

    #[test]
    fn get_default_none_when_empty() {
        let reg = AgentRegistry::new(vec![]);
        assert!(reg.get_default().is_none());
    }

    #[test]
    fn get_by_id_is_case_insensitive() {
        let reg = AgentRegistry::with_built_ins();
        assert_eq!(reg.get_by_id("claude").unwrap().id, "claude");
        assert_eq!(reg.get_by_id("CLAUDE").unwrap().id, "claude");
        assert_eq!(reg.get_by_id("Codex").unwrap().id, "codex");
        assert_eq!(reg.get_by_id("PI").unwrap().id, "pi");
        assert!(reg.get_by_id("gemini").is_none());
    }

    #[test]
    fn claude_default_has_1m_context_window() {
        let reg = AgentRegistry::with_built_ins();
        let claude = reg.get_by_id("claude").unwrap();
        assert_eq!(claude.context_window_tokens, 1_000_000);
        assert_eq!(claude.resume_by_id_args, vec!["--resume".to_string()]);
    }

    #[test]
    fn copilot_resume_by_id_uses_equals_suffix() {
        let reg = AgentRegistry::with_built_ins();
        let copilot = reg.get_by_id("copilot").unwrap();
        assert_eq!(copilot.resume_by_id_args, vec!["--resume=".to_string()]);
        assert_eq!(copilot.resume_args, vec!["--continue".to_string()]);
    }

    #[test]
    fn from_settings_returns_built_ins_when_empty() {
        let settings = Settings::default();
        let reg = AgentRegistry::from_settings(&settings);
        assert_eq!(reg.get_all().len(), built_in_defaults().len());
        assert_eq!(reg.get_default().unwrap().id, "claude");
    }

    #[test]
    fn from_settings_uses_overrides_when_provided() {
        let mut settings = Settings::default();
        settings.agents = vec![AgentProfile {
            id: "custom".into(),
            display_name: "Custom".into(),
            command: "custom-cli".into(),
            resume_args: vec![],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec![],
            is_default: true,
            icon: None,
            context_window_tokens: 0,
        }];
        let reg = AgentRegistry::from_settings(&settings);
        assert_eq!(reg.get_all().len(), 1);
        assert_eq!(reg.get_default().unwrap().id, "custom");
    }

    #[test]
    fn from_settings_honours_default_agent_setting() {
        // `settings.default_agent = "codex"` must flip `is_default`
        // on the codex profile and demote every other built-in
        // (Claude included) so `get_default()` returns Codex without
        // the caller having to hand-edit per-profile flags.
        let mut settings = Settings::default();
        settings.default_agent = "codex".into();
        let reg = AgentRegistry::from_settings(&settings);
        let def = reg.get_default().expect("default present");
        assert_eq!(def.id, "codex");
        assert!(def.is_default);
        // Claude must have been demoted.
        let claude = reg.get_by_id("claude").unwrap();
        assert!(!claude.is_default);
    }

    #[test]
    fn from_settings_default_agent_lookup_is_case_insensitive() {
        // Hand-edited `settings.json` with `"defaultAgent": "Codex"`
        // (mixed case) still resolves — matches the lookup behaviour
        // of `get_by_id`.
        let mut settings = Settings::default();
        settings.default_agent = "CODEX".into();
        let reg = AgentRegistry::from_settings(&settings);
        assert_eq!(reg.get_default().unwrap().id, "codex");
    }

    #[test]
    fn from_settings_unknown_default_agent_leaves_built_in_flag_intact() {
        // Typos must not silently wipe the baked-in default — the
        // user should still get Claude back when they fat-finger an
        // unknown id.
        let mut settings = Settings::default();
        settings.default_agent = "gemini".into();
        let reg = AgentRegistry::from_settings(&settings);
        assert_eq!(reg.get_default().unwrap().id, "claude");
    }

    #[test]
    fn agent_profile_serde_round_trip() {
        let profile = AgentProfile {
            id: "claude".into(),
            display_name: "Claude Code".into(),
            command: "claude".into(),
            resume_args: vec![],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec!["--resume".into()],
            is_default: true,
            icon: Some("✶".into()),
            context_window_tokens: 1_000_000,
        };
        let json = serde_json::to_string(&profile).unwrap();
        let parsed: AgentProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, profile);
    }

    #[test]
    fn agent_profile_serde_uses_camel_case() {
        let profile = AgentProfile {
            id: "claude".into(),
            display_name: "Claude Code".into(),
            command: "claude".into(),
            resume_args: vec![],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec!["--resume".into()],
            is_default: true,
            icon: Some("✶".into()),
            context_window_tokens: 1_000_000,
        };
        let json = serde_json::to_string(&profile).unwrap();
        assert!(json.contains("\"displayName\""));
        assert!(json.contains("\"resumeByIdArgs\""));
        assert!(json.contains("\"isDefault\""));
        assert!(json.contains("\"contextWindowTokens\""));
    }
}
