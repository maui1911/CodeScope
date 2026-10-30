//! Which processes run inside a terminal tab.
//!
//! A tab only knows the pid of its shell; the agent CLI is a child (or
//! grandchild, behind `node` / a `.cmd` shim) of that shell. Agents that
//! record their session per pid (Claude Code's
//! `~/.claude/sessions/<pid>.json`) can then be matched to the exact tab
//! they run in — see [`crate::agents::claude::live_session`].

use std::collections::{HashMap, HashSet};

/// One `(pid, parent pid)` pair per running process.
pub type ProcessTable = Vec<(u32, u32)>;

/// Every descendant of `root` in `table`, breadth-first (children
/// before grandchildren). `root` itself is not included.
pub fn descendants(root: u32, table: &[(u32, u32)]) -> Vec<u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for &(pid, ppid) in table {
        // The idle / system process is its own parent on Windows.
        if pid != ppid {
            children.entry(ppid).or_default().push(pid);
        }
    }
    let mut seen = HashSet::from([root]);
    let mut out = Vec::new();
    let mut frontier = vec![root];
    while !frontier.is_empty() {
        let mut next = Vec::new();
        for parent in frontier {
            for &child in children.get(&parent).into_iter().flatten() {
                // A recycled parent pid can make the table cyclic.
                if seen.insert(child) {
                    out.push(child);
                    next.push(child);
                }
            }
        }
        frontier = next;
    }
    out
}

/// Snapshot of the running processes. Empty when the platform query
/// fails — callers treat that as "nothing known", never as an error.
#[cfg(windows)]
pub fn process_table() -> ProcessTable {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };

    let mut out = Vec::new();
    // SAFETY: the snapshot handle is checked, only read through the
    // ToolHelp calls below, and closed before returning.
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
            return out;
        };
        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };
        let mut more = Process32FirstW(snapshot, &mut entry).is_ok();
        while more {
            out.push((entry.th32ProcessID, entry.th32ParentProcessID));
            more = Process32NextW(snapshot, &mut entry).is_ok();
        }
        let _ = CloseHandle(snapshot);
    }
    out
}

/// Snapshot of the running processes. Empty when the platform query
/// fails — callers treat that as "nothing known", never as an error.
#[cfg(not(windows))]
pub fn process_table() -> ProcessTable {
    crate::process::no_window_command("ps")
        .args(["-A", "-o", "pid=", "-o", "ppid="])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| parse_ps(&String::from_utf8_lossy(&o.stdout)))
        .unwrap_or_default()
}

/// Parse `ps -o pid= -o ppid=` output: two numbers per line.
#[cfg_attr(windows, allow(dead_code))]
fn parse_ps(text: &str) -> ProcessTable {
    text.lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace().map(str::parse::<u32>);
            Some((cols.next()?.ok()?, cols.next()?.ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descendants_walks_children_before_grandchildren() {
        // shell 10 → node 11 → claude 12; shell 10 → git 13; unrelated 20.
        let table = vec![(10, 1), (11, 10), (12, 11), (13, 10), (20, 1)];
        assert_eq!(descendants(10, &table), vec![11, 13, 12]);
    }

    #[test]
    fn descendants_of_a_leaf_is_empty() {
        assert!(descendants(12, &[(12, 11)]).is_empty());
    }

    #[test]
    fn descendants_survives_cycles_from_recycled_pids() {
        let table = vec![(2, 1), (1, 2), (0, 0)];
        assert_eq!(descendants(1, &table), vec![2]);
        assert!(descendants(0, &table).is_empty());
    }

    #[test]
    fn parse_ps_reads_pid_ppid_pairs_and_skips_junk() {
        let text = "    1     0\n  412     1\nnot a row\n  413\n";
        assert_eq!(parse_ps(text), vec![(1, 0), (412, 1)]);
    }

    #[test]
    fn own_process_table_contains_this_process() {
        let me = std::process::id();
        assert!(process_table().iter().any(|&(pid, _)| pid == me));
    }
}
