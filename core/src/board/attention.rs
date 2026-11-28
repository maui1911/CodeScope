//! When a dispatched session may be waiting for the user (ADR-0027): it is mid-turn, with nothing
//! new in its transcript, for [`MAY_BE_WAITING_AFTER`]. Claude Code writes a question or a
//! permission prompt to its transcript only once it is answered, so the session shows as busy
//! while it waits, and a long tool call or a long think looks the same.

use std::time::{Duration, Instant};

use crate::telemetry::{SessionState, TelemetrySnapshot};

/// How long a session goes mid-turn without anything new before it may be waiting.
pub const MAY_BE_WAITING_AFTER: Duration = Duration::from_secs(3 * 60);

/// What a board event that rings for the user is about; each can be turned off in Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    /// A research round posted its findings.
    Findings,
    /// A dispatch can't go on without the user.
    NeedsYou,
    /// A dispatched session went [`MAY_BE_WAITING_AFTER`] mid-turn with nothing new.
    MayBeWaiting,
}

impl Attention {
    /// Every kind, in the order Settings lists them.
    pub const ALL: [Attention; 3] =
        [Attention::Findings, Attention::NeedsYou, Attention::MayBeWaiting];
}

/// One session's time mid-turn with nothing new, as its snapshots show it over time.
#[derive(Debug, Default)]
pub struct Stall {
    /// The snapshot the stall started with, and when it was first seen.
    pending: Option<(TelemetrySnapshot, Instant)>,
    /// The start of the stall the bell rang for.
    rang_for: Option<Instant>,
}

impl Stall {
    /// Take the session's latest snapshot at `now`; `None` when it has no session.
    pub fn observe(&mut self, snapshot: Option<&TelemetrySnapshot>, now: Instant) {
        match (snapshot.and_then(mid_turn), &self.pending) {
            (Some(latest), Some((seen, _))) if latest == *seen => {}
            (Some(latest), _) => self.pending = Some((latest, now)),
            (None, _) => self.pending = None,
        }
    }

    /// When the stall started, once it has lasted [`MAY_BE_WAITING_AFTER`].
    pub fn waiting_since(&self, now: Instant) -> Option<Instant> {
        let (_, since) = self.pending.as_ref()?;
        (now.saturating_duration_since(*since) >= MAY_BE_WAITING_AFTER).then_some(*since)
    }

    /// Whether to ring the bell: once per stall, from when it counts as waiting.
    pub fn ring(&mut self, now: Instant) -> bool {
        let Some(since) = self.waiting_since(now) else {
            return false;
        };
        let first = self.rang_for != Some(since);
        self.rang_for = Some(since);
        first
    }
}

/// The snapshot as a stall compares it, when the session is mid-turn: a busy session that went
/// quiet past its timeout is still the same turn.
fn mid_turn(snapshot: &TelemetrySnapshot) -> Option<TelemetrySnapshot> {
    let state = if snapshot.quiet_timeout { SessionState::Busy } else { snapshot.state };
    matches!(state, SessionState::Busy | SessionState::PendingToolUse).then(|| {
        TelemetrySnapshot {
            state,
            quiet_timeout: false,
            ..snapshot.clone()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_kind_of_attention_is_listed_once() {
        let all = Attention::ALL;
        assert_eq!(all.len(), 3);
        assert!(all.iter().enumerate().all(|(i, a)| !all[..i].contains(a)));
    }

    fn snapshot(state: SessionState, tokens_used: u64) -> TelemetrySnapshot {
        TelemetrySnapshot {
            model: Some("claude-opus-5-5".into()),
            tokens_used,
            context_pct: None,
            turn_count: 2,
            last_turn_duration: None,
            state,
            quiet_timeout: false,
            title: None,
        }
    }

    const MINUTE: Duration = Duration::from_secs(60);

    #[test]
    fn one_tool_call_with_nothing_new_may_be_waiting_after_three_minutes() {
        let start = Instant::now();
        let mut stall = Stall::default();
        let pending = snapshot(SessionState::PendingToolUse, 1_000);
        stall.observe(Some(&pending), start);
        stall.observe(Some(&pending), start + 2 * MINUTE);
        assert_eq!(stall.waiting_since(start + 3 * MINUTE - Duration::from_secs(1)), None);
        assert_eq!(stall.waiting_since(start + 3 * MINUTE), Some(start));
    }

    #[test]
    fn a_busy_session_with_nothing_new_may_be_waiting_through_its_quiet_timeout() {
        let start = Instant::now();
        let mut stall = Stall::default();
        let busy = snapshot(SessionState::Busy, 1_000);
        stall.observe(Some(&busy), start);
        assert_eq!(stall.waiting_since(start + 3 * MINUTE), Some(start), "an unwritten prompt");
        let quiet = TelemetrySnapshot {
            quiet_timeout: true,
            ..snapshot(SessionState::Idle, 1_000)
        };
        stall.observe(Some(&quiet), start + 10 * MINUTE);
        assert_eq!(stall.waiting_since(start + 11 * MINUTE), Some(start), "the same quiet");
    }

    #[test]
    fn something_new_in_the_transcript_or_a_finished_turn_starts_over() {
        let start = Instant::now();
        let mut stall = Stall::default();
        stall.observe(Some(&snapshot(SessionState::PendingToolUse, 1_000)), start);
        let next_call = start + 2 * MINUTE;
        stall.observe(Some(&snapshot(SessionState::PendingToolUse, 1_400)), next_call);
        assert_eq!(stall.waiting_since(start + 4 * MINUTE), None, "a later tool call");
        assert_eq!(stall.waiting_since(next_call + 3 * MINUTE), Some(next_call));

        let result = next_call + 3 * MINUTE;
        stall.observe(Some(&snapshot(SessionState::Busy, 1_400)), result);
        assert_eq!(stall.waiting_since(result + 2 * MINUTE), None, "its result came in");

        stall.observe(Some(&snapshot(SessionState::Idle, 1_900)), result + 3 * MINUTE);
        assert_eq!(stall.waiting_since(result + 9 * MINUTE), None, "the turn ended");

        stall.observe(Some(&snapshot(SessionState::PendingToolUse, 1_800)), start);
        stall.observe(None, start + MINUTE);
        assert_eq!(stall.waiting_since(start + 9 * MINUTE), None, "its tab closed");
    }

    #[test]
    fn the_bell_rings_once_per_stall() {
        let start = Instant::now();
        let mut stall = Stall::default();
        let pending = snapshot(SessionState::PendingToolUse, 1_000);
        stall.observe(Some(&pending), start);
        assert!(!stall.ring(start + 2 * MINUTE));
        assert!(stall.ring(start + 3 * MINUTE));
        assert!(!stall.ring(start + 4 * MINUTE), "rang for this stall");

        let next = start + 5 * MINUTE;
        stall.observe(Some(&snapshot(SessionState::PendingToolUse, 1_500)), next);
        assert!(!stall.ring(next + MINUTE));
        assert!(stall.ring(next + 3 * MINUTE), "a stall of its own");
    }
}
