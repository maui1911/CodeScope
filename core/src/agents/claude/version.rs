//! Which Claude Code build is installed, and whether a running tab is
//! behind it.
//!
//! Claude Code's native installer swaps `~/.local/bin/claude.exe` while
//! sessions are still running (it renames the in-use image aside to
//! `claude.exe.old.<ts>.<pid>`), so an update lands without closing
//! anything. Every `claude` that was already running keeps the old
//! build until it restarts — the "Update installed · Restart to update"
//! footer. Comparing the version each tab's process reports in its
//! `~/.claude/sessions/<pid>.json` (see [`super::live_session`]) with
//! the installed one names the tabs that need that restart.

use std::fmt;

/// A dotted numeric Claude Code version (`2.1.326`). Ordered component
/// by component, so `2.1.40 < 2.1.326`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaudeVersion(Vec<u64>);

impl ClaudeVersion {
    /// Parse `2.1.326`. A pre-release / build suffix after the numeric
    /// core (`2.1.326-beta.1`) is ignored. `None` for anything without
    /// a numeric core.
    pub fn parse(text: &str) -> Option<Self> {
        let core = text.split(['-', '+']).next()?;
        let parts = core
            .split('.')
            .map(|part| part.parse::<u64>().ok())
            .collect::<Option<Vec<u64>>>()?;
        Some(Self(parts))
    }
}

impl fmt::Display for ClaudeVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let parts: Vec<String> = self.0.iter().map(u64::to_string).collect();
        f.write_str(&parts.join("."))
    }
}

/// Parse the output of `claude --version` (`2.1.326 (Claude Code)`).
pub fn parse_version_output(output: &str) -> Option<ClaudeVersion> {
    ClaudeVersion::parse(output.split_whitespace().next()?)
}

/// The Claude Code build a freshly started `claude` would run right
/// now, from `claude --version`. `None` when `claude` is not on PATH or
/// its output is unrecognised, or it doesn't answer within
/// [`VERSION_TIMEOUT`]. Spawns a process (a few hundred ms) and blocks
/// on it, so call it off the UI thread.
///
/// `claude` resolves through CodeScope's own PATH, which can differ
/// from a tab shell's (a profile that prepends an npm install, say).
/// Callers must not keep acting on a mismatch: a restart that comes
/// back on the "old" build is not retried.
pub fn installed_version() -> Option<ClaudeVersion> {
    use std::io::Read;
    use std::process::Stdio;
    use std::time::Instant;

    let mut cmd = crate::process::no_window_command("claude");
    for var in crate::process::INHERITED_AGENT_SESSION_VARS {
        cmd.env_remove(var);
    }
    let mut child = cmd
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    parse_version_output(&out)
}

/// How long `claude --version` may take before the check gives up —
/// a hung CLI must not stall the update watch for good.
pub const VERSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Whether a tab running `running` should restart to pick up
/// `installed`. An unparseable running version is never outdated —
/// better to miss a nudge than to restart a tab for nothing.
pub fn is_outdated(running: Option<&str>, installed: &ClaudeVersion) -> bool {
    running
        .and_then(ClaudeVersion::parse)
        .is_some_and(|running| running < *installed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> ClaudeVersion {
        ClaudeVersion::parse(text).unwrap()
    }

    #[test]
    fn parses_a_dotted_version() {
        assert_eq!(v("2.1.326").to_string(), "2.1.326");
    }

    #[test]
    fn ignores_a_prerelease_suffix() {
        assert_eq!(v("2.1.326-beta.1"), v("2.1.326"));
    }

    #[test]
    fn rejects_text_without_a_numeric_core() {
        assert_eq!(ClaudeVersion::parse(""), None);
        assert_eq!(ClaudeVersion::parse("latest"), None);
        assert_eq!(ClaudeVersion::parse("v2.1.3"), None);
        assert_eq!(ClaudeVersion::parse("2..3"), None);
    }

    #[test]
    fn orders_numerically_not_lexically() {
        assert!(v("2.1.40") < v("2.1.326"));
        assert!(v("2.1.326") < v("2.2.0"));
        assert!(v("2.10.0") > v("2.9.99"));
    }

    #[test]
    fn parses_claude_version_output() {
        assert_eq!(parse_version_output("2.1.326 (Claude Code)\n"), Some(v("2.1.326")));
        assert_eq!(parse_version_output("  2.1.327 (Claude Code)\r\n"), Some(v("2.1.327")));
        assert_eq!(parse_version_output(""), None);
        assert_eq!(parse_version_output("command not found"), None);
    }

    #[test]
    fn a_tab_behind_the_installed_build_is_outdated() {
        assert!(is_outdated(Some("2.1.325"), &v("2.1.326")));
    }

    #[test]
    fn a_tab_on_or_ahead_of_the_installed_build_is_not() {
        assert!(!is_outdated(Some("2.1.326"), &v("2.1.326")));
        // A tab started before a manual downgrade.
        assert!(!is_outdated(Some("2.1.327"), &v("2.1.326")));
    }

    #[test]
    fn an_unknown_running_version_is_not_outdated() {
        assert!(!is_outdated(None, &v("2.1.326")));
        assert!(!is_outdated(Some("garbage"), &v("2.1.326")));
    }
}
