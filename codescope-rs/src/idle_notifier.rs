//! Cross-platform OS notification for the "turn complete" toast.
//!
//! Port of the C# `WindowsIdleToastNotifier` / `IIdleToastNotifier`
//! pair (`src/CodeScope.App/Notifications/`, `src/CodeScope.Ui/Services/`).
//! The Rust port uses [`notify_rust`] so the same code path lights up
//! a WinRT toast on Windows, a FreeDesktop dbus notification on Linux,
//! and an `NSUserNotification` on macOS — no per-platform fork required.
//!
//! # Surface vs the C# build
//!
//! - **Show-only**: `notify_rust` does not support click-activation on
//!   Windows (it's a Linux/BSD/macOS-only API), and the in-app bell
//!   already handles tab routing on click. So the toast is purely a
//!   "look at the app" signal — same role the C# build's toast served
//!   in practice once the bell shipped.
//! - **Window-active gate, not minimised**: gpui exposes
//!   `Window::is_window_active()` cross-platform but no minimised
//!   getter. We gate on "window is not the active OS window" instead,
//!   which is broader than the C# minimised-only check (toast fires
//!   when CodeScope is behind another window too) but matches user
//!   intent — if you're staring at the app, you don't need an OS
//!   toast.
//!
//! # De-dupe
//!
//! The telemetry layer fires the same transition from the FS watcher
//! *and* the poll fallback (~100–500 ms apart) — two toasts for one
//! turn-complete would be noise. We track the last-fired wall-clock
//! per session id and drop a fire that lands inside a 2 s window after
//! the previous one, matching the C# `DedupeWindow`. Stale entries are
//! pruned in-place on every call so the map stays bounded to "sessions
//! seen in the last 2 s" (a handful of entries at most).

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Window the de-dupe filter keeps a session quiet after the previous
/// fire. Matches `WindowsIdleToastNotifier.DedupeWindow` in the C#
/// build (2 s).
pub const DEDUPE_WINDOW: Duration = Duration::from_secs(2);

/// Per-session de-dupe state for "turn complete" OS toasts.
///
/// Tested in isolation via [`Self::should_fire`]; the actual toast
/// `show()` call is intentionally separate so unit tests don't need
/// to spawn a real notification surface.
pub struct IdleNotifier {
    last_fired: HashMap<String, Instant>,
}

impl IdleNotifier {
    pub fn new() -> Self {
        Self { last_fired: HashMap::new() }
    }

    /// Returns `true` when a toast for `session_id` should be shown,
    /// `false` when the dedupe window is still active for that id.
    ///
    /// Updates internal state on every call: on a `true` return we
    /// stamp `now` so the next call inside the window is dropped; on
    /// a `false` return we leave the existing stamp in place.
    /// Always prunes stale entries before checking so a long-running
    /// session can't pin the map.
    pub fn should_fire(&mut self, session_id: &str) -> bool {
        self.should_fire_at(session_id, Instant::now())
    }

    /// Test seam for [`Self::should_fire`] — accepts the wall-clock so
    /// tests don't have to sleep through the dedupe window.
    pub fn should_fire_at(&mut self, session_id: &str, now: Instant) -> bool {
        // Prune entries strictly older than the window. `<=` keeps an
        // entry at the exact boundary, matching the C# build's
        // `kv.Value < cutoff` predicate (which retains values that
        // *equal* the cutoff). One-nanosecond edge but worth pinning
        // for parity with the test fixture.
        self.last_fired
            .retain(|_, t| now.saturating_duration_since(*t) <= DEDUPE_WINDOW);
        if self.last_fired.contains_key(session_id) {
            return false;
        }
        self.last_fired.insert(session_id.to_string(), now);
        true
    }

    /// Dropped to keep tests independent — fresh notifier per case.
    #[cfg(test)]
    fn entry_count(&self) -> usize {
        self.last_fired.len()
    }
}

impl Default for IdleNotifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Fire-and-forget OS notification. Intended to be spawned on
/// `cx.background_executor()` — the underlying `notify-rust` call can
/// block for tens of milliseconds on Windows (COM marshalling) and
/// macOS (NSUserNotification round-trip), and we don't want to stall
/// the gpui main loop on a non-essential surface.
///
/// Errors are swallowed: the toast is a best-effort enhancement and
/// shouldn't be able to take the app down. Common failures are
/// sandbox / missing Start-menu shortcut on a first-run unpackaged
/// exe.
pub fn fire_os_notification(title: &str, detail: &str) {
    let _ = notify_rust::Notification::new()
        .summary(title)
        .body(detail)
        .appname("CodeScope")
        .show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_fire_returns_true_and_records_session() {
        let mut n = IdleNotifier::new();
        let t0 = Instant::now();
        assert!(n.should_fire_at("sid-a", t0));
        assert_eq!(n.entry_count(), 1);
    }

    #[test]
    fn second_fire_within_window_returns_false() {
        let mut n = IdleNotifier::new();
        let t0 = Instant::now();
        assert!(n.should_fire_at("sid-a", t0));
        let t1 = t0 + Duration::from_millis(500);
        assert!(!n.should_fire_at("sid-a", t1));
    }

    #[test]
    fn second_fire_after_window_returns_true_again() {
        let mut n = IdleNotifier::new();
        let t0 = Instant::now();
        assert!(n.should_fire_at("sid-a", t0));
        let t1 = t0 + DEDUPE_WINDOW + Duration::from_millis(1);
        assert!(n.should_fire_at("sid-a", t1));
    }

    #[test]
    fn distinct_sessions_do_not_block_each_other() {
        let mut n = IdleNotifier::new();
        let t0 = Instant::now();
        assert!(n.should_fire_at("sid-a", t0));
        // Inside the window for sid-a, but sid-b is fresh.
        assert!(n.should_fire_at("sid-b", t0 + Duration::from_millis(100)));
        assert_eq!(n.entry_count(), 2);
    }

    #[test]
    fn stale_entries_are_pruned_on_every_call() {
        let mut n = IdleNotifier::new();
        let t0 = Instant::now();
        // Seed a stale-from-the-future entry; pruning happens before
        // the contains_key check so it should not block sid-a.
        n.should_fire_at("sid-stale", t0);
        let t_far = t0 + DEDUPE_WINDOW + Duration::from_secs(60);
        // The stale "sid-stale" entry is now well outside the window;
        // calling for a *different* session should prune it.
        assert!(n.should_fire_at("sid-fresh", t_far));
        assert_eq!(n.entry_count(), 1, "stale entry should have been pruned");
    }

    #[test]
    fn re_fire_at_exact_window_boundary_is_blocked() {
        // The check is `< DEDUPE_WINDOW`, so a fire at exactly
        // `t0 + DEDUPE_WINDOW` is still inside the window and should
        // be dropped. One nanosecond later it's allowed (covered by
        // `second_fire_after_window_returns_true_again`).
        let mut n = IdleNotifier::new();
        let t0 = Instant::now();
        assert!(n.should_fire_at("sid-a", t0));
        assert!(!n.should_fire_at("sid-a", t0 + DEDUPE_WINDOW));
    }
}
