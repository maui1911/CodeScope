//! Restart the `claude` in a tab onto the installed build, keeping its
//! conversation.
//!
//! The tab's shell outlives the agent (`pwsh -NoExit`, or the
//! auto-typed command on other shells), so the restart is: end the
//! idle `claude` the way a user would — Ctrl+C — then type
//! `claude --resume <session id>` at the shell prompt it drops back to.
//!
//! An idle Claude Code needs up to three Ctrl+C: the first clears a
//! draft prompt if there is one, the next arms "press Ctrl+C again to
//! exit", the one after exits. The arm only holds for a moment, so the
//! interrupts go out a short gap apart, and stop as soon as `claude`
//! is gone. With no draft the second one already exits; a third then
//! only goes out if shutdown is still running, which at worst cuts
//! that shutdown short — the idle conversation is on disk already.

use std::time::Duration;

/// The byte a terminal sends for Ctrl+C.
pub const INTERRUPT: &[u8] = b"\x03";

/// What to do next while stopping a tab's `claude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopStep {
    /// Send [`INTERRUPT`], then give `claude` up to this long to exit
    /// before asking again.
    Interrupt { wait: Duration },
    /// `claude` is gone — type the resume command.
    Resume,
    /// Still running after every interrupt; leave the tab alone.
    GiveUp,
}

/// How long to wait for `claude` to exit after each interrupt. The
/// gaps stay inside Claude Code's double-press window so the interrupt
/// after an arming one exits; after the last, shutdown gets time to
/// stop MCP servers before the tab is given up on.
const WAITS: [Duration; 3] = [
    Duration::from_millis(450),
    Duration::from_millis(450),
    Duration::from_secs(8),
];

/// The next step, given how many interrupts went out so far and whether
/// the tab's `claude` process is still alive.
pub fn next_step(interrupts_sent: usize, claude_running: bool) -> StopStep {
    if !claude_running {
        return StopStep::Resume;
    }
    match WAITS.get(interrupts_sent) {
        Some(&wait) => StopStep::Interrupt { wait },
        None => StopStep::GiveUp,
    }
}

/// The command that brings the tab's `claude` back once it exited:
/// `claude --resume <id>` when the conversation has a transcript, a
/// plain `claude` otherwise. A session nobody sent a prompt in yet
/// already has an id in its pid file but nothing on disk, and resuming
/// it fails with "No conversation found".
pub fn relaunch_command(
    profile: &crate::AgentProfile,
    session_id: &str,
    has_transcript: bool,
) -> Option<String> {
    if has_transcript {
        crate::build_resume_auto_type(profile, Some(session_id))
    } else {
        crate::build_new_session_auto_type(profile)
    }
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
    fn relaunch_resumes_a_conversation_with_a_transcript() {
        assert_eq!(
            relaunch_command(&claude_profile(), "sid-a", true).as_deref(),
            Some("claude --resume sid-a")
        );
    }

    #[test]
    fn relaunch_starts_fresh_when_nothing_was_ever_sent() {
        assert_eq!(relaunch_command(&claude_profile(), "sid-a", false).as_deref(), Some("claude"));
    }

    #[test]
    fn resumes_as_soon_as_claude_is_gone() {
        assert_eq!(next_step(0, false), StopStep::Resume);
        assert_eq!(next_step(2, false), StopStep::Resume);
    }

    #[test]
    fn interrupts_inside_the_double_press_window_then_waits_for_shutdown() {
        let waits: Vec<StopStep> = (0..3).map(|n| next_step(n, true)).collect();
        assert_eq!(
            waits,
            vec![
                StopStep::Interrupt { wait: Duration::from_millis(450) },
                StopStep::Interrupt { wait: Duration::from_millis(450) },
                StopStep::Interrupt { wait: Duration::from_secs(8) },
            ]
        );
    }

    #[test]
    fn gives_up_after_three_interrupts() {
        assert_eq!(next_step(3, true), StopStep::GiveUp);
        assert_eq!(next_step(9, true), StopStep::GiveUp);
    }
}
