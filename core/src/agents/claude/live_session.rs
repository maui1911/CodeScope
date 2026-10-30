//! Which Claude Code session a running `claude` process is in.
//!
//! Claude Code keeps one `~/.claude/sessions/<pid>.json` per running
//! interactive process, holding the id of the conversation it is in
//! right now (rewritten when `/clear` rotates it) and its cwd. Given
//! the processes running in a tab (see [`crate::process_tree`]), that
//! names the tab's transcript exactly — unlike the "newest transcript
//! in this folder" heuristic in [`super::discovery`], which hands two
//! tabs sharing a worktree each other's conversations when they start
//! close together, and can take a transcript from a `claude` running
//! outside CodeScope altogether.

use std::path::{Path, PathBuf};

/// `~/.claude/sessions`.
pub fn default_sessions_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".claude").join("sessions"))
}

/// The session the first of `pids` that is a Claude Code process
/// running in `working_directory` is in, or `None` when none is (no
/// `claude` started yet, or a Claude Code too old to write the file).
///
/// The cwd check guards against a stale file whose pid the OS has since
/// handed to an unrelated process in the tab.
pub fn session_for_pids(
    sessions_dir: &Path,
    pids: &[u32],
    working_directory: &str,
) -> Option<String> {
    pids.iter().find_map(|&pid| {
        let text = std::fs::read_to_string(sessions_dir.join(format!("{pid}.json"))).ok()?;
        let entry: serde_json::Value = serde_json::from_str(&text).ok()?;
        let same_pid = entry.get("pid").and_then(serde_json::Value::as_u64) == Some(pid.into());
        let cwd = entry.get("cwd").and_then(serde_json::Value::as_str)?;
        let session_id = entry.get("sessionId").and_then(serde_json::Value::as_str)?;
        (same_pid
            && !session_id.is_empty()
            && crate::path_canon::paths_match(cwd, working_directory))
        .then(|| session_id.to_owned())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, pid: u32, body: &str) {
        std::fs::write(dir.join(format!("{pid}.json")), body).unwrap();
    }

    fn entry(pid: u32, sid: &str, cwd: &str) -> String {
        serde_json::json!({ "pid": pid, "sessionId": sid, "cwd": cwd, "kind": "interactive" })
            .to_string()
    }

    #[test]
    fn finds_the_session_of_a_claude_pid_in_the_tab() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), 12, &entry(12, "sid-a", r"C:\repo\alpha"));
        // 11 (node / a shim) has no file; 12 is the claude process.
        assert_eq!(
            session_for_pids(dir.path(), &[11, 12], "C:/repo/alpha").as_deref(),
            Some("sid-a")
        );
    }

    #[test]
    fn none_when_no_pid_has_a_session_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(session_for_pids(dir.path(), &[11, 12], "C:/repo"), None);
    }

    #[test]
    fn ignores_a_file_for_another_directory() {
        // Stale file from a dead claude whose pid was reused.
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), 12, &entry(12, "sid-a", r"C:\other"));
        assert_eq!(session_for_pids(dir.path(), &[12], "C:/repo"), None);
    }

    #[test]
    fn ignores_malformed_or_mismatched_files() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), 12, "{not json");
        write(dir.path(), 13, &entry(99, "sid-b", "C:/repo"));
        write(dir.path(), 14, &entry(14, "", "C:/repo"));
        assert_eq!(session_for_pids(dir.path(), &[12, 13, 14], "C:/repo"), None);
    }
}
