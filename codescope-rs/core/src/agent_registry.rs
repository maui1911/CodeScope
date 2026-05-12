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

/// Build the auto-type command string used to (re)attach a terminal
/// session to a previously-running agent conversation. Mirrors C#
/// `SessionManager.CreateAgentSession` resume branch + `JoinResumeByIdArgs`
/// (see `src/CodeScope.Core/Services/SessionManager.cs`).
///
/// Resolution table:
///
/// 1. `agent_session_id = Some(id)` AND `resume_by_id_args` non-empty
///    → `[command, resume_by_id_args[0..n-1]..,
///        resume_by_id_args[last] + id_suffix]`
///    where the last token is concat'd with the id without a space
///    when it ends with `=` (Copilot's `--resume=<id>` shape) and
///    appended as a separate argv element otherwise (Claude's
///    `--resume <id>`, OpenCode's `--session <id>`, Pi's
///    `--session <id>`).
/// 2. Otherwise (no known id, or the profile has no resume-by-id
///    flow) → `[command, resume_args...]`. Empty `resume_args` yields
///    just the bare `command` — matches C# `CreateAgentSession` with
///    `resume = true` and an empty `ResumeArgs`.
///
/// Tokens are joined with single spaces. The result is intended for
/// `auto_type` in a PowerShell prompt, exactly like the C# build.
pub fn build_resume_auto_type(
    profile: &AgentProfile,
    agent_session_id: Option<&str>,
) -> Option<String> {
    if profile.command.is_empty() {
        return None;
    }

    let id_for_resume = agent_session_id
        .map(str::trim)
        .filter(|s| !s.is_empty());

    if let Some(id) = id_for_resume {
        if !profile.resume_by_id_args.is_empty() {
            let mut argv: Vec<String> =
                Vec::with_capacity(1 + profile.resume_by_id_args.len());
            argv.push(profile.command.clone());
            let last_idx = profile.resume_by_id_args.len() - 1;
            for (i, token) in profile.resume_by_id_args.iter().enumerate() {
                if i == last_idx && token.ends_with('=') {
                    // Copilot's optional-value flag — `--resume=<id>`
                    // requires no space between flag and value.
                    argv.push(format!("{token}{id}"));
                } else if i == last_idx {
                    argv.push(token.clone());
                    argv.push(id.to_string());
                } else {
                    argv.push(token.clone());
                }
            }
            return Some(argv.join(" "));
        }
        // Fall through to resume_args — the profile has no per-id
        // resume verb (Codex), so the best we can do is "continue
        // most recent" or just re-launch.
    }

    let mut argv: Vec<String> = Vec::with_capacity(1 + profile.resume_args.len());
    argv.push(profile.command.clone());
    argv.extend(profile.resume_args.iter().cloned());
    Some(argv.join(" "))
}

/// Build the auto-type command string used to launch a brand-new
/// session for `profile`. Mirrors C#
/// `SessionManager.CreateAgentSession` with `resume = false`:
/// `[command, new_session_args...]` joined by single spaces.
///
/// Returns `None` when `profile.command` is empty — defensive against
/// a hand-edited `settings.json` so callers can fall back to a plain
/// shell instead of emitting a leading-space argv.
///
/// Shared helper called from every new-session entry point (the
/// sidebar's double-click handler, `AppShell::default_agent_auto_type`,
/// the worktree menu's `New session ▸` parent row in
/// `Sidebar::build_new_session_parent_row`, and the per-agent rows in
/// `Sidebar::render_new_session_submenu`) so they can't drift out of
/// sync.
pub fn build_new_session_auto_type(profile: &AgentProfile) -> Option<String> {
    if profile.command.is_empty() {
        return None;
    }
    let mut argv: Vec<String> = Vec::with_capacity(1 + profile.new_session_args.len());
    argv.push(profile.command.clone());
    argv.extend(profile.new_session_args.iter().cloned());
    Some(argv.join(" "))
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

    // ─── build_resume_auto_type ────────────────────────────────────
    //
    // Covers the resume-by-explicit-id flow for every built-in
    // agent. Each test pairs a "with id" assertion against the
    // "without id" fallback so a regression in either branch is
    // caught. Mirrors the C# `SessionManager.CreateAgentSession`
    // resume branch + `JoinResumeByIdArgs`.

    fn registry_profile(id: &str) -> AgentProfile {
        AgentRegistry::with_built_ins()
            .get_by_id(id)
            .cloned()
            .expect("built-in profile present")
    }

    #[test]
    fn build_resume_auto_type_claude_with_id() {
        let profile = registry_profile("claude");
        assert_eq!(
            build_resume_auto_type(&profile, Some("abc-123")),
            Some("claude --resume abc-123".into()),
        );
    }

    #[test]
    fn build_resume_auto_type_claude_without_id_yields_bare_command() {
        // Claude's `resume_args` is empty — the bare `claude` is the
        // fallback when no specific session id is known.
        let profile = registry_profile("claude");
        assert_eq!(
            build_resume_auto_type(&profile, None),
            Some("claude".into()),
        );
    }

    #[test]
    fn build_resume_auto_type_codex_ignores_id_when_resume_by_id_args_empty() {
        // Codex has `resume_args = ["resume"]` but no
        // `resume_by_id_args` — passing an id must not change the
        // shape, the helper falls through to `resume_args`.
        let profile = registry_profile("codex");
        assert_eq!(
            build_resume_auto_type(&profile, Some("ignored-id")),
            Some("codex resume".into()),
        );
        assert_eq!(
            build_resume_auto_type(&profile, None),
            Some("codex resume".into()),
        );
    }

    #[test]
    fn build_resume_auto_type_copilot_with_id_concats_with_equals() {
        // Copilot's last `resume_by_id_args` entry is `"--resume="`
        // (trailing `=`). The helper must concat the id without
        // emitting an intermediate space.
        let profile = registry_profile("copilot");
        assert_eq!(
            build_resume_auto_type(&profile, Some("abc-123")),
            Some("copilot --resume=abc-123".into()),
        );
    }

    #[test]
    fn build_resume_auto_type_copilot_without_id_uses_continue() {
        let profile = registry_profile("copilot");
        assert_eq!(
            build_resume_auto_type(&profile, None),
            Some("copilot --continue".into()),
        );
    }

    #[test]
    fn build_resume_auto_type_opencode_with_id() {
        // OpenCode resumes by `--session <id>` (space-separated, no
        // trailing `=` on the flag). The built-in command is
        // `opencode-cli` on Windows.
        let profile = registry_profile("opencode");
        let cmd = profile.command.clone();
        assert_eq!(
            build_resume_auto_type(&profile, Some("abc-123")),
            Some(format!("{cmd} --session abc-123")),
        );
    }

    #[test]
    fn build_resume_auto_type_opencode_without_id_uses_continue() {
        let profile = registry_profile("opencode");
        let cmd = profile.command.clone();
        assert_eq!(
            build_resume_auto_type(&profile, None),
            Some(format!("{cmd} --continue")),
        );
    }

    #[test]
    fn build_resume_auto_type_pi_with_id_uses_session_flag() {
        // The user-visible bug: `pi -c` was resuming "most recent"
        // instead of `pi --session <id>` — this test pins the
        // correct shape.
        let profile = registry_profile("pi");
        assert_eq!(
            build_resume_auto_type(&profile, Some("abc-123")),
            Some("pi --session abc-123".into()),
        );
    }

    #[test]
    fn build_resume_auto_type_pi_without_id_falls_back_to_continue() {
        let profile = registry_profile("pi");
        assert_eq!(
            build_resume_auto_type(&profile, None),
            Some("pi -c".into()),
        );
    }

    #[test]
    fn build_resume_auto_type_empty_id_treated_as_no_id() {
        // Defensive: a persisted `agent_session_id = Some("")` (or
        // whitespace) must not produce `claude --resume ` with a
        // trailing space — fall back to the no-id path instead.
        let profile = registry_profile("claude");
        assert_eq!(
            build_resume_auto_type(&profile, Some("")),
            Some("claude".into()),
        );
        assert_eq!(
            build_resume_auto_type(&profile, Some("   ")),
            Some("claude".into()),
        );
    }

    #[test]
    fn build_resume_auto_type_returns_none_when_command_empty() {
        // Defensive: a hand-edited `settings.json` with a blank
        // `command` (or a registry override that forgot to set
        // one) must not emit a leading-space argv. Returning
        // `None` lets the caller fall through to a plain shell.
        let profile = AgentProfile {
            id: "broken".into(),
            display_name: "Broken".into(),
            command: String::new(),
            resume_args: vec!["--continue".into()],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec!["--resume".into()],
            is_default: false,
            icon: None,
            context_window_tokens: 0,
        };
        assert!(build_resume_auto_type(&profile, Some("abc")).is_none());
        assert!(build_resume_auto_type(&profile, None).is_none());
    }

    #[test]
    fn build_resume_auto_type_multi_token_resume_by_id_args_appends_id_to_last() {
        // Future-proofing for a hypothetical profile whose
        // `resume_by_id_args` is e.g. `["--resume", "--id"]` —
        // tokens are emitted in order and the id rides the last
        // one (still space-separated because the last token
        // doesn't end with `=`).
        let profile = AgentProfile {
            id: "multi".into(),
            display_name: "Multi".into(),
            command: "multi".into(),
            resume_args: vec!["--continue".into()],
            new_session_args: vec![],
            session_id_flag: None,
            resume_by_id_args: vec!["--resume".into(), "--id".into()],
            is_default: false,
            icon: None,
            context_window_tokens: 0,
        };
        assert_eq!(
            build_resume_auto_type(&profile, Some("xyz")),
            Some("multi --resume --id xyz".into()),
        );
    }

    // ─── build_new_session_auto_type ───────────────────────────────
    //
    // Pins the contract every new-session entry point (sidebar
    // double-click, Ctrl+Shift+T, per-agent submenu rows) relies on:
    // `<command> [<new_session_args>...]` joined by single spaces,
    // with `None` only when `command` is empty. All five built-in
    // profiles ship empty `new_session_args` today, so the bare-
    // command shape covers each one — the custom-profile test below
    // pins the multi-arg joining shape for user-defined profiles.

    #[test]
    fn build_new_session_auto_type_claude_yields_bare_command() {
        let profile = registry_profile("claude");
        assert_eq!(
            build_new_session_auto_type(&profile),
            Some("claude".into()),
        );
    }

    #[test]
    fn build_new_session_auto_type_codex_yields_bare_command() {
        // Codex's `new_session_args` is empty — the bare `codex` is
        // the expected new-session launch (`resume_args = ["resume"]`
        // is the resume shape, not the new-session shape).
        let profile = registry_profile("codex");
        assert_eq!(
            build_new_session_auto_type(&profile),
            Some("codex".into()),
        );
    }

    #[test]
    fn build_new_session_auto_type_copilot_yields_bare_command() {
        let profile = registry_profile("copilot");
        assert_eq!(
            build_new_session_auto_type(&profile),
            Some("copilot".into()),
        );
    }

    #[test]
    fn build_new_session_auto_type_joins_custom_new_session_args() {
        // A user-defined profile with new-session args must serialise
        // as `<command> <arg1> <arg2>...` so the terminal gets a
        // single ready-to-run line.
        let profile = AgentProfile {
            id: "custom".into(),
            display_name: "Custom".into(),
            command: "my-cli".into(),
            resume_args: vec![],
            new_session_args: vec!["--init".into(), "fresh".into()],
            session_id_flag: None,
            resume_by_id_args: vec![],
            is_default: true,
            icon: None,
            context_window_tokens: 0,
        };
        assert_eq!(
            build_new_session_auto_type(&profile),
            Some("my-cli --init fresh".into()),
        );
    }

    #[test]
    fn build_new_session_auto_type_returns_none_when_command_empty() {
        // Defensive: a hand-edited `settings.json` with a blank
        // `command` must not emit a leading-space argv. Returning
        // `None` lets the caller fall through to a plain shell.
        let profile = AgentProfile {
            id: "broken".into(),
            display_name: "Broken".into(),
            command: String::new(),
            resume_args: vec![],
            new_session_args: vec!["--init".into()],
            session_id_flag: None,
            resume_by_id_args: vec![],
            is_default: false,
            icon: None,
            context_window_tokens: 0,
        };
        assert!(build_new_session_auto_type(&profile).is_none());
    }
}
