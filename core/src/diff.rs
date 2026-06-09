//! Working-tree diff model for the in-app diff viewer.
//!
//! Two halves:
//!
//! * A pure unified-diff parser ([`parse_unified_diff`]) that turns
//!   `git diff` output into files → hunks → lines, with per-line old /
//!   new line numbers and intraline change emphasis so the renderer
//!   can paint pixel-level highlights without re-deriving anything.
//! * A collector ([`worktree_diff`]) that shells out to `git` (per the
//!   project rule: no libgit2) and folds untracked files in as
//!   synthetic all-added entries, because "what changed in this
//!   worktree" includes the file you just created.
//!
//! The parser is deliberately tolerant: anything it doesn't recognise
//! inside a file section (mode lines, index lines, `\ No newline at
//! end of file`) is skipped rather than failing the whole diff — a
//! viewer that drops one exotic header is useful, one that errors on
//! it is not.

use std::path::Path;

use anyhow::{Context as _, Result};

use crate::git::run_git;

/// Hard cap on synthesized untracked-file content, in bytes. Untracked
/// files are read straight from disk; without a cap a stray multi-MB
/// log file would balloon the snapshot and the render tree.
const MAX_UNTRACKED_BYTES: u64 = 262_144;

/// Hard cap on synthesized untracked-file content, in lines. Same
/// rationale as [`MAX_UNTRACKED_BYTES`].
const MAX_UNTRACKED_LINES: usize = 5_000;

/// How a file changed relative to `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
    /// Not known to git at all — synthesized from the working tree.
    Untracked,
}

impl FileStatus {
    /// Single-letter badge for list rows (`A`/`M`/`D`/`R`/`U`).
    pub fn badge(self) -> &'static str {
        match self {
            FileStatus::Added => "A",
            FileStatus::Modified => "M",
            FileStatus::Deleted => "D",
            FileStatus::Renamed => "R",
            FileStatus::Untracked => "U",
        }
    }
}

/// One changed file: identity plus its hunks and roll-up counts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffFile {
    /// New path, slash-separated as git emits it.
    pub path: String,
    /// Previous path when `status == Renamed`.
    pub old_path: Option<String>,
    pub status: FileStatus,
    /// Binary change — no hunks, the viewer shows a placeholder.
    pub binary: bool,
    pub hunks: Vec<DiffHunk>,
    /// Total added lines across hunks.
    pub added: u32,
    /// Total removed lines across hunks.
    pub removed: u32,
    /// True when content was cut off at a display cap (untracked files
    /// larger than [`MAX_UNTRACKED_BYTES`] / [`MAX_UNTRACKED_LINES`]).
    pub truncated: bool,
}

/// One `@@`-delimited hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    /// The raw `@@ -a,b +c,d @@ context` line, for display.
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

/// One line inside a hunk, prefix stripped, with the line numbers it
/// occupies on each side (`None` on the side it doesn't exist on).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
    /// Char range (`start..end` into `text`'s chars) that actually
    /// changed, for paired removed/added lines — the renderer paints
    /// this span with a stronger tint. `None` = whole-line change.
    pub emphasis: Option<(usize, usize)>,
}

/// Parse `git diff` unified output (`--no-color`) into [`DiffFile`]s.
/// Pure and total: unknown lines are skipped, an empty input yields an
/// empty vec.
pub fn parse_unified_diff(input: &str) -> Vec<DiffFile> {
    let mut files: Vec<DiffFile> = Vec::new();
    // Running (old, new) line numbers for the hunk currently being
    // filled — reset by every `@@` header. Kept outside the hunk so
    // appending a line is O(1) instead of re-counting the hunk.
    let mut counters: (u32, u32) = (0, 0);

    for line in input.lines() {
        let line = line.strip_suffix('\r').unwrap_or(line);

        if let Some(rest) = line.strip_prefix("diff --git ") {
            files.push(DiffFile {
                path: paths_from_diff_git(rest).1,
                old_path: None,
                status: FileStatus::Modified,
                binary: false,
                hunks: Vec::new(),
                added: 0,
                removed: 0,
                truncated: false,
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };

        if let Some(hunk) = file.hunks.last_mut()
            && !line.starts_with("@@ ")
        {
            // Inside a hunk body every line starts with '+', '-',
            // ' ' or '\' — anything else ends the hunk section and
            // falls through to header handling below.
            match line.as_bytes().first() {
                Some(b'+') => {
                    push_hunk_line(hunk, LineKind::Added, &line[1..], &mut counters);
                    file.added += 1;
                    continue;
                }
                Some(b'-') => {
                    push_hunk_line(hunk, LineKind::Removed, &line[1..], &mut counters);
                    file.removed += 1;
                    continue;
                }
                Some(b' ') => {
                    push_hunk_line(hunk, LineKind::Context, &line[1..], &mut counters);
                    continue;
                }
                // `\ No newline at end of file` — metadata, not text.
                Some(b'\\') => continue,
                // Empty context line: git emits a fully blank line for
                // a context line whose content is empty.
                None => {
                    push_hunk_line(hunk, LineKind::Context, "", &mut counters);
                    continue;
                }
                _ => {}
            }
        }

        if let Some(header) = line.strip_prefix("@@ ") {
            if let Some((old_start, new_start)) = parse_hunk_header(header) {
                counters = (old_start, new_start);
                file.hunks.push(DiffHunk {
                    header: line.to_string(),
                    old_start,
                    new_start,
                    lines: Vec::new(),
                });
            }
        } else if line.starts_with("new file mode") {
            file.status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            file.status = FileStatus::Deleted;
        } else if let Some(from) = line.strip_prefix("rename from ") {
            file.status = FileStatus::Renamed;
            file.old_path = Some(from.to_string());
        } else if let Some(to) = line.strip_prefix("rename to ") {
            file.status = FileStatus::Renamed;
            file.path = to.to_string();
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            file.binary = true;
        } else if let Some(new_path) = line.strip_prefix("+++ ") {
            // `+++ b/<path>` is the most reliable path source (the
            // `diff --git` split is ambiguous for paths containing
            // spaces). `/dev/null` means deletion — keep the `---`
            // side's path instead.
            if let Some(p) = strip_diff_path(new_path, "b/") {
                file.path = p;
            }
        } else if let Some(old_path) = line.strip_prefix("--- ")
            && let Some(p) = strip_diff_path(old_path, "a/")
        {
            if file.status == FileStatus::Deleted {
                file.path = p;
            } else if file.status == FileStatus::Renamed && file.old_path.is_none() {
                file.old_path = Some(p);
            }
        }
    }

    for file in &mut files {
        apply_intraline_emphasis(file);
    }
    files
}

/// Append a line to `hunk`, consuming line numbers from the parser's
/// running `(old, new)` counters (reset by each `@@` header). O(1)
/// per line — re-deriving the numbers from the hunk contents would
/// make parsing quadratic on large hunks.
fn push_hunk_line(
    hunk: &mut DiffHunk,
    kind: LineKind,
    text: &str,
    counters: &mut (u32, u32),
) {
    let (old_no, new_no) = match kind {
        LineKind::Context => {
            let nos = (Some(counters.0), Some(counters.1));
            counters.0 += 1;
            counters.1 += 1;
            nos
        }
        LineKind::Added => {
            let nos = (None, Some(counters.1));
            counters.1 += 1;
            nos
        }
        LineKind::Removed => {
            let nos = (Some(counters.0), None);
            counters.0 += 1;
            nos
        }
    };
    hunk.lines.push(DiffLine {
        kind,
        old_no,
        new_no,
        text: text.to_string(),
        emphasis: None,
    });
}

/// Parse `-<old>[,<n>] +<new>[,<m>] @@ ...` (the part after `@@ `).
fn parse_hunk_header(header: &str) -> Option<(u32, u32)> {
    let mut parts = header.split(' ');
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    let old_start = old.split(',').next()?.parse().ok()?;
    let new_start = new.split(',').next()?.parse().ok()?;
    Some((old_start, new_start))
}

/// Strip the `a/` / `b/` prefix (and optional quoting) from a
/// `---` / `+++` path. Returns `None` for `/dev/null`.
fn strip_diff_path(raw: &str, prefix: &str) -> Option<String> {
    let raw = raw.trim_end();
    let raw = raw.strip_prefix('"').unwrap_or(raw);
    let raw = raw.strip_suffix('"').unwrap_or(raw);
    if raw == "/dev/null" {
        return None;
    }
    Some(raw.strip_prefix(prefix).unwrap_or(raw).to_string())
}

/// Best-effort path split of the `a/<p> b/<p>` tail of a `diff --git`
/// line. Ambiguous for paths with spaces — the `+++` / `---` handlers
/// overwrite with the authoritative value when those lines arrive.
fn paths_from_diff_git(rest: &str) -> (String, String) {
    let rest = rest.trim();
    if let Some(idx) = rest.find(" b/") {
        let old = rest[..idx].strip_prefix("a/").unwrap_or(&rest[..idx]);
        let new = &rest[idx + 3..];
        return (old.to_string(), new.to_string());
    }
    (rest.to_string(), rest.to_string())
}

/// Compute the changed char span between two versions of a line by
/// trimming the common prefix and suffix. Returns `(old_span, new_span)`
/// as char ranges, or `None` when the lines are identical.
pub fn intraline_emphasis(old: &str, new: &str) -> Option<((usize, usize), (usize, usize))> {
    if old == new {
        return None;
    }
    let old_chars: Vec<char> = old.chars().collect();
    let new_chars: Vec<char> = new.chars().collect();

    let mut prefix = 0;
    while prefix < old_chars.len()
        && prefix < new_chars.len()
        && old_chars[prefix] == new_chars[prefix]
    {
        prefix += 1;
    }
    let mut suffix = 0;
    while suffix < old_chars.len() - prefix
        && suffix < new_chars.len() - prefix
        && old_chars[old_chars.len() - 1 - suffix] == new_chars[new_chars.len() - 1 - suffix]
    {
        suffix += 1;
    }
    Some((
        (prefix, old_chars.len() - suffix),
        (prefix, new_chars.len() - suffix),
    ))
}

/// Pair removed/added blocks inside each hunk and stamp intraline
/// emphasis on both sides. Pairing is positional: the i-th removed
/// line of a contiguous removed-block pairs with the i-th added line
/// of the added-block that immediately follows it — the way unified
/// diffs lay out a modification.
fn apply_intraline_emphasis(file: &mut DiffFile) {
    for hunk in &mut file.hunks {
        let mut i = 0;
        while i < hunk.lines.len() {
            if hunk.lines[i].kind != LineKind::Removed {
                i += 1;
                continue;
            }
            let removed_start = i;
            while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Removed {
                i += 1;
            }
            let added_start = i;
            while i < hunk.lines.len() && hunk.lines[i].kind == LineKind::Added {
                i += 1;
            }
            let pairs = (added_start - removed_start).min(i - added_start);
            for p in 0..pairs {
                let old_idx = removed_start + p;
                let new_idx = added_start + p;
                if let Some((old_span, new_span)) = intraline_emphasis(
                    &hunk.lines[old_idx].text,
                    &hunk.lines[new_idx].text,
                ) {
                    hunk.lines[old_idx].emphasis = Some(old_span);
                    hunk.lines[new_idx].emphasis = Some(new_span);
                }
            }
        }
    }
}

/// Collect the full working-tree diff of `worktree` against `HEAD`:
/// tracked changes (staged *and* unstaged — the viewer answers "what
/// did this session change", not "what is in the index") plus
/// untracked files as synthetic all-added entries. Untracked entries
/// sort after the tracked ones, each side alphabetical (git's own
/// ordering for the tracked half).
///
/// On a repo with no commits yet every file is untracked, so the
/// missing-`HEAD` case degrades naturally to the untracked-only path.
pub fn worktree_diff(worktree: &Path) -> Result<Vec<DiffFile>> {
    let has_head = run_git(worktree, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok();

    let mut files = if has_head {
        let output = run_git(
            worktree,
            &["diff", "HEAD", "-M", "--no-color", "--no-ext-diff"],
        )
        .context("git diff HEAD")?;
        parse_unified_diff(&String::from_utf8_lossy(&output.stdout))
    } else {
        Vec::new()
    };

    let status = run_git(worktree, &["status", "--porcelain", "--untracked-files=all"])
        .context("git status --porcelain")?;
    let stdout = String::from_utf8_lossy(&status.stdout);
    let mut untracked: Vec<DiffFile> = stdout
        .lines()
        .filter_map(|l| l.strip_prefix("?? "))
        .map(|p| {
            let path = unquote_porcelain_path(p);
            untracked_file_entry(worktree, &path)
        })
        .collect();
    untracked.sort_by(|a, b| a.path.cmp(&b.path));
    files.append(&mut untracked);
    Ok(files)
}

/// Porcelain quotes paths containing special chars (`"a b.txt"`).
/// Strip the quotes; embedded escapes are left as-is (the entry still
/// renders, existence-dependent features degrade gracefully).
fn unquote_porcelain_path(raw: &str) -> String {
    let raw = raw.trim();
    raw.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(raw)
        .to_string()
}

/// Build the synthetic all-added [`DiffFile`] for an untracked path.
/// Reads from disk with byte/line caps; a read failure or NUL byte in
/// the prefix marks the entry binary instead of erroring the diff.
fn untracked_file_entry(worktree: &Path, rel_path: &str) -> DiffFile {
    let mut entry = DiffFile {
        path: rel_path.to_string(),
        old_path: None,
        status: FileStatus::Untracked,
        binary: false,
        hunks: Vec::new(),
        added: 0,
        removed: 0,
        truncated: false,
    };

    // Bounded read: pull at most cap+1 bytes off disk (the +1 tells
    // truncation apart from an exactly-cap-sized file) so a multi-GB
    // untracked artifact never lands in memory just to be previewed.
    let abs = worktree.join(rel_path);
    let bytes = {
        use std::io::Read as _;
        let Ok(file) = std::fs::File::open(&abs) else {
            entry.binary = true;
            return entry;
        };
        let mut buf = Vec::new();
        if file
            .take(MAX_UNTRACKED_BYTES + 1)
            .read_to_end(&mut buf)
            .is_err()
        {
            entry.binary = true;
            return entry;
        }
        buf
    };
    let too_big = bytes.len() as u64 > MAX_UNTRACKED_BYTES;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        entry.binary = true;
        return entry;
    }

    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(MAX_UNTRACKED_BYTES as usize)]);
    let mut lines: Vec<&str> = text.lines().collect();
    let line_capped = lines.len() > MAX_UNTRACKED_LINES;
    if line_capped {
        lines.truncate(MAX_UNTRACKED_LINES);
    }
    entry.truncated = too_big || line_capped;

    let mut hunk = DiffHunk {
        header: format!("@@ -0,0 +1,{} @@", lines.len()),
        old_start: 0,
        new_start: 1,
        lines: Vec::with_capacity(lines.len()),
    };
    for (idx, line) in lines.iter().enumerate() {
        hunk.lines.push(DiffLine {
            kind: LineKind::Added,
            old_no: None,
            new_no: Some(idx as u32 + 1),
            text: (*line).to_string(),
            emphasis: None,
        });
    }
    entry.added = hunk.lines.len() as u32;
    if !hunk.lines.is_empty() {
        entry.hunks.push(hunk);
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1111111..2222222 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,4 @@
 fn main() {
-    println!(\"hello\");
+    println!(\"world\");
 }
 // tail
@@ -10,2 +10,3 @@ mod tests {
 line_a
+line_b
 line_c
diff --git a/docs/new.md b/docs/new.md
new file mode 100644
index 0000000..3333333
--- /dev/null
+++ b/docs/new.md
@@ -0,0 +1,2 @@
+# Title
+body
";

    #[test]
    fn parses_files_hunks_and_counts() {
        let files = parse_unified_diff(SAMPLE);
        assert_eq!(files.len(), 2);

        let main = &files[0];
        assert_eq!(main.path, "src/main.rs");
        assert_eq!(main.status, FileStatus::Modified);
        assert_eq!(main.hunks.len(), 2);
        assert_eq!((main.added, main.removed), (2, 1));

        let new = &files[1];
        assert_eq!(new.path, "docs/new.md");
        assert_eq!(new.status, FileStatus::Added);
        assert_eq!((new.added, new.removed), (2, 0));
    }

    #[test]
    fn line_numbers_track_both_sides() {
        let files = parse_unified_diff(SAMPLE);
        let hunk = &files[0].hunks[0];
        assert_eq!(hunk.header, "@@ -1,4 +1,4 @@");
        assert_eq!((hunk.old_start, hunk.new_start), (1, 1));

        let nums: Vec<(Option<u32>, Option<u32>)> =
            hunk.lines.iter().map(|l| (l.old_no, l.new_no)).collect();
        assert_eq!(
            nums,
            vec![
                (Some(1), Some(1)),  // context: fn main() {
                (Some(2), None),     // removed: println hello
                (None, Some(2)),     // added: println world
                (Some(3), Some(3)),  // context: }
                (Some(4), Some(4)),  // context: // tail
            ]
        );

        let hunk2 = &files[0].hunks[1];
        assert_eq!((hunk2.old_start, hunk2.new_start), (10, 10));
        assert_eq!(hunk2.lines[1].new_no, Some(11));
    }

    #[test]
    fn paired_lines_get_intraline_emphasis() {
        let files = parse_unified_diff(SAMPLE);
        let hunk = &files[0].hunks[0];
        let removed = &hunk.lines[1];
        let added = &hunk.lines[2];
        // `    println!("hello");` vs `    println!("world");`
        // common prefix `    println!("` = 14 chars, common suffix `");` = 3.
        assert_eq!(removed.emphasis, Some((14, 19)));
        assert_eq!(added.emphasis, Some((14, 19)));
        // Unpaired added line in hunk 2 keeps whole-line emphasis.
        assert_eq!(files[0].hunks[1].lines[1].emphasis, None);
    }

    #[test]
    fn parses_rename_delete_and_binary() {
        let input = "\
diff --git a/old_name.rs b/new_name.rs
similarity index 97%
rename from old_name.rs
rename to new_name.rs
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
--- a/gone.txt
+++ /dev/null
@@ -1,1 +0,0 @@
-bye
diff --git a/img.png b/img.png
Binary files a/img.png and b/img.png differ
";
        let files = parse_unified_diff(input);
        assert_eq!(files.len(), 3);

        assert_eq!(files[0].status, FileStatus::Renamed);
        assert_eq!(files[0].path, "new_name.rs");
        assert_eq!(files[0].old_path.as_deref(), Some("old_name.rs"));

        assert_eq!(files[1].status, FileStatus::Deleted);
        assert_eq!(files[1].path, "gone.txt");
        assert_eq!((files[1].added, files[1].removed), (0, 1));

        assert!(files[2].binary);
        assert!(files[2].hunks.is_empty());
    }

    #[test]
    fn empty_input_yields_no_files() {
        assert!(parse_unified_diff("").is_empty());
    }

    #[test]
    fn intraline_emphasis_trims_prefix_and_suffix() {
        assert_eq!(
            intraline_emphasis("let x = 1;", "let x = 2;"),
            Some(((8, 9), (8, 9)))
        );
        // Pure insertion: empty old span at the insertion point.
        assert_eq!(
            intraline_emphasis("ab", "axb"),
            Some(((1, 1), (1, 2)))
        );
        // Identical lines pair to None.
        assert_eq!(intraline_emphasis("same", "same"), None);
    }

    #[test]
    fn hunk_header_without_counts_parses() {
        assert_eq!(parse_hunk_header("-1 +1 @@"), Some((1, 1)));
        assert_eq!(parse_hunk_header("-10,4 +12,6 @@ fn x()"), Some((10, 12)));
        assert_eq!(parse_hunk_header("garbage"), None);
    }

    // ─── Integration tests against a real repo ──────────────────────

    fn init_repo() -> Option<(tempfile::TempDir, std::path::PathBuf)> {
        if crate::process::no_window_command("git")
            .arg("--version")
            .output()
            .is_err()
        {
            eprintln!("skipping: `git` not on PATH");
            return None;
        }
        let dir = tempfile::tempdir().ok()?;
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).ok()?;
        run(&repo, &["-c", "init.defaultBranch=main", "init", "-q"]);
        run(&repo, &["config", "user.email", "test@example.invalid"]);
        run(&repo, &["config", "user.name", "Test"]);
        Some((dir, repo))
    }

    fn run(repo: &Path, args: &[&str]) {
        let output = crate::process::no_window_command("git")
            .args(args)
            .current_dir(repo)
            .output()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    #[test]
    fn worktree_diff_reports_modified_and_untracked() {
        let Some((_guard, repo)) = init_repo() else { return };
        std::fs::write(repo.join("tracked.txt"), "one\ntwo\n").unwrap();
        run(&repo, &["add", "tracked.txt"]);
        run(&repo, &["commit", "-m", "seed", "-q"]);

        std::fs::write(repo.join("tracked.txt"), "one\nTWO\n").unwrap();
        std::fs::write(repo.join("fresh.txt"), "hello\nworld\n").unwrap();

        let files = worktree_diff(&repo).expect("diff succeeds");
        assert_eq!(files.len(), 2, "got: {files:#?}");

        let tracked = &files[0];
        assert_eq!(tracked.path, "tracked.txt");
        assert_eq!(tracked.status, FileStatus::Modified);
        assert_eq!((tracked.added, tracked.removed), (1, 1));
        // Paired change two→TWO: whole word differs.
        let removed = tracked.hunks[0]
            .lines
            .iter()
            .find(|l| l.kind == LineKind::Removed)
            .unwrap();
        assert_eq!(removed.text, "two");

        let fresh = &files[1];
        assert_eq!(fresh.path, "fresh.txt");
        assert_eq!(fresh.status, FileStatus::Untracked);
        assert_eq!(fresh.added, 2);
        assert_eq!(fresh.hunks[0].lines[1].text, "world");
        assert_eq!(fresh.hunks[0].lines[1].new_no, Some(2));
    }

    #[test]
    fn worktree_diff_on_clean_repo_is_empty() {
        let Some((_guard, repo)) = init_repo() else { return };
        run(&repo, &["commit", "--allow-empty", "-m", "init", "-q"]);
        let files = worktree_diff(&repo).expect("diff succeeds");
        assert!(files.is_empty(), "got: {files:#?}");
    }

    #[test]
    fn worktree_diff_without_head_lists_everything_untracked() {
        let Some((_guard, repo)) = init_repo() else { return };
        std::fs::write(repo.join("first.txt"), "a\n").unwrap();
        let files = worktree_diff(&repo).expect("diff succeeds");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].status, FileStatus::Untracked);
    }

    #[test]
    fn untracked_binary_file_is_flagged_not_expanded() {
        let Some((_guard, repo)) = init_repo() else { return };
        std::fs::write(repo.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();
        let files = worktree_diff(&repo).expect("diff succeeds");
        assert_eq!(files.len(), 1);
        assert!(files[0].binary);
        assert!(files[0].hunks.is_empty());
    }
}
