//! Content-based session titles.
//!
//! Agents name their own sessions: Claude Code appends `ai-title`
//! entries (and `custom-title` on `/rename`) to its transcript,
//! Copilot CLI writes `name:` into `workspace.yaml`. Agents that never
//! name a session still record the prompt that started it. Each
//! telemetry tail folds what it sees into one [`SessionTitle`] via
//! [`SessionTitle::offer`]; the shell shows it on the sidebar's session
//! rows and in the tab strip, and persists it on
//! [`crate::projects::Session::title`] so closed history rows keep it.
//!
//! A title the user typed in CodeScope's own Rename dialog
//! (`Session.display_name`) always wins over all of this — see
//! [`session_label`].

/// Where a title came from. Ordered weakest → strongest: a stronger
/// source replaces a weaker one, never the other way round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TitleSource {
    /// The session's first prompt, trimmed to a line. A stand-in until
    /// the agent names the session (or for agents that never do).
    Prompt,
    /// A title the agent generated from the conversation.
    Generated,
    /// A title the user gave inside the agent (`/rename`, Copilot's
    /// `user_named`).
    User,
}

/// A session's best-known title and its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTitle {
    pub text: String,
    pub source: TitleSource,
}

/// Longest title kept, in chars. Agent titles are short; this only
/// bites on prompt fallbacks, which can be pages long.
pub const MAX_TITLE_CHARS: usize = 80;

impl SessionTitle {
    /// Fold a candidate title into `current`. Returns `true` when
    /// `current` changed.
    ///
    /// * A [`TitleSource::Prompt`] only fills an empty slot: the first
    ///   prompt names the session, later prompts don't rename it.
    /// * A generated or user title replaces anything of equal or lower
    ///   rank — agents re-title as the conversation moves on, and the
    ///   latest one is the one they show themselves.
    ///
    /// The candidate is normalised with [`normalize_title`] first; one
    /// that normalises to nothing is ignored.
    pub fn offer(current: &mut Option<SessionTitle>, raw: &str, source: TitleSource) -> bool {
        let Some(text) = normalize_title(raw) else {
            return false;
        };
        let replace = match current {
            None => true,
            Some(_) if source == TitleSource::Prompt => false,
            Some(cur) => source >= cur.source && cur.text != text,
        };
        if replace {
            *current = Some(SessionTitle { text, source });
        }
        replace
    }
}

/// Reduce raw title text to one display line: the first non-blank
/// line, inner whitespace collapsed, capped at [`MAX_TITLE_CHARS`]
/// with a trailing `…`. `None` when nothing printable is left.
pub fn normalize_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let collapsed: String = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= MAX_TITLE_CHARS {
        return Some(collapsed);
    }
    let mut out: String = collapsed.chars().take(MAX_TITLE_CHARS - 1).collect();
    out.truncate(out.trim_end().len());
    out.push('…');
    Some(out)
}

/// The label a session shows in the sidebar: the user's rename, else
/// the content title, else the agent's display name, else `"shell"`.
pub fn session_label(
    display_name: Option<&str>,
    title: Option<&str>,
    agent_label: Option<&str>,
) -> String {
    [display_name, title, agent_label]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
        .unwrap_or("shell")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer(cur: &mut Option<SessionTitle>, raw: &str, source: TitleSource) -> bool {
        SessionTitle::offer(cur, raw, source)
    }

    #[test]
    fn prompt_fills_an_empty_slot() {
        let mut t = None;
        assert!(offer(&mut t, "fix the resize bug", TitleSource::Prompt));
        assert_eq!(t.unwrap().text, "fix the resize bug");
    }

    #[test]
    fn later_prompt_does_not_rename() {
        let mut t = None;
        offer(&mut t, "first", TitleSource::Prompt);
        assert!(!offer(&mut t, "second", TitleSource::Prompt));
        assert_eq!(t.unwrap().text, "first");
    }

    #[test]
    fn generated_replaces_prompt_and_older_generated() {
        let mut t = None;
        offer(&mut t, "first prompt", TitleSource::Prompt);
        assert!(offer(&mut t, "Resize bug", TitleSource::Generated));
        assert!(offer(&mut t, "Terminal resize fix", TitleSource::Generated));
        assert_eq!(t.unwrap().text, "Terminal resize fix");
    }

    #[test]
    fn generated_never_replaces_user_title() {
        let mut t = None;
        offer(&mut t, "Mine", TitleSource::User);
        assert!(!offer(&mut t, "Agent's", TitleSource::Generated));
        assert!(!offer(&mut t, "prompt", TitleSource::Prompt));
        assert_eq!(t.unwrap().text, "Mine");
    }

    #[test]
    fn same_title_again_is_not_a_change() {
        let mut t = None;
        offer(&mut t, "Same", TitleSource::Generated);
        assert!(!offer(&mut t, "Same", TitleSource::Generated));
    }

    #[test]
    fn blank_candidate_is_ignored() {
        let mut t = None;
        assert!(!offer(&mut t, "  \n\t ", TitleSource::Generated));
        assert!(t.is_none());
    }

    #[test]
    fn normalize_takes_first_line_and_collapses_whitespace() {
        assert_eq!(
            normalize_title("\n  fix   the\tbug  \nsecond line").as_deref(),
            Some("fix the bug")
        );
    }

    #[test]
    fn normalize_caps_long_text_with_ellipsis() {
        let long = "word ".repeat(40);
        let out = normalize_title(&long).unwrap();
        assert_eq!(out.chars().count(), MAX_TITLE_CHARS);
        assert!(out.ends_with('…'));
        assert!(!out.ends_with(" …"));
    }

    #[test]
    fn normalize_counts_chars_not_bytes() {
        let s = "é".repeat(MAX_TITLE_CHARS);
        assert_eq!(normalize_title(&s).as_deref(), Some(s.as_str()));
    }

    #[test]
    fn label_prefers_rename_then_title_then_agent() {
        assert_eq!(session_label(Some("Mine"), Some("Title"), Some("Claude Code")), "Mine");
        assert_eq!(session_label(None, Some("Title"), Some("Claude Code")), "Title");
        assert_eq!(session_label(None, None, Some("Claude Code")), "Claude Code");
        assert_eq!(session_label(None, None, None), "shell");
    }

    #[test]
    fn label_skips_blank_values() {
        assert_eq!(session_label(Some(" "), Some(""), Some("Pi")), "Pi");
    }
}
