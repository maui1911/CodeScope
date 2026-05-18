//! Persistent notification state.
//!
//! This is the Rust port of the C# `INotificationService` /
//! `NotificationService` pair (`legacy:CodeScope.Core/Services/`).
//! The popover render lives in `app.rs` as `AppShell::render_notifications_popover`,
//! mirroring how `render_toasts` and `render_tab_menu` are structured there.
//!
//! # Lifecycle
//!
//! `Notifications` lives as a plain struct field inside `AppShell` — no
//! `Entity<T>` wrapper needed because all mutations are driven from
//! `AppShell` methods that already hold `&mut self`.
//! `AppShell::push_notification` is the only external writer path; the
//! future bell button calls `toggle_open` via `AppShell`.
//!
//! # Ring buffer
//!
//! Newest entry sits at index 0 (most-recent-first), mirroring
//! `LinkedList.AddFirst` in the C# build.  When the buffer is full
//! (`MAX_ENTRIES = 50`) the oldest tail entry falls off.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use gpui::SharedString;

// ─── Id generator ──────────────────────────────────────────────────────────

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

// ─── Domain types ──────────────────────────────────────────────────────────

/// Semantic class of a notification — drives the colour of the kind dot
/// in the popover.  Mirrors the C# `NotificationKind` enum.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationKind {
    /// Agent finished a turn after being in Wait/Composing — user can reply.
    SessionReady,
    /// Agent paused for a permission prompt (manual-mode tool_use).
    SessionWaiting,
}

/// One entry in the ring buffer.  Mutable only for the `read` flag
/// (mark_read / mark_all_read).
#[derive(Clone, Debug)]
pub struct NotificationEntry {
    pub id: u64,
    pub kind: NotificationKind,
    pub title: SharedString,
    pub detail: SharedString,
    /// `None` when the notification is not tied to a specific session.
    pub session_title: Option<SharedString>,
    /// UTC seconds since the Unix epoch — formatted as "HH:mm" by
    /// `format_hhmm`.  Matches the `Timestamp` column in the BellPopup XAML.
    pub timestamp: u64,
    pub read: bool,
}

impl NotificationEntry {
    /// Create a new unread entry, stamping it with the current wall clock.
    pub fn new(
        kind: NotificationKind,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        session_title: Option<SharedString>,
    ) -> Self {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            id: next_id(),
            kind,
            title: title.into(),
            detail: detail.into(),
            session_title,
            timestamp,
            read: false,
        }
    }
}

// ─── Timestamp formatting ──────────────────────────────────────────────────

/// Format a Unix-epoch second value as "HH:mm" in local time.
///
/// Converts the *specific* timestamp to local time (not "current
/// offset applied to arbitrary timestamp"), so entries straddling
/// a DST boundary render with the offset that actually applied
/// when they occurred. We avoid the `chrono` crate (not in the
/// dependency tree); instead we go epoch_secs → FILETIME →
/// SystemTime UTC → SystemTime local via Win32 directly.
///
/// Falls back to UTC HH:mm if any step fails (the conversion can
/// only fail for timestamps before 1601 or after ~30828 AD, plus
/// non-Windows hosts).
pub fn format_hhmm(epoch_secs: u64) -> SharedString {
    let (h, m) = local_hh_mm(epoch_secs).unwrap_or_else(|| utc_hh_mm(epoch_secs));
    format!("{:02}:{:02}", h, m).into()
}

/// Convert a specific Unix-epoch second value to local-time
/// `(hour, minute)` using the OS's per-instant timezone rules
/// (handles DST transitions correctly, unlike "current offset
/// applied uniformly").
fn local_hh_mm(epoch_secs: u64) -> Option<(u16, u16)> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
        use windows::Win32::System::Time::{
            FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime,
        };

        // Unix epoch (1970-01-01) in 100-ns ticks since the Win32
        // FILETIME epoch (1601-01-01). Constant: 11644473600 seconds.
        const UNIX_TO_FILETIME_OFFSET_TICKS: u64 = 11_644_473_600 * 10_000_000;
        let ticks = UNIX_TO_FILETIME_OFFSET_TICKS
            .checked_add(epoch_secs.checked_mul(10_000_000)?)?;
        let ft = FILETIME {
            dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32,
            dwHighDateTime: (ticks >> 32) as u32,
        };

        // SAFETY: both calls take `*const FILETIME` / `*const SYSTEMTIME`
        // pointing at locals we own, and write to `*mut SYSTEMTIME`
        // pointing at locals we own. The `Option<*const TIME_ZONE_INFORMATION>`
        // is None to use the system's current rules.
        unsafe {
            let mut utc_st = SYSTEMTIME::default();
            FileTimeToSystemTime(&ft, &mut utc_st).ok()?;
            let mut local_st = SYSTEMTIME::default();
            SystemTimeToTzSpecificLocalTime(None, &utc_st, &mut local_st).ok()?;
            Some((local_st.wHour, local_st.wMinute))
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = epoch_secs;
        None
    }
}

/// Pure-arithmetic UTC fallback used when the Win32 conversion
/// chain fails (or on non-Windows hosts).
fn utc_hh_mm(epoch_secs: u64) -> (u16, u16) {
    let secs_of_day = epoch_secs % 86_400;
    ((secs_of_day / 3_600) as u16, ((secs_of_day % 3_600) / 60) as u16)
}

// ─── Ring buffer state ─────────────────────────────────────────────────────

/// Maximum number of entries kept in the ring buffer.
/// Mirrors `NotificationService.MaxEntries` default (50) in the C# build.
pub const MAX_ENTRIES: usize = 50;

/// In-memory ring buffer of recent agent events.
///
/// Lives as a plain struct field inside `AppShell` — no separate `Entity<T>`
/// needed.  The popover render is `AppShell::render_notifications_popover`
/// in `app.rs` (same pattern as `render_toasts` / `render_tab_menu`).
pub struct Notifications {
    /// Most-recent-first.  Cap: `MAX_ENTRIES`.
    entries: Vec<NotificationEntry>,
    /// Whether the popover is currently visible.  The bell button (integrating
    /// PR) toggles this; the render method checks it before producing an element.
    is_open: bool,
}

impl Notifications {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            is_open: false,
        }
    }

    // ── Accessors ──────────────────────────────────────────────────────

    /// Snapshot of entries, most-recent-first.
    pub fn entries(&self) -> &[NotificationEntry] {
        &self.entries
    }

    pub fn has_any(&self) -> bool {
        !self.entries.is_empty()
    }

    /// Returns `true` when at least one entry has not been read.
    /// Used by the bell button to show the unread dot.
    pub fn has_unread(&self) -> bool {
        self.entries.iter().any(|e| !e.read)
    }

    pub fn is_open(&self) -> bool {
        self.is_open
    }

    // ── Popover visibility ─────────────────────────────────────────────

    pub fn set_open(&mut self, open: bool) {
        self.is_open = open;
    }

    /// Toggle popover open/closed.  Called by the bell button.
    pub fn toggle(&mut self) {
        self.is_open = !self.is_open;
    }

    // ── Mutations ──────────────────────────────────────────────────────

    /// Push a new unread entry, evicting the oldest when the ring is full.
    /// Returns the id of the new entry.
    pub fn push(
        &mut self,
        kind: NotificationKind,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        session_title: Option<SharedString>,
    ) -> u64 {
        let entry = NotificationEntry::new(kind, title, detail, session_title);
        let id = entry.id;
        self.entries.insert(0, entry);
        if self.entries.len() > MAX_ENTRIES {
            self.entries.pop();
        }
        id
    }

    /// Remove all entries and close the popover.
    pub fn clear_all(&mut self) {
        self.entries.clear();
        self.is_open = false;
    }

    /// Mark the entry read and return its `session_title` so the caller
    /// (`AppShell`) can jump to that tab.
    pub fn activate(&mut self, id: u64) -> Option<SharedString> {
        let entry = self.entries.iter_mut().find(|e| e.id == id)?;
        entry.read = true;
        entry.session_title.clone()
    }
}

impl Default for Notifications {
    fn default() -> Self {
        Self::new()
    }
}

// ─── Unit tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn push_one(kind: NotificationKind, title: &'static str) -> (u64, Notifications) {
        let mut n = Notifications::new();
        let id = n.push(kind, title, "detail", None);
        (id, n)
    }

    #[test]
    fn push_adds_entry_most_recent_first() {
        let mut n = Notifications::new();
        n.push(NotificationKind::SessionReady, "first", "d", None);
        n.push(NotificationKind::SessionReady, "second", "d", None);
        assert_eq!(n.entries().len(), 2);
        assert_eq!(n.entries()[0].title, SharedString::from("second"));
        assert_eq!(n.entries()[1].title, SharedString::from("first"));
    }

    #[test]
    fn push_evicts_oldest_when_at_cap() {
        let mut n = Notifications::new();
        for i in 0..MAX_ENTRIES + 5 {
            n.push(NotificationKind::SessionReady, format!("entry {i}"), "d", None);
        }
        assert_eq!(n.entries().len(), MAX_ENTRIES);
        // Newest entry is at index 0.
        assert_eq!(
            n.entries()[0].title,
            SharedString::from(format!("entry {}", MAX_ENTRIES + 4))
        );
    }

    #[test]
    fn new_entries_are_unread() {
        let (_, n) = push_one(NotificationKind::SessionReady, "t");
        assert!(n.has_unread());
        assert!(!n.entries()[0].read);
    }

    #[test]
    fn clear_all_empties_and_closes() {
        let mut n = Notifications::new();
        n.push(NotificationKind::SessionReady, "a", "d", None);
        n.set_open(true);
        n.clear_all();
        assert!(!n.has_any());
        assert!(!n.is_open());
    }

    #[test]
    fn has_any_and_has_unread_invariants() {
        let mut n = Notifications::new();
        assert!(!n.has_any());
        assert!(!n.has_unread());

        let id = n.push(NotificationKind::SessionReady, "t", "d", None);
        assert!(n.has_any());
        assert!(n.has_unread());

        n.activate(id);
        assert!(n.has_any()); // entry still present
        assert!(!n.has_unread()); // but now read
    }

    #[test]
    fn toggle_open_flips_state() {
        let mut n = Notifications::new();
        assert!(!n.is_open());
        n.toggle();
        assert!(n.is_open());
        n.toggle();
        assert!(!n.is_open());
    }

    #[test]
    fn activate_marks_read_and_returns_session_title() {
        let mut n = Notifications::new();
        let id = n.push(
            NotificationKind::SessionReady,
            "t",
            "d",
            Some("my-session".into()),
        );
        let title = n.activate(id);
        assert_eq!(title, Some(SharedString::from("my-session")));
        assert!(n.entries()[0].read);
    }

    #[test]
    fn activate_unknown_id_returns_none() {
        let mut n = Notifications::new();
        assert!(n.activate(9999).is_none());
    }

    #[test]
    fn format_hhmm_produces_padded_string() {
        // We can't guarantee the local offset in CI, so we just check
        // the format: exactly 5 chars, colon at index 2, digits elsewhere.
        let s: String = format_hhmm(0).to_string();
        assert_eq!(s.len(), 5);
        assert_eq!(&s[2..3], ":");
        assert!(s[0..2].chars().all(|c| c.is_ascii_digit()));
        assert!(s[3..5].chars().all(|c| c.is_ascii_digit()));
    }

}
