//! Structured tab titles.
//!
//! A tab opened for a project reads `"{project} · {location}"`, where
//! the location is the worktree's branch (or its folder leaf while the
//! branch is unknown), optionally followed by the spawn-time qualifier
//! (`"{project} · {branch} · claude"`). Once the session has a label —
//! the user's rename, else its content title — the tab reads
//! `"{project} · {label}"`.
//!
//! The parts are kept apart and the display string is rendered from
//! them, so a branch change or a new content title replaces one field.
//! Re-parsing the display text instead broke as soon as a project name
//! or folder leaf contained the separator itself (#370):
//! `rename_project` accepts `alpha · beta`.
//!
//! Kept here (not in the gpui crate) so it can be TDD'd without
//! spinning up a window.

use std::fmt;

/// Separator rendered between the parts of a tab title.
pub const TAB_TITLE_SEPARATOR: &str = " · ";

/// The parts a tab title is rendered from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabTitle {
    /// A title outside the project scheme — `"Terminal 3"`, an agent's
    /// display name from the palette. Rendered as is; branch changes and
    /// session labels leave it alone.
    Plain(String),
    /// A tab opened for a project.
    Project {
        /// Project name; always the leading part.
        project: String,
        /// Branch, or the folder leaf while the branch is unknown.
        /// `None` for a tab opened on the project itself, which never
        /// had a location to follow.
        location: Option<String>,
        /// Qualifier picked at spawn (`claude`, `shell`).
        suffix: Option<String>,
        /// The session's label: the user's rename, else its content
        /// title. When set it stands in for location and suffix.
        label: Option<String>,
    },
}

impl TabTitle {
    /// A free-form title the project scheme does not own.
    pub fn plain(text: impl Into<String>) -> Self {
        Self::Plain(text.into())
    }

    /// A tab opened on a project, with no worktree location:
    /// `"{project}"`, or `"{project} · {label}"` once labelled.
    pub fn project(project: impl Into<String>) -> Self {
        Self::Project { project: project.into(), location: None, suffix: None, label: None }
    }

    /// A tab opened on a worktree: `"{project} · {location}"`, the
    /// location being its branch or folder leaf. An empty location is
    /// treated as none.
    pub fn worktree(project: impl Into<String>, location: impl Into<String>) -> Self {
        let location: String = location.into();
        Self::Project {
            project: project.into(),
            location: (!location.is_empty()).then_some(location),
            suffix: None,
            label: None,
        }
    }

    /// Title for a tab restored at startup — the same shape a sidebar
    /// worktree click produces.
    ///
    /// The location is the persisted branch when there is one, else the
    /// worktree's folder leaf (else the whole path). The primary
    /// checkout persists `branch: None`, so the fallback is common — and
    /// it has to keep the project name in front: titling it after the
    /// agent made every project's primary checkout come back as
    /// `"Claude Code · …"`, and after the git poll swapped in the
    /// branch, several identical `"Claude Code · main"` tabs. The
    /// fallback is a location like any other, so [`Self::set_branch`]
    /// swaps the real branch in once the poll reports it.
    pub fn restored(project: &str, branch: Option<&str>, worktree_path: &str) -> Self {
        let location = branch.filter(|b| !b.is_empty()).map(str::to_owned).unwrap_or_else(|| {
            std::path::Path::new(worktree_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| worktree_path.to_owned())
        });
        Self::worktree(project, location)
    }

    /// Append a spawn-time qualifier (`"{project} · {branch} · claude"`).
    /// No effect on a plain title.
    pub fn with_suffix(mut self, suffix: impl Into<String>) -> Self {
        if let Self::Project { suffix: slot, .. } = &mut self {
            *slot = Some(suffix.into());
        }
        self
    }

    /// Seed the session label, e.g. with the title persisted from an
    /// earlier run. `None` or blank leaves the title as it is.
    pub fn with_label(mut self, label: Option<&str>) -> Self {
        if let Some(label) = label {
            self.set_label(label);
        }
        self
    }

    /// The session label currently shown, if any.
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Plain(_) => None,
            Self::Project { label, .. } => label.as_deref(),
        }
    }

    /// Whether a session label can show on this title — every project
    /// tab, never a plain one.
    pub fn takes_label(&self) -> bool {
        matches!(self, Self::Project { .. })
    }

    /// Follow a branch change (the user ran `git checkout other` in the
    /// worktree's pty). Replaces the location; the project and suffix
    /// are untouched whatever they contain.
    ///
    /// Returns whether the parts changed. That is `false` for a blank
    /// branch (detached HEAD — better to keep the old label than render
    /// `"acme ·  · claude"`), an unchanged one, a plain title, and a
    /// project tab that never had a location (its suffix is not a
    /// branch). A label shadows the location, so the parts can change
    /// while the rendered text does not; the branch is kept for when
    /// the label goes away.
    pub fn set_branch(&mut self, branch: &str) -> bool {
        if branch.is_empty() {
            return false;
        }
        match self {
            Self::Project { location: Some(location), .. } if location != branch => {
                *location = branch.to_owned();
                true
            }
            _ => false,
        }
    }

    /// Show `label` (trimmed) after the project, in place of location
    /// and suffix: `"acme · main · claude"` reads `"acme · Resize fix"`.
    /// Returns whether the label changed; `false` for a blank label and
    /// for a plain title.
    pub fn set_label(&mut self, label: &str) -> bool {
        let label = label.trim();
        if label.is_empty() {
            return false;
        }
        match self {
            Self::Project { label: slot, .. } if slot.as_deref() != Some(label) => {
                *slot = Some(label.to_owned());
                true
            }
            _ => false,
        }
    }

    /// Drop the session label, so location and suffix show again.
    /// Returns whether there was one.
    pub fn clear_label(&mut self) -> bool {
        match self {
            Self::Project { label, .. } => label.take().is_some(),
            Self::Plain(_) => false,
        }
    }

    /// The display string for the tab strip.
    pub fn render(&self) -> String {
        match self {
            Self::Plain(text) => text.clone(),
            Self::Project { project, location, suffix, label } => {
                let mut out = project.clone();
                let tail: [Option<&String>; 2] = match label {
                    Some(label) => [Some(label), None],
                    None => [location.as_ref(), suffix.as_ref()],
                };
                for part in tail.into_iter().flatten() {
                    out.push_str(TAB_TITLE_SEPARATOR);
                    out.push_str(part);
                }
                out
            }
        }
    }
}

impl fmt::Display for TabTitle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply a branch change and return the rendered title, or `None`
    /// when nothing changed.
    fn after_branch(mut title: TabTitle, branch: &str) -> Option<String> {
        title.set_branch(branch).then(|| title.render())
    }

    /// Apply a session label and return the rendered title, or `None`
    /// when nothing changed.
    fn after_label(mut title: TabTitle, label: &str) -> Option<String> {
        title.set_label(label).then(|| title.render())
    }

    #[test]
    fn titled_keeps_project_and_drops_branch_and_agent() {
        let title = TabTitle::worktree("acme", "main").with_suffix("Claude Code");
        assert_eq!(title.render(), "acme · main · Claude Code");
        assert_eq!(after_label(title, "Resize fix").as_deref(), Some("acme · Resize fix"));
    }

    #[test]
    fn titled_leaves_renamed_and_unchanged_tabs_alone() {
        assert!(after_label(TabTitle::plain("My custom tab"), "Resize fix").is_none());
        let titled = TabTitle::worktree("acme", "main").with_label(Some("Resize fix"));
        assert!(after_label(titled, "Resize fix").is_none());
        assert!(after_label(TabTitle::worktree("acme", "main"), "  ").is_none());
    }

    #[test]
    fn label_is_trimmed() {
        let mut title = TabTitle::worktree("acme", "main");
        assert!(title.set_label("  Resize fix "));
        assert_eq!(title.label(), Some("Resize fix"));
        assert_eq!(title.render(), "acme · Resize fix");
    }

    #[test]
    fn project_tab_takes_a_label() {
        // Opened on the project itself: no location, but the project
        // prefix still leads the label.
        assert_eq!(
            after_label(TabTitle::project("acme"), "Resize fix").as_deref(),
            Some("acme · Resize fix"),
        );
        assert!(TabTitle::project("acme").takes_label());
        assert!(!TabTitle::plain("Terminal 3").takes_label());
    }

    #[test]
    fn clearing_the_label_brings_the_location_back() {
        // A tab that lets go of a neighbour's transcript loses the
        // label; it must not lose its branch with it.
        let mut title = TabTitle::worktree("acme", "main").with_label(Some("Neighbour"));
        assert!(title.clear_label());
        assert_eq!(title.render(), "acme · main");
        assert!(!title.clear_label());
    }

    #[test]
    fn branch_change_under_a_label_is_kept_for_later() {
        // The label shadows the branch, but the branch still follows
        // checkouts so clearing the label shows the current one.
        let mut title = TabTitle::worktree("acme", "main").with_label(Some("Resize fix"));
        assert!(title.set_branch("feature/x"));
        assert_eq!(title.render(), "acme · Resize fix");
        title.clear_label();
        assert_eq!(title.render(), "acme · feature/x");
    }

    #[test]
    fn swaps_branch_in_two_segment_title() {
        assert_eq!(
            after_branch(TabTitle::worktree("acme", "main"), "feature/x").as_deref(),
            Some("acme · feature/x"),
        );
    }

    #[test]
    fn preserves_agent_suffix() {
        let title = TabTitle::worktree("acme", "main").with_suffix("Claude Code");
        assert_eq!(
            after_branch(title, "feature/x").as_deref(),
            Some("acme · feature/x · Claude Code"),
        );
    }

    #[test]
    fn preserves_multi_segment_agent_suffix() {
        // A suffix that itself contains the separator is still one part.
        let title = TabTitle::worktree("acme", "main").with_suffix("Claude · extra");
        assert_eq!(
            after_branch(title, "feature/x").as_deref(),
            Some("acme · feature/x · Claude · extra"),
        );
    }

    #[test]
    fn single_segment_title_returns_none() {
        // A free-form title has no branch to follow.
        assert!(after_branch(TabTitle::plain("My custom tab"), "feature/x").is_none());
    }

    #[test]
    fn project_tab_does_not_grow_a_branch() {
        // Opened on the project itself (with or without an agent
        // qualifier): there is no branch part to replace, and the
        // qualifier is not one.
        assert!(after_branch(TabTitle::project("acme"), "feature/x").is_none());
        let with_agent = TabTitle::project("acme").with_suffix("claude");
        assert!(after_branch(with_agent, "feature/x").is_none());
    }

    #[test]
    fn empty_branch_returns_none() {
        // Detached HEAD edge case — better to leave the existing
        // label alone than produce `"acme ·  · Claude"`.
        assert!(after_branch(TabTitle::worktree("acme", "main"), "").is_none());
    }

    #[test]
    fn unchanged_title_returns_none() {
        // Avoid spurious re-renders when the poll resurfaces the
        // same branch.
        assert!(after_branch(TabTitle::worktree("acme", "main"), "main").is_none());
        let with_agent = TabTitle::worktree("acme", "main").with_suffix("Claude Code");
        assert!(after_branch(with_agent, "main").is_none());
    }

    #[test]
    fn branch_with_slash_round_trips() {
        // Branch names with `/` are common (`feature/x`, `release/1.2`).
        assert_eq!(
            after_branch(TabTitle::worktree("acme", "main"), "release/1.2").as_deref(),
            Some("acme · release/1.2"),
        );
    }

    #[test]
    fn restored_title_leads_with_project_and_branch() {
        assert_eq!(
            TabTitle::restored("acme", Some("feature/x"), "/dev/acme-x").render(),
            "acme · feature/x",
        );
    }

    #[test]
    fn restored_title_without_branch_falls_back_to_folder_leaf() {
        // A primary checkout persists `branch: None`. The project name
        // still leads — never the agent name — so two projects that
        // both sit on `main` stay tellable apart.
        assert_eq!(
            TabTitle::restored("acme", None, "/dev/acme-checkout").render(),
            "acme · acme-checkout",
        );
    }

    #[test]
    fn restored_title_treats_empty_branch_as_unknown() {
        assert_eq!(TabTitle::restored("acme", Some(""), "/dev/acme").render(), "acme · acme");
    }

    #[test]
    fn restored_title_for_non_git_folder_names_the_folder() {
        // A project without git never reports a branch, so the folder
        // label is permanent — it must be the folder, not an id.
        assert_eq!(TabTitle::restored("notes", None, "/dev/notes").render(), "notes · notes");
    }

    #[test]
    fn restored_title_without_leaf_uses_whole_path() {
        assert_eq!(TabTitle::restored("acme", None, "/").render(), "acme · /");
    }

    #[test]
    fn restored_fallback_title_is_one_the_branch_poll_can_fix() {
        // The folder-leaf fallback only holds until the git poll
        // reports the branch; the branch change must then produce the
        // same title a sidebar click would.
        let restored = TabTitle::restored("acme", None, "/dev/acme");
        assert_eq!(after_branch(restored, "main").as_deref(), Some("acme · main"));
    }

    #[test]
    fn project_with_separator_like_chars_preserved() {
        // Project names are user-chosen (`rename_project` accepts any
        // non-blank name), so the project part must round-trip byte
        // for byte whatever it contains.
        assert_eq!(
            after_branch(TabTitle::worktree("a-b", "main"), "feature/x").as_deref(),
            Some("a-b · feature/x"),
        );
    }

    #[test]
    fn project_containing_separator_survives_branch_change() {
        // #370: re-parsing `alpha · beta · main` swapped the second
        // half of the project name for the branch.
        let title = TabTitle::worktree("alpha · beta", "main");
        assert_eq!(title.render(), "alpha · beta · main");
        assert_eq!(
            after_branch(title, "feature/x").as_deref(),
            Some("alpha · beta · feature/x"),
        );
    }

    #[test]
    fn project_containing_separator_survives_content_title() {
        // #370: keeping "everything before the first separator" cut
        // the project name down to `alpha`.
        let title = TabTitle::worktree("alpha · beta", "main");
        assert_eq!(
            after_label(title, "Resize fix").as_deref(),
            Some("alpha · beta · Resize fix"),
        );
    }

    #[test]
    fn project_containing_separator_survives_branch_then_label() {
        let mut title = TabTitle::restored("alpha · beta", None, "/dev/alpha");
        assert!(title.set_branch("main"));
        assert!(title.set_label("Resize fix"));
        assert!(title.set_branch("feature/x"));
        assert_eq!(title.render(), "alpha · beta · Resize fix");
        assert!(title.clear_label());
        assert_eq!(title.render(), "alpha · beta · feature/x");
    }

    #[test]
    fn folder_leaf_containing_separator_is_replaced_whole() {
        // The folder leaf stands in for the branch until the poll
        // reports one; a leaf with the separator in it must be
        // replaced as a whole, not segment by segment.
        let title = TabTitle::restored("acme", None, "/dev/x · y");
        assert_eq!(title.render(), "acme · x · y");
        assert_eq!(after_branch(title, "main").as_deref(), Some("acme · main"));
    }
}
