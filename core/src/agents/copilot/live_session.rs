//! Which Copilot CLI sessions the `copilot` running in a tab holds.
//!
//! Copilot CLI marks every session a process has open with
//! `~/.copilot/session-state/<sid>/inuse.<pid>.lock` (holding just the
//! pid), created together with `workspace.yaml` when the session starts
//! — before the first prompt (observed on Copilot CLI 1.0.61). Given the processes running in a
//! tab (see [`crate::process_tree`]), that names the tab's session
//! exactly, the way [`crate::agents::claude::live_session`] does for
//! Claude Code, and it rules out sessions another live `copilot` holds:
//! a neighbour tab's, or one running outside this CodeScope instance.
//!
//! The lock outlives a `copilot` that was killed, so a holder only
//! counts while its pid is still running.

use std::collections::HashSet;
use std::path::Path;
use std::time::SystemTime;

/// Pids named by the `inuse.<pid>.lock` files in `session_dir`. Empty
/// when there are none or the directory can't be read.
pub fn holders(session_dir: &Path) -> Vec<u32> {
    let Ok(entries) = std::fs::read_dir(session_dir) else { return Vec::new() };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name();
            name.to_str()?.strip_prefix("inuse.")?.strip_suffix(".lock")?.parse().ok()
        })
        .collect()
}

/// Who holds a session, judged from its lock files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Holder {
    /// A process running in the tab.
    Tab,
    /// Only live processes outside the tab.
    Other,
    /// No live process — no lock yet, or only stale ones.
    Nobody,
}

/// Classify `holders` against the pids running in the tab (`tab_pids`)
/// and every running pid (`live`).
pub fn classify(holders: &[u32], tab_pids: &[u32], live: &HashSet<u32>) -> Holder {
    if holders.iter().any(|pid| tab_pids.contains(pid)) {
        Holder::Tab
    } else if holders.iter().any(|pid| live.contains(pid)) {
        Holder::Other
    } else {
        Holder::Nobody
    }
}

/// What the lock files say about a tab's adoption candidates.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Ownership {
    /// The newest candidate a process in the tab holds.
    pub exact: Option<String>,
    /// Candidates the tab must not adopt: those only other live
    /// processes hold, and — once `exact` is known — every other one
    /// (a finished `copilot -p` run in the same folder, say, or a
    /// session the tab's own `copilot` has since left). Without an
    /// `exact`, the rest are left to the heuristic.
    pub foreign: Vec<String>,
}

/// Judge each of `hits` — `(session id, mtime)` adoption candidates
/// under `session_state_root` — by its lock files. One `read_dir` per
/// candidate; callers only do this when a tab is about to adopt one.
pub fn ownership(
    session_state_root: &Path,
    hits: &[(String, SystemTime)],
    tab_pids: &[u32],
    live: &HashSet<u32>,
) -> Ownership {
    let mut exact: Option<(SystemTime, &str)> = None;
    let mut others = Vec::new();
    for (sid, mtime) in hits {
        let holder = classify(&holders(&session_state_root.join(sid)), tab_pids, live);
        if holder == Holder::Tab && exact.is_none_or(|(t, _)| *mtime >= t) {
            exact = Some((*mtime, sid));
        }
        others.push((sid, holder));
    }
    let exact = exact.map(|(_, sid)| sid.to_owned());
    let foreign = others
        .into_iter()
        .filter(|(sid, holder)| match &exact {
            Some(own) => *sid != own,
            None => *holder == Holder::Other,
        })
        .map(|(sid, _)| sid.clone())
        .collect();
    Ownership { exact, foreign }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    const SID_A: &str = "aaaaaaaa-0000-0000-0000-000000000001";
    const SID_B: &str = "bbbbbbbb-0000-0000-0000-000000000002";
    const SID_C: &str = "cccccccc-0000-0000-0000-000000000003";

    fn session(root: &Path, sid: &str, pids: &[u32]) {
        let dir = root.join(sid);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("workspace.yaml"), "cwd: C:/repo/alpha\n").unwrap();
        for pid in pids {
            std::fs::write(dir.join(format!("inuse.{pid}.lock")), pid.to_string()).unwrap();
        }
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn live(pids: &[u32]) -> HashSet<u32> {
        pids.iter().copied().collect()
    }

    #[test]
    fn holders_reads_pids_from_lock_file_names_only() {
        let dir = tempfile::tempdir().unwrap();
        session(dir.path(), SID_A, &[27404, 8128]);
        std::fs::write(dir.path().join(SID_A).join("inuse.x.lock"), "").unwrap();
        std::fs::write(dir.path().join(SID_A).join("events.jsonl"), "").unwrap();
        let mut pids = holders(&dir.path().join(SID_A));
        pids.sort_unstable();
        assert_eq!(pids, vec![8128, 27404]);
    }

    #[test]
    fn holders_of_a_missing_directory_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(holders(&dir.path().join(SID_A)).is_empty());
    }

    #[test]
    fn classify_prefers_the_tab_over_other_holders() {
        // A resumed session can carry a stale lock next to the live one.
        assert_eq!(classify(&[5, 12], &[11, 12], &live(&[5, 11, 12])), Holder::Tab);
    }

    #[test]
    fn classify_names_live_holders_outside_the_tab() {
        assert_eq!(classify(&[40], &[11, 12], &live(&[11, 12, 40])), Holder::Other);
    }

    #[test]
    fn classify_ignores_locks_of_dead_processes() {
        // `copilot` killed without cleaning up: the lock stays behind.
        assert_eq!(classify(&[40], &[11, 12], &live(&[11, 12])), Holder::Nobody);
        assert_eq!(classify(&[], &[11, 12], &live(&[11, 12])), Holder::Nobody);
    }

    #[test]
    fn ownership_picks_the_newest_session_the_tab_holds() {
        let dir = tempfile::tempdir().unwrap();
        // The tab's copilot (12) rotated from A to B; C is a neighbour's.
        session(dir.path(), SID_A, &[12]);
        session(dir.path(), SID_B, &[12]);
        session(dir.path(), SID_C, &[40]);
        let hits = vec![
            (SID_B.to_owned(), at(20)),
            (SID_A.to_owned(), at(10)),
            (SID_C.to_owned(), at(30)),
        ];
        let got = ownership(dir.path(), &hits, &[11, 12], &live(&[11, 12, 40]));
        assert_eq!(got.exact.as_deref(), Some(SID_B));
        // The session it left and the neighbour's are both off-limits.
        assert_eq!(got.foreign, vec![SID_A.to_owned(), SID_C.to_owned()]);
    }

    #[test]
    fn ownership_without_an_own_session_only_rules_out_live_holders() {
        let dir = tempfile::tempdir().unwrap();
        session(dir.path(), SID_A, &[]);
        session(dir.path(), SID_C, &[40]);
        let hits = vec![(SID_A.to_owned(), at(10)), (SID_C.to_owned(), at(30))];
        let got = ownership(dir.path(), &hits, &[11, 12], &live(&[11, 12, 40]));
        assert_eq!(got, Ownership { exact: None, foreign: vec![SID_C.to_owned()] });
    }

    #[test]
    fn ownership_with_an_own_session_rules_out_unheld_ones_too() {
        // A finished `copilot -p` in the same folder left SID_A behind
        // with no live holder; the tab's own copilot holds SID_B.
        let dir = tempfile::tempdir().unwrap();
        session(dir.path(), SID_A, &[]);
        session(dir.path(), SID_B, &[12]);
        let hits = vec![(SID_A.to_owned(), at(30)), (SID_B.to_owned(), at(10))];
        let got = ownership(dir.path(), &hits, &[11, 12], &live(&[11, 12]));
        let want = Ownership { exact: Some(SID_B.to_owned()), foreign: vec![SID_A.to_owned()] };
        assert_eq!(got, want);
    }

    #[test]
    fn ownership_leaves_unheld_sessions_to_the_heuristic() {
        let dir = tempfile::tempdir().unwrap();
        // No lock yet (or an older copilot that never writes one), and a
        // stale lock from a dead process: neither exact nor foreign.
        session(dir.path(), SID_A, &[]);
        session(dir.path(), SID_B, &[99]);
        let hits = vec![(SID_A.to_owned(), at(10)), (SID_B.to_owned(), at(20))];
        assert_eq!(ownership(dir.path(), &hits, &[11], &live(&[11])), Ownership::default());
    }
}
