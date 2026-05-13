//! Local attachment storage for terminal-agent prompts.
//!
//! Terminal CLIs can only receive text through the PTY. CodeScope's
//! screenshot-paste flow stores clipboard image bytes under the active
//! worktree and then pastes the relative path into the agent prompt.
//! This module owns the filesystem contract so the gpui terminal layer
//! only has to supply bytes + an image extension.

use std::collections::hash_map::DefaultHasher;
use std::fs::{self, OpenOptions};
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};

use crate::process::no_window_command;
use crate::time::unix_secs_to_civil;

const ATTACHMENT_DIR: &str = ".codescope/attachments";
// Unanchored on purpose: the tab's working directory can be a
// subdirectory of the worktree, so attachments live at
// `<working_dir>/.codescope/attachments/`, not at the worktree root.
// A leading `/` would only match at the root and miss those.
const GIT_EXCLUDE_PATTERN: &str = ".codescope/attachments/";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedAttachment {
    /// Absolute path to the file CodeScope wrote.
    pub absolute_path: PathBuf,
    /// Text that should be inserted into the terminal prompt.
    /// Always relative to the storage root and slash-normalized so it
    /// works in PowerShell, bash, and agent prompt parsers.
    pub paste_path: String,
}

/// Store attachment bytes under `<working-directory>/.codescope/attachments/`
/// and return the prompt-friendly relative path.
///
/// Best-effort updates the repo-local `.git/info/exclude` when the
/// working directory is inside a git worktree. A failure to find git or
/// write the exclude file does **not** fail the paste — the screenshot
/// is already local and the user asked for a path, not a git operation.
pub fn save_attachment_bytes(
    working_directory: Option<&Path>,
    extension: &str,
    bytes: &[u8],
) -> Result<SavedAttachment> {
    let root = match working_directory {
        Some(path) => path.to_path_buf(),
        None => {
            std::env::current_dir().context("resolve current directory for attachment paste")?
        }
    };
    save_attachment_bytes_at(&root, extension, bytes, SystemTime::now())
}

fn save_attachment_bytes_at(
    root: &Path,
    extension: &str,
    bytes: &[u8],
    now: SystemTime,
) -> Result<SavedAttachment> {
    if bytes.is_empty() {
        return Err(anyhow!("clipboard image was empty"));
    }

    fs::create_dir_all(root.join(ATTACHMENT_DIR))?;

    let extension = sanitize_extension(extension);
    let filename = attachment_filename(bytes, &extension, now);
    let mut relative_path = PathBuf::from(ATTACHMENT_DIR).join(&filename);
    let mut absolute_path = root.join(&relative_path);

    let mut suffix = 2usize;
    while absolute_path.exists() {
        let suffix_to_strip = format!(".{extension}");
        let stem = filename.strip_suffix(&suffix_to_strip).unwrap_or(&filename);
        let candidate = format!("{stem}-{suffix}.{extension}");
        relative_path = PathBuf::from(ATTACHMENT_DIR).join(&candidate);
        absolute_path = root.join(&relative_path);
        suffix += 1;
    }

    fs::write(&absolute_path, bytes)?;
    let _ = ensure_git_exclude(root);

    Ok(SavedAttachment {
        absolute_path,
        paste_path: slash_path(&relative_path),
    })
}

fn attachment_filename(bytes: &[u8], extension: &str, now: SystemTime) -> String {
    let duration = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = duration.as_secs() as i64;
    let (year, month, day, hour, minute, second) = unix_secs_to_civil(secs);

    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    duration.as_nanos().hash(&mut hasher);
    let fingerprint = hasher.finish() & 0x00ff_ffff;

    format!(
        "screenshot-{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}-{fingerprint:06x}.{extension}"
    )
}

fn sanitize_extension(extension: &str) -> String {
    let cleaned: String = extension
        .trim()
        .trim_start_matches('.')
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(|c| c.to_lowercase())
        .take(8)
        .collect();

    match cleaned.as_str() {
        "" => "png".to_string(),
        "jpeg" => "jpg".to_string(),
        other => other.to_string(),
    }
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn ensure_git_exclude(root: &Path) -> Result<()> {
    // Resolve the worktree root first so subsequent git invocations
    // produce paths relative to a stable base. `git rev-parse --git-path`
    // returns paths relative to its current working directory, so calling
    // it from a subdirectory of a worktree would otherwise give us a
    // relative path that only resolves correctly via filesystem `..`
    // traversal — fragile, and on some hosts would silently create a
    // stray `.git/info/exclude` under the subdirectory.
    let top_level_output = no_window_command("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;

    if !top_level_output.status.success() {
        return Ok(());
    }

    let top_level_str = String::from_utf8_lossy(&top_level_output.stdout)
        .trim()
        .to_string();
    if top_level_str.is_empty() {
        return Ok(());
    }
    let top_level = PathBuf::from(top_level_str);

    let output = no_window_command("git")
        .args(["rev-parse", "--git-path", "info/exclude"])
        .current_dir(&top_level)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()?;

    if !output.status.success() {
        return Ok(());
    }

    let raw_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw_path.is_empty() {
        return Ok(());
    }

    let exclude_path = {
        let path = PathBuf::from(raw_path);
        if path.is_absolute() {
            path
        } else {
            top_level.join(path)
        }
    };

    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut existing = String::new();
    if exclude_path.exists() {
        fs::File::open(&exclude_path)?.read_to_string(&mut existing)?;
    }

    if existing
        .lines()
        .any(|line| line.trim() == GIT_EXCLUDE_PATTERN)
    {
        return Ok(());
    }

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        writeln!(file)?;
    }
    writeln!(file, "{GIT_EXCLUDE_PATTERN}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn saves_attachment_under_codescope_folder_and_returns_slash_relative_path() {
        let dir = TempDir::new().unwrap();
        let bytes = b"not really png, but saved verbatim";
        let saved = save_attachment_bytes_at(
            dir.path(),
            "png",
            bytes,
            UNIX_EPOCH + std::time::Duration::from_secs(1_779_811_330),
        )
        .unwrap();

        assert_eq!(fs::read(&saved.absolute_path).unwrap(), bytes);
        assert!(saved.absolute_path.starts_with(dir.path()));
        assert!(
            saved
                .paste_path
                .starts_with(".codescope/attachments/screenshot-")
        );
        assert!(saved.paste_path.ends_with(".png"));
        assert!(!saved.paste_path.contains('\\'));
    }

    #[test]
    fn sanitizes_extensions_for_filenames() {
        assert_eq!(sanitize_extension(".PNG"), "png");
        assert_eq!(sanitize_extension("jpeg"), "jpg");
        assert_eq!(sanitize_extension(" ../bad!! "), "bad");
        assert_eq!(sanitize_extension(""), "png");
    }

    #[test]
    fn rejects_empty_attachment_bytes() {
        let dir = TempDir::new().unwrap();
        let err = save_attachment_bytes_at(dir.path(), "png", b"", UNIX_EPOCH).unwrap_err();
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn writes_git_exclude_at_worktree_root_when_saving_from_subdir() {
        if no_window_command("git").arg("--version").output().is_err() {
            return;
        }

        let dir = TempDir::new().unwrap();
        let init = no_window_command("git")
            .arg("init")
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(init.status.success());

        let subdir = dir.path().join("nested").join("workdir");
        fs::create_dir_all(&subdir).unwrap();

        save_attachment_bytes_at(&subdir, "png", b"sub", UNIX_EPOCH).unwrap();

        // The exclude file must live at the worktree root, not under the
        // subdirectory we saved from.
        let root_exclude = dir.path().join(".git/info/exclude");
        assert!(root_exclude.exists(), "exclude file should be at worktree root");
        assert!(
            !subdir.join(".git").exists(),
            "must not have created a stray .git under the subdirectory"
        );

        let exclude = fs::read_to_string(&root_exclude).unwrap();
        // Pattern must be unanchored so it matches attachments saved from
        // any subdirectory of the worktree.
        assert!(
            exclude
                .lines()
                .any(|line| line.trim() == GIT_EXCLUDE_PATTERN),
            "exclude file should contain the unanchored attachments pattern"
        );
        assert!(
            !GIT_EXCLUDE_PATTERN.starts_with('/'),
            "pattern must not be anchored to the worktree root"
        );
    }

    #[test]
    fn appends_git_exclude_once_when_inside_git_worktree() {
        if no_window_command("git").arg("--version").output().is_err() {
            return;
        }

        let dir = TempDir::new().unwrap();
        let init = no_window_command("git")
            .arg("init")
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(init.status.success());

        save_attachment_bytes_at(dir.path(), "png", b"one", UNIX_EPOCH).unwrap();
        save_attachment_bytes_at(dir.path(), "png", b"two", UNIX_EPOCH).unwrap();

        let exclude = fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude
                .lines()
                .filter(|line| line.trim() == GIT_EXCLUDE_PATTERN)
                .count(),
            1
        );
    }
}
