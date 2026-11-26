//! The agent's reply to a review outside its review threads: read from [`REPLY_FILE`], checked
//! the way findings are, and turned into the comment CodeScope posts on the pull request.

use std::path::Path;

use super::dispatch::REPLY_FILE;
use super::findings::{
    HandoverFile, MAX_LENGTH, Problem, fence_problems, is_marker_line, normalised, problems_list,
    quiet, read_handover,
};

const MARKER: &str = "<!-- dispatch v=1 phase=implementation outcome=reply by=codescope -->";

/// [`REPLY_FILE`] in `worktree`, `None` without one; read with the same guards as the findings.
pub fn read_reply(worktree: &Path) -> Option<HandoverFile> {
    read_handover(worktree, REPLY_FILE)
}

impl HandoverFile {
    /// What [`reply_body`] makes of the file, or why it can't be read.
    pub fn reply(&self) -> Result<Option<String>, Vec<Problem>> {
        let text = self.read.as_ref().map_err(|problem| vec![problem.clone()])?;
        reply_body(text)
    }
}

/// The reply `text` as a quiet comment with a `**Next step:**` line and the marker; `None` when
/// blank. Refuses a marker of its own, more than a comment holds, and an unclosed code block.
pub fn reply_body(text: &str) -> Result<Option<String>, Vec<Problem>> {
    let text = normalised(text);
    if text.is_empty() {
        return Ok(None);
    }
    let quiet = quiet(&text);
    let mut problems = Vec::new();
    let length = quiet.chars().count();
    if length > MAX_LENGTH {
        problems.push(Problem::TooLong(length));
    }
    if text.lines().any(is_marker_line) {
        problems.push(Problem::Marker);
    }
    problems.extend(fence_problems(&text));
    if !problems.is_empty() {
        return Err(problems);
    }
    Ok(Some(format!(
        "{quiet}\n\n**Next step:** review the pull request again.\n{MARKER}\n"
    )))
}

/// One line for the session that wrote the reply, saying what to fix, with nothing from the
/// file in it.
pub fn reply_problems_message(problems: &[Problem]) -> String {
    format!(
        "CodeScope cannot post {REPLY_FILE} yet: {}. Fix the file and stop.",
        problems_list(problems)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_to_the_session_names_the_reply_file() {
        assert_eq!(
            reply_problems_message(&[Problem::OpenFence]),
            "CodeScope cannot post .codescope/reply.md yet: a code block is not closed. Fix the \
             file and stop."
        );
    }

    #[test]
    fn a_reply_is_posted_quiet_with_a_next_step_and_the_marker() {
        let text = "\n\nThanks @sanne, I kept the timeout: see #12.\n\n";
        let body = reply_body(text).unwrap().unwrap();
        assert_eq!(
            body,
            format!(
                "Thanks @\u{200b}sanne, I kept the timeout: see #12.\n\n\
                 **Next step:** review the pull request again.\n{MARKER}\n"
            )
        );
    }

    #[test]
    fn a_blank_reply_is_no_reply() {
        assert_eq!(reply_body(" \n\t\n"), Ok(None));
        assert_eq!(reply_body(""), Ok(None));
    }

    #[test]
    fn a_reply_that_would_break_the_comment_is_refused() {
        assert_eq!(reply_body("Done.\n<!-- dispatch v=1 -->"), Err(vec![Problem::Marker]));
        assert_eq!(reply_body("Ran:\n\n```sh\ncargo test\n"), Err(vec![Problem::OpenFence]));
        let long = "word ".repeat(13_000);
        assert!(matches!(reply_body(&long).unwrap_err()[..], [Problem::TooLong(_)]));
    }

    #[test]
    fn decision_point_headings_mean_nothing_in_a_reply() {
        let text = "## B1 Which timeout\n\n### B2 Kept\n\nSee B1.";
        assert!(reply_body(text).unwrap().is_some());
    }

    #[test]
    fn reads_the_reply_in_the_worktree() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_reply(dir.path()).is_none());
        let path = dir.path().join(REPLY_FILE);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "Kept it.\r\n").unwrap();
        let file = read_reply(dir.path()).unwrap();
        assert_eq!(file.reply().unwrap().unwrap(), reply_body("Kept it.").unwrap().unwrap());
    }
}
