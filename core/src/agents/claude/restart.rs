//! Restart the `claude` in a tab onto the installed build, keeping its
//! conversation.
//!
//! The tab's shell outlives the agent (`pwsh -NoExit`, or the
//! auto-typed command on other shells), so the restart is: end the
//! idle `claude` from its own prompt, then type
//! `claude --resume <session id>` at the shell prompt it drops back to.
//!
//! Ending it takes Ctrl+C then `/exit`: Ctrl+C clears a draft prompt if
//! there is one (otherwise it only shows "press Ctrl+C again to exit"),
//! and `/exit` on the now empty prompt shuts Claude Code down cleanly.
//! No double-press timing window is involved, and nothing interrupts
//! the shutdown once it runs.

use std::time::Duration;

/// What the tab's `claude` looks like right now, as far as the caller
/// could tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeState {
    /// The process is gone from under the tab's shell.
    Gone,
    /// Still running and waiting for a prompt (or its pid file is
    /// already gone, mid-shutdown).
    Idle,
    /// Still running and working on a turn.
    Busy,
    /// Couldn't tell — the process snapshot failed, or the tab's shell
    /// itself is gone. Never act on a guess.
    Unknown,
}

/// What to do next while stopping a tab's `claude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopStep {
    /// Type these bytes, then give `claude` up to `wait` to exit.
    Send { bytes: &'static [u8], wait: Duration },
    /// `claude` is gone — type the relaunch command.
    Resume,
    /// Still running after the whole sequence; leave the tab alone.
    GiveUp,
    /// The user took the tab back (it went busy) or its state is
    /// unknown; stop without typing anything else.
    Abort,
}

/// Ctrl+C, then `/exit` + Enter. The first wait only lets the Ctrl+C
/// land; the second gives shutdown time to stop MCP servers.
const SEQUENCE: [(&[u8], Duration); 2] = [
    (b"\x03", Duration::from_millis(450)),
    (b"/exit\r", Duration::from_secs(10)),
];

/// The next step, given how many [`SEQUENCE`] steps went out so far
/// and the state of the tab's `claude`.
pub fn next_step(sent: usize, state: ClaudeState) -> StopStep {
    match state {
        ClaudeState::Gone => StopStep::Resume,
        ClaudeState::Unknown => StopStep::Abort,
        _ if sent >= SEQUENCE.len() => StopStep::GiveUp,
        ClaudeState::Busy => StopStep::Abort,
        ClaudeState::Idle => {
            let (bytes, wait) = SEQUENCE[sent];
            StopStep::Send { bytes, wait }
        }
    }
}

/// A session id safe to type into a shell: UUID-shaped (ASCII
/// alphanumerics and dashes, at most 64). It comes from a file any
/// process in the tab can write, and is typed at a shell prompt.
fn is_plain_session_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The command that brings the tab's `claude` back once it exited:
/// `claude --resume <id>` when the conversation has a transcript, a
/// plain `claude` otherwise. A session nobody sent a prompt in yet
/// already has an id in its pid file but nothing on disk, and resuming
/// it fails with "No conversation found". `None` for a session id that
/// isn't safe to type into a shell.
pub fn relaunch_command(
    profile: &crate::AgentProfile,
    session_id: &str,
    has_transcript: bool,
) -> Option<String> {
    if !is_plain_session_id(session_id) {
        return None;
    }
    if has_transcript {
        crate::build_resume_auto_type(profile, Some(session_id))
    } else {
        crate::build_new_session_auto_type(profile)
    }
}

/// Whether an outdated tab may be restarted without asking. Only when
/// nothing the user has in hand can be lost or jump under them: the
/// `claude` is `idle` (not mid-turn), the user hasn't typed into it
/// since their last Enter (Ctrl+C would wipe a draft), and it isn't on
/// screen in a focused window (the screen would flash and redraw while
/// they read it). A tab that fails waits for a later check.
pub fn may_auto_restart(idle: bool, may_have_draft: bool, in_view: bool) -> bool {
    idle && !may_have_draft && !in_view
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_profile() -> crate::AgentProfile {
        crate::agent_registry::built_in_defaults()
            .into_iter()
            .find(|p| p.id == "claude")
            .unwrap()
    }

    #[test]
    fn sends_ctrl_c_then_exit_while_claude_idles() {
        assert_eq!(
            next_step(0, ClaudeState::Idle),
            StopStep::Send { bytes: b"\x03", wait: Duration::from_millis(450) }
        );
        assert_eq!(
            next_step(1, ClaudeState::Idle),
            StopStep::Send { bytes: b"/exit\r", wait: Duration::from_secs(10) }
        );
    }

    #[test]
    fn resumes_as_soon_as_claude_is_gone() {
        assert_eq!(next_step(0, ClaudeState::Gone), StopStep::Resume);
        assert_eq!(next_step(2, ClaudeState::Gone), StopStep::Resume);
    }

    #[test]
    fn gives_up_when_claude_outlives_the_sequence() {
        assert_eq!(next_step(2, ClaudeState::Idle), StopStep::GiveUp);
        // `/exit` may flip the status while shutting down.
        assert_eq!(next_step(2, ClaudeState::Busy), StopStep::GiveUp);
    }

    #[test]
    fn aborts_when_the_tab_goes_busy_or_its_state_is_unknown() {
        assert_eq!(next_step(0, ClaudeState::Busy), StopStep::Abort);
        assert_eq!(next_step(1, ClaudeState::Busy), StopStep::Abort);
        // An empty process snapshot must never read as "claude exited".
        assert_eq!(next_step(0, ClaudeState::Unknown), StopStep::Abort);
        assert_eq!(next_step(2, ClaudeState::Unknown), StopStep::Abort);
    }

    #[test]
    fn auto_restarts_an_idle_tab_out_of_view_without_a_draft() {
        assert!(may_auto_restart(true, false, false));
    }

    #[test]
    fn never_auto_restarts_a_busy_tab_a_draft_or_the_tab_in_view() {
        assert!(!may_auto_restart(false, false, false));
        assert!(!may_auto_restart(true, true, false));
        assert!(!may_auto_restart(true, false, true));
    }

    #[test]
    fn relaunch_resumes_a_conversation_with_a_transcript() {
        assert_eq!(
            relaunch_command(&claude_profile(), "f3b97fd8-1697-4325-b075-da31a284b704", true)
                .as_deref(),
            Some("claude --resume f3b97fd8-1697-4325-b075-da31a284b704")
        );
    }

    #[test]
    fn relaunch_starts_fresh_when_nothing_was_ever_sent() {
        assert_eq!(relaunch_command(&claude_profile(), "sid-a", false).as_deref(), Some("claude"));
    }

    #[test]
    fn relaunch_refuses_a_session_id_that_is_not_safe_to_type() {
        let profile = claude_profile();
        for id in ["", "x; Remove-Item C:\\", "a b", "id\r", "$(calc)", &"a".repeat(65)] {
            assert_eq!(relaunch_command(&profile, id, true), None, "{id:?}");
        }
    }
}
