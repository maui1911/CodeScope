//! Pure helpers for tab drag-reorder + cross-group reparent.
//!
//! Kept in `core` (away from gpui) so the drop-index math can be unit
//! tested without spinning up a window. Mirrors the
//! `ResolveDropIndex` loop in C# `GroupStripView.xaml.cs` — split a
//! cursor X against each tab's horizontal mid-point and return the
//! index of the gap the cursor sits over.

/// Axis-aligned tab bounds along the strip. Only `left` and `right`
/// are needed for the index computation; vertical extent and tab id
/// belong to the caller. `f32` instead of gpui's `Pixels` keeps this
/// crate independent of the rendering layer (and trivially testable).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TabRect {
    /// Left edge of the tab in strip-local pixels (or any coordinate
    /// frame, as long as it matches `cursor_x`).
    pub left: f32,
    /// Right edge of the tab in the same coordinate frame as `left`.
    pub right: f32,
}

impl TabRect {
    /// Horizontal midpoint — splits the tab into "drop before" and
    /// "drop after" halves.
    #[inline]
    fn mid_x(&self) -> f32 {
        (self.left + self.right) * 0.5
    }
}

/// Resolve where a tab should be inserted given a cursor X over the
/// strip and the current tab rects. Mirrors C#
/// `GroupStripView.ResolveDropIndex`: the first tab whose midpoint is
/// to the right of the cursor wins; otherwise the cursor sits past
/// every tab and we return `tabs.len()` (append).
///
/// Returns a value in `0..=tabs.len()` — caller-side `Vec::insert`
/// accepts both endpoints. An empty strip resolves to `0`.
pub fn compute_drop_index(cursor_x: f32, tabs: &[TabRect]) -> usize {
    for (i, rect) in tabs.iter().enumerate() {
        if cursor_x < rect.mid_x() {
            return i;
        }
    }
    tabs.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(left: f32, right: f32) -> TabRect {
        TabRect { left, right }
    }

    #[test]
    fn empty_strip_resolves_to_zero() {
        assert_eq!(compute_drop_index(50.0, &[]), 0);
    }

    #[test]
    fn before_first_tab_returns_zero() {
        let tabs = [rect(0.0, 100.0), rect(100.0, 200.0)];
        // Cursor 30 px in — left of the first tab's midpoint (50).
        assert_eq!(compute_drop_index(30.0, &tabs), 0);
    }

    #[test]
    fn past_first_midpoint_returns_one() {
        let tabs = [rect(0.0, 100.0), rect(100.0, 200.0)];
        // 60 px — past tab 0's mid (50), before tab 1's mid (150).
        assert_eq!(compute_drop_index(60.0, &tabs), 1);
    }

    #[test]
    fn past_last_midpoint_returns_len_for_append() {
        let tabs = [rect(0.0, 100.0), rect(100.0, 200.0)];
        // 180 px — past tab 1's mid (150). Append slot.
        assert_eq!(compute_drop_index(180.0, &tabs), 2);
    }

    #[test]
    fn cursor_far_to_the_right_still_appends() {
        let tabs = [rect(0.0, 100.0)];
        assert_eq!(compute_drop_index(9999.0, &tabs), 1);
    }

    #[test]
    fn cursor_far_to_the_left_returns_zero() {
        let tabs = [rect(50.0, 150.0)];
        // Negative is conceptually outside the strip, but the helper
        // is total — `-100 < mid (100)` so we resolve to 0.
        assert_eq!(compute_drop_index(-100.0, &tabs), 0);
    }

    #[test]
    fn exactly_on_midpoint_resolves_after() {
        let tabs = [rect(0.0, 100.0), rect(100.0, 200.0)];
        // `<` is strict, so mid-x lands in the *after* half — matches
        // C# `pt.X < center.X` semantics.
        assert_eq!(compute_drop_index(50.0, &tabs), 1);
    }

    #[test]
    fn three_tabs_picks_middle_gap() {
        let tabs = [rect(0.0, 100.0), rect(100.0, 200.0), rect(200.0, 300.0)];
        // 120 — past tab 0's mid (50), before tab 1's mid (150).
        assert_eq!(compute_drop_index(120.0, &tabs), 1);
        // 220 — past tab 1's mid (150), before tab 2's mid (250).
        assert_eq!(compute_drop_index(220.0, &tabs), 2);
    }
}
