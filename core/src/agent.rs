//! Agent identity for the Rust port.
//!
//! Mirrors the `agentId` strings the C# build threads through
//! `MainViewModel.RegisterAgentTelemetry` / `BeginAgentAdoption`:
//! `"claude"`, `"codex"`, `"copilot"`, `"opencode"`, `"pi"`. The Rust
//! port doesn't yet have a settings-driven agent picker (the sidebar
//! today only offers a "New Claude session" menu row), but the
//! dispatch surface needs an enum so the telemetry / discovery layer
//! can fan out correctly when other agents land. The [`crate::agent_registry`]
//! module owns the richer `AgentProfile` shape (argv, icons,
//! context-window tokens); this enum is the narrow id used by the
//! per-agent telemetry dispatch.
//!
//! Detection is currently based on the auto-typed launch command
//! recorded on each [`crate::projects::Session`]'s tab — the same
//! signal `crate::agent::agent_id_from_auto_type` already uses.
//! Anything else (plain `pwsh`, no auto-type, an unknown agent) maps
//! to `None`, leaving the tab telemetry-less just like a shell tab.

/// One of the five supported agent backends. Names match the
/// `agentId` strings the C# `MainViewModel` branches on; keep them
/// stable so on-disk session records (which carry `AgentId` as a
/// plain string) round-trip cleanly between builds. Codex was added
/// alongside the [`crate::agent_registry::AgentRegistry`] port — the
/// telemetry/discovery layer for it is a follow-up, but the id needs
/// to round-trip now so registry consumers can use it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AgentId {
    Claude,
    Codex,
    Copilot,
    OpenCode,
    Pi,
}

impl AgentId {
    /// Stable string id — matches the C# `agentId` literals used by
    /// `RegisterAgentTelemetry` / `BeginAgentAdoption`.
    pub const fn as_str(self) -> &'static str {
        match self {
            AgentId::Claude => "claude",
            AgentId::Codex => "codex",
            AgentId::Copilot => "copilot",
            AgentId::OpenCode => "opencode",
            AgentId::Pi => "pi",
        }
    }

    /// Parse a stable string id back into the enum (case-insensitive).
    /// Mirrors `string.Equals(agentId, "...", OrdinalIgnoreCase)` in
    /// the C# dispatch.
    ///
    /// A handful of *executable-name aliases* are accepted alongside
    /// the canonical ids so first-token parsing of an `auto_type`
    /// command (`agent_id_from_auto_type`) still classifies sessions
    /// correctly when the registry's `command` differs from the id —
    /// Windows ships OpenCode as `opencode-cli.exe`, for instance,
    /// because `opencode` clashes with a reserved name. The alias list
    /// is intentionally small and explicit; unknown binaries still
    /// resolve to `None`.
    pub fn from_str(s: &str) -> Option<Self> {
        if s.eq_ignore_ascii_case("claude") {
            Some(AgentId::Claude)
        } else if s.eq_ignore_ascii_case("codex") {
            Some(AgentId::Codex)
        } else if s.eq_ignore_ascii_case("copilot") {
            Some(AgentId::Copilot)
        } else if s.eq_ignore_ascii_case("opencode") || s.eq_ignore_ascii_case("opencode-cli") {
            // `opencode-cli` is the Windows npm-package binary name —
            // the built-in profile uses it as `command`, and
            // `agent_id_from_auto_type` would otherwise classify
            // OpenCode sessions as `None`.
            Some(AgentId::OpenCode)
        } else if s.eq_ignore_ascii_case("pi") {
            Some(AgentId::Pi)
        } else {
            None
        }
    }
}

/// Detect the agent id from the auto-typed launch command recorded on
/// a tab. Returns `None` for plain shell tabs, missing commands, or
/// unrecognised binaries — same gate `is_claude_auto_type` was using
/// before this PR, just generalised across the four agents we know.
///
/// Matching is case-insensitive on the first whitespace-delimited
/// token, so `claude --resume <id>` and `pi --new` both resolve. The
/// goal is parity with `MainViewModel.ResolveAgentIdForNewSession`'s
/// final string id once the new-session menu grows beyond Claude.
pub fn agent_id_from_auto_type(auto_type: Option<&str>) -> Option<AgentId> {
    let s = auto_type?.trim_start();
    let first = s.split(|c: char| c.is_whitespace()).next().unwrap_or("");
    if first.is_empty() {
        return None;
    }
    AgentId::from_str(first)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_str_round_trip() {
        for id in [
            AgentId::Claude,
            AgentId::Codex,
            AgentId::Copilot,
            AgentId::OpenCode,
            AgentId::Pi,
        ] {
            assert_eq!(AgentId::from_str(id.as_str()), Some(id));
        }
    }

    #[test]
    fn from_str_is_case_insensitive() {
        assert_eq!(AgentId::from_str("CLAUDE"), Some(AgentId::Claude));
        assert_eq!(AgentId::from_str("Codex"), Some(AgentId::Codex));
        assert_eq!(AgentId::from_str("Copilot"), Some(AgentId::Copilot));
        assert_eq!(AgentId::from_str("OPENCODE"), Some(AgentId::OpenCode));
        assert_eq!(AgentId::from_str("Pi"), Some(AgentId::Pi));
    }

    #[test]
    fn from_str_rejects_unknown() {
        assert_eq!(AgentId::from_str(""), None);
        assert_eq!(AgentId::from_str("gemini"), None);
        assert_eq!(AgentId::from_str("pwsh"), None);
        assert_eq!(AgentId::from_str("claudeflare"), None);
    }

    #[test]
    fn auto_type_none_or_blank_is_no_agent() {
        assert_eq!(agent_id_from_auto_type(None), None);
        assert_eq!(agent_id_from_auto_type(Some("")), None);
        assert_eq!(agent_id_from_auto_type(Some("   ")), None);
        assert_eq!(agent_id_from_auto_type(Some("\t")), None);
    }

    #[test]
    fn auto_type_resolves_each_agent() {
        assert_eq!(agent_id_from_auto_type(Some("claude")), Some(AgentId::Claude));
        assert_eq!(agent_id_from_auto_type(Some("codex")), Some(AgentId::Codex));
        assert_eq!(agent_id_from_auto_type(Some("copilot")), Some(AgentId::Copilot));
        assert_eq!(agent_id_from_auto_type(Some("opencode")), Some(AgentId::OpenCode));
        assert_eq!(agent_id_from_auto_type(Some("pi")), Some(AgentId::Pi));
    }

    #[test]
    fn auto_type_with_args_resolves_first_token() {
        assert_eq!(
            agent_id_from_auto_type(Some("claude --resume abc-123")),
            Some(AgentId::Claude),
        );
        assert_eq!(
            agent_id_from_auto_type(Some("pi --new")),
            Some(AgentId::Pi),
        );
        assert_eq!(
            agent_id_from_auto_type(Some("\tcopilot --workspace .")),
            Some(AgentId::Copilot),
        );
    }

    #[test]
    fn auto_type_case_insensitive() {
        assert_eq!(agent_id_from_auto_type(Some("Claude")), Some(AgentId::Claude));
        assert_eq!(agent_id_from_auto_type(Some("OPENCODE")), Some(AgentId::OpenCode));
    }

    #[test]
    fn auto_type_rejects_unknown_binaries() {
        assert_eq!(agent_id_from_auto_type(Some("pwsh")), None);
        assert_eq!(agent_id_from_auto_type(Some("gemini --foo")), None);
        assert_eq!(agent_id_from_auto_type(Some("notclaude")), None);
        // `claudeflare` must NOT match Claude — same anti-substring rule
        // `is_claude_auto_type` enforced.
        assert_eq!(agent_id_from_auto_type(Some("claudeflare")), None);
    }

    #[test]
    fn opencode_cli_alias_resolves_to_opencode() {
        // Windows OpenCode npm package ships the binary as
        // `opencode-cli.exe`; the alias keeps
        // `agent_id_from_auto_type` honest when the sidebar's
        // "New OpenCode session" row launches `opencode-cli`.
        assert_eq!(AgentId::from_str("opencode-cli"), Some(AgentId::OpenCode));
        assert_eq!(AgentId::from_str("OPENCODE-CLI"), Some(AgentId::OpenCode));
        assert_eq!(
            agent_id_from_auto_type(Some("opencode-cli")),
            Some(AgentId::OpenCode),
        );
        assert_eq!(
            agent_id_from_auto_type(Some("opencode-cli --continue")),
            Some(AgentId::OpenCode),
        );
    }
}
