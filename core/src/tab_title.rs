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

/// Title for a tab restored at startup: `"{project} · {label}"`, the
/// same shape a sidebar worktree click produces.
///
/// `label` is the persisted branch when there is one, else the
/// worktree's folder leaf (else the whole path). The primary checkout
/// persists `branch: None`, so the fallback is common — and it has to
/// keep the project name in front: titling it after the agent made
/// every project's primary checkout come back as `"Claude Code · …"`,
/// and after the git poll swapped in the branch, several identical
/// `"Claude Code · main"` tabs. The fallback keeps the two-segment
/// shape so [`rebuild_title`] swaps the real branch in once the poll
/// reports it.
pub fn restored_title(project: &str, branch: Option<&str>, worktree_path: &str) -> String {
    let label = branch.filter(|b| !b.is_empty()).map(str::to_owned).unwrap_or_else(|| {
        std::path::Path::new(worktree_path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| worktree_path.to_owned())
    });
    format!("{project}{TAB_TITLE_SEPARATOR}{label}")
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
    fn restored_title_leads_with_project_and_branch() {
        assert_eq!(restored_title("acme", Some("feature/x"), "/dev/acme-x"), "acme · feature/x");
    }

    #[test]
    fn restored_title_without_branch_falls_back_to_folder_leaf() {
        // A primary checkout persists `branch: None`. The project name
        // still leads — never the agent name — so two projects that
        // both sit on `main` stay tellable apart.
        assert_eq!(restored_title("acme", None, "/dev/acme-checkout"), "acme · acme-checkout");
    }

    #[test]
    fn restored_title_treats_empty_branch_as_unknown() {
        assert_eq!(restored_title("acme", Some(""), "/dev/acme"), "acme · acme");
    }

    #[test]
    fn restored_title_for_non_git_folder_names_the_folder() {
        // A project without git never reports a branch, so the folder
        // label is permanent — it must be the folder, not an id.
        assert_eq!(restored_title("notes", None, "/dev/notes"), "notes · notes");
    }

    #[test]
    fn restored_title_without_leaf_uses_whole_path() {
        assert_eq!(restored_title("acme", None, "/"), "acme · /");
    }

    #[test]
    fn restored_fallback_title_is_one_the_branch_poll_can_fix() {
        // The folder-leaf fallback only holds until the git poll
        // reports the branch; `rebuild_title` must then produce the
        // same title a sidebar click would.
        let restored = restored_title("acme", None, "/dev/acme");
        assert_eq!(rebuild_title(&restored, "main").as_deref(), Some("acme · main"));
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
