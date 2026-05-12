//! Pure helpers for the tab-title string format.
//!
//! Tabs that target a worktree carry a title of the form
//! `"{project} · {branch}"` or `"{project} · {branch} · {agent}"`.
//! When the branch on disk changes (the user runs `git checkout other`
//! inside the worktree's pty), the tab strip has to follow — the
//! poller picks up the new branch and the host calls
//! [`rebuild_title`] to compute the rewritten label.
//!
//! Mirrors C# `MainViewModel.RefreshTabTitlesForWorktree` which
//! recomputes `"{project.Name} · {branch}"` on every
//! `WorktreeStatusUpdated` store change.
//!
//! Kept here (not in the gpui crate) so it can be TDD'd without
//! spinning up a window.

/// Separator used between segments in a tab title.
pub const TAB_TITLE_SEPARATOR: &str = " · ";

/// Rebuild a tab title so the branch segment reflects `new_branch`.
///
/// The title is expected to look like one of:
///
/// * `"{project} · {branch}"` — worktree tab, no agent
/// * `"{project} · {branch} · {agent}"` — worktree tab + agent suffix
///
/// Strategy:
///
/// * Split on `" · "` ([`TAB_TITLE_SEPARATOR`]).
/// * If there are fewer than 2 segments, the title isn't in the
///   `Project · Branch` shape we own — return `None` so the caller
///   leaves it untouched (typical case: user-renamed tab).
/// * Otherwise replace segment index `1` with `new_branch` and rejoin.
///
/// Returns `None` when:
///
/// * `new_branch` is empty (rebuilding to a blank branch would
///   produce a misleading `"acme ·  · Claude"` label).
/// * `current_title` has fewer than 2 ` · `-separated segments.
/// * The rebuilt title equals the input (no-op — saves a render).
///
/// This is intentionally syntactic, not semantic — it doesn't try to
/// recognise a "project name" vs "branch name". Anything that already
/// has the two-segment shape gets the middle slot swapped. Tabs the
/// user explicitly renamed via the Rename dialog should be filtered
/// upstream (the host checks `Session.display_name` before calling
/// this), but a renamed tab whose new name happens to contain no
/// ` · ` separator is also protected here by the segment-count guard.
pub fn rebuild_title(current_title: &str, new_branch: &str) -> Option<String> {
    if new_branch.is_empty() {
        return None;
    }
    let segments: Vec<&str> = current_title.split(TAB_TITLE_SEPARATOR).collect();
    if segments.len() < 2 {
        return None;
    }
    let mut out = String::with_capacity(current_title.len() + new_branch.len());
    for (i, seg) in segments.iter().enumerate() {
        if i > 0 {
            out.push_str(TAB_TITLE_SEPARATOR);
        }
        if i == 1 {
            out.push_str(new_branch);
        } else {
            out.push_str(seg);
        }
    }
    if out == current_title {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swaps_branch_in_two_segment_title() {
        assert_eq!(
            rebuild_title("acme · main", "feature/x").as_deref(),
            Some("acme · feature/x"),
        );
    }

    #[test]
    fn preserves_agent_suffix() {
        assert_eq!(
            rebuild_title("acme · main · Claude Code", "feature/x").as_deref(),
            Some("acme · feature/x · Claude Code"),
        );
    }

    #[test]
    fn preserves_multi_segment_agent_suffix() {
        // Defensive: more than three segments still only swaps slot 1.
        assert_eq!(
            rebuild_title("acme · main · Claude · extra", "feature/x").as_deref(),
            Some("acme · feature/x · Claude · extra"),
        );
    }

    #[test]
    fn single_segment_title_returns_none() {
        // A renamed tab without our standard separator must not be
        // mutated — the host's display_name check is the primary
        // guard, this is the belt-and-braces fallback.
        assert!(rebuild_title("My custom tab", "feature/x").is_none());
    }

    #[test]
    fn empty_branch_returns_none() {
        // Detached HEAD edge case — better to leave the existing
        // label alone than produce `"acme ·  · Claude"`.
        assert!(rebuild_title("acme · main", "").is_none());
    }

    #[test]
    fn unchanged_title_returns_none() {
        // Avoid spurious re-renders when the poll resurfaces the
        // same branch.
        assert!(rebuild_title("acme · main", "main").is_none());
        assert!(rebuild_title("acme · main · Claude Code", "main").is_none());
    }

    #[test]
    fn branch_with_slash_round_trips() {
        // Branch names with `/` are common (`feature/x`, `release/1.2`).
        assert_eq!(
            rebuild_title("acme · main", "release/1.2").as_deref(),
            Some("acme · release/1.2"),
        );
    }

    #[test]
    fn project_with_separator_like_chars_preserved() {
        // Project names can't actually contain our separator
        // (we control where it gets inserted), but a defensive
        // assertion that slot 0 round-trips byte-for-byte.
        assert_eq!(
            rebuild_title("a-b · main", "feature/x").as_deref(),
            Some("a-b · feature/x"),
        );
    }
}
