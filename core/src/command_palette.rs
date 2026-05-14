//! Command palette fuzzy matcher — pure scoring logic, no UI.
//!
//! 1:1 port of `CommandPaletteViewModel.Score` from the C# build
//! (`legacy:CodeScope.Ui/ViewModels/CommandPaletteViewModel.cs`). The
//! ranker is kept in `core` so the C# unit tests have a direct
//! mirror in `cargo test` without spinning up gpui.
//!
//! Scoring tiers (case-insensitive throughout):
//!
//! - **Empty needle** → `0`. Caller still includes the row, preserving
//!   the natural order of the input list (we only filter on `< 0`).
//! - **Contiguous substring at index 0** → `1500` (prefix bonus).
//! - **Contiguous substring elsewhere** → `1000 + max(0, 200 - idx)`,
//!   so earlier substrings beat later ones.
//! - **Subsequence match** → `100` + per-character bonuses:
//!     - `+5` for matches contiguous with the previous match,
//!     - `+1` otherwise,
//!     - `+3` when the matched char sits at a word boundary
//!       (start of haystack, or after `' '`, `'-'`, `'_'`, `'.'`, `'/'`).
//! - **No match** → `-1` (caller filters).
//!
//! Why not pull in a fuzzy-match crate: 60 lines, no extra dep, and
//! the C# tests pin exact integer scores (e.g. prefix ≥ 1500). A drop-
//! in third-party scorer would silently drift parity.

/// One scored row. Higher score = better match. Callers sort descending
/// and drop entries with `score < 0`.
///
/// `index` is the position into the caller's input slice so the caller
/// can resolve the original row (and its closure / metadata) after the
/// sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScoredIndex {
    pub score: i32,
    pub index: usize,
}

/// Score `haystack` against `needle`. Mirrors the C#
/// `CommandPaletteViewModel.Score` static method byte-for-byte.
///
/// Returns `-1` when the needle can't be matched at all (caller should
/// filter the row out), `0` for an empty needle (match-all), and a
/// positive integer otherwise — higher is a better match.
pub fn score(haystack: &str, needle: &str) -> i32 {
    if needle.is_empty() {
        return 0;
    }
    if haystack.is_empty() {
        return -1;
    }

    // ASCII-fast lowercase. Both inputs are user-typed UI labels —
    // realistic strings stay short and the C# build also uses
    // `ToLowerInvariant`, which folds case via Unicode tables; ASCII
    // folding is enough for the labels we ship ("New session", paths,
    // branches, theme ids — none of which carry case-sensitive
    // non-ASCII glyphs). If a downstream user introduces a non-ASCII
    // label that needs Unicode-aware folding we can revisit.
    let h: Vec<char> = haystack.chars().map(|c| c.to_ascii_lowercase()).collect();
    let n: Vec<char> = needle.chars().map(|c| c.to_ascii_lowercase()).collect();

    if let Some(idx) = find_subslice(&h, &n) {
        let bonus = if idx == 0 { 500 } else { (200 - idx as i32).max(0) };
        return 1000 + bonus;
    }

    let mut hi = 0usize;
    let mut ni = 0usize;
    let mut bonus = 0i32;
    let mut prev_match_pos: i32 = -2;
    while hi < h.len() && ni < n.len() {
        if h[hi] == n[ni] {
            bonus += if hi as i32 - prev_match_pos == 1 { 5 } else { 1 };
            let at_word_boundary = if hi == 0 {
                true
            } else {
                matches!(h[hi - 1], ' ' | '-' | '_' | '.' | '/')
            };
            if at_word_boundary {
                bonus += 3;
            }
            prev_match_pos = hi as i32;
            ni += 1;
        }
        hi += 1;
    }

    if ni == n.len() { 100 + bonus } else { -1 }
}

/// Find the first index where `needle` appears contiguously in
/// `haystack`. Both slices are already lowercased by the caller. Pulled
/// out because we work in `Vec<char>` (not `&str`) so `str::find` is
/// not directly available.
fn find_subslice(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    if needle.len() > haystack.len() {
        return None;
    }
    let last_start = haystack.len() - needle.len();
    (0..=last_start).find(|&i| haystack[i..i + needle.len()].iter().eq(needle.iter()))
}

/// Rank `rows` against `needle`. Returns a vector of indices into
/// `rows` sorted by descending score with non-matches dropped. Empty
/// needle returns indices in original order (no filtering, no reorder).
///
/// Mirrors what C# `OnQueryChanged` does to its `_all` list.
pub fn rank(rows: &[impl AsRef<str>], needle: &str) -> Vec<usize> {
    if needle.is_empty() {
        return (0..rows.len()).collect();
    }
    let mut scored: Vec<ScoredIndex> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| ScoredIndex { score: score(r.as_ref(), needle), index: i })
        .filter(|s| s.score >= 0)
        .collect();
    // Stable sort by descending score so ties preserve original order
    // (matches the C# `OrderByDescending` which is also stable).
    scored.sort_by(|a, b| b.score.cmp(&a.score));
    scored.into_iter().map(|s| s.index).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---------- Score (pure ranker) — mirrors C# unit tests ----------

    #[test]
    fn score_empty_needle_returns_zero() {
        assert_eq!(score("hello world", ""), 0);
    }

    #[test]
    fn score_empty_haystack_non_empty_needle_returns_minus_one() {
        assert_eq!(score("", "hi"), -1);
    }

    #[test]
    fn score_prefix_match_gets_highest() {
        let prefix = score("New session", "new");
        let middle = score("Open new tab", "new");
        assert!(prefix > middle);
        // 1000 + 500 prefix bonus
        assert!(prefix >= 1500);
    }

    #[test]
    fn score_substring_match_beats_subsequence_match() {
        let contig = score("Toggle diff panel", "diff");
        let sub = score("Defer initial fetches forever", "diff");
        assert!(contig > sub);
        assert!(contig >= 1000);
    }

    #[test]
    fn score_subsequence_match_returns_at_least_100() {
        let s = score("Reveal in Explorer", "rev");
        assert!(s >= 100);
    }

    #[test]
    fn score_no_match_returns_minus_one() {
        assert_eq!(score("Toggle diff panel", "xyz"), -1);
    }

    #[test]
    fn score_is_case_insensitive() {
        assert!(score("New Session", "NEW") >= 1500);
        assert!(score("new session", "NEW") >= 1500);
    }

    #[test]
    fn score_prefix_substring_beats_later_substring() {
        // "tab" at index 0 of "tabletop" → 1000 + 500 = 1500.
        // "tab" at index 5 of "Next tab" → 1000 + max(0, 200-5) = 1195.
        let prefix = score("tabletop", "tab");
        let later = score("Next tab", "tab");
        assert!(prefix > later);
    }

    #[test]
    fn score_word_boundary_bonus_applies_at_haystack_start() {
        // Subsequence match where the first match sits at the start
        // of the haystack picks up the +3 word-boundary bonus.
        // Greedy matching means subsequent matches land on the first
        // hit of each remaining needle char, so we can't easily build
        // an "after-separator" example without backtracking — but the
        // start-of-string case alone proves the boundary arm runs.
        //
        // "xz" in "xenocrats puzzle":
        //   `x` at hi=0 → +1 (delta 2) +3 (boundary, hi==0) = 4
        //   `z` at hi=12 → +1 (delta 12) → no boundary
        //   total bonus = 5, score = 105.
        //
        // "xz" in "axe puzzle":
        //   `x` at hi=1 → +1 (delta 3) → no boundary
        //   `z` at hi=6 → +1 (delta 5) → no boundary
        //   total bonus = 2, score = 102.
        let with_boundary = score("xenocrats puzzle", "xz");
        let without_boundary = score("axe puzzle", "xz");
        assert_eq!(with_boundary, 105);
        assert_eq!(without_boundary, 102);
        assert!(with_boundary > without_boundary);
    }

    // ---------- rank() ----------

    #[test]
    fn rank_empty_needle_keeps_original_order() {
        let rows = ["alpha", "beta", "gamma"];
        assert_eq!(rank(&rows, ""), vec![0, 1, 2]);
    }

    #[test]
    fn rank_filters_no_match() {
        let rows = ["alpha", "Toggle diff panel", "New session"];
        let order = rank(&rows, "diff");
        // "Toggle diff panel" is the only match.
        assert_eq!(order, vec![1]);
    }

    #[test]
    fn rank_orders_by_score_descending() {
        let rows = ["Defer initial fetches forever", "Toggle diff panel"];
        let order = rank(&rows, "diff");
        // Contiguous substring (index 1) outranks scattered subsequence (index 0).
        assert_eq!(order[0], 1);
        assert!(order.contains(&0));
    }
}
