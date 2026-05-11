//! Cross-platform path canonicalisation for cwd comparison.
//!
//! Mirrors C# `PiSessionDiscovery.CanonicalizePath` — used by every
//! non-Claude session-discovery service to compare a transcript's
//! recorded `cwd` against the spawned tab's working directory across
//! slash direction, drive-letter colon, leading/trailing slashes, and
//! case.
//!
//! Examples (all collapse to the same canonical form):
//!
//! ```
//! # use codescope_core::path_canon::canonicalize_path;
//! assert_eq!(canonicalize_path("C:\\dev\\codescope"), "c/dev/codescope");
//! assert_eq!(canonicalize_path("/c/dev/codescope"),  "c/dev/codescope");
//! assert_eq!(canonicalize_path("c:/dev/codescope"),  "c/dev/codescope");
//! ```

/// Compare two paths for logical equivalence — same on-disk location
/// even when one form uses backslashes / lower case / a `/c/...`
/// MSYS-style mount and the other a Windows drive letter. Cheap
/// wrapper over [`canonicalize_path`] so callers (sidebar busy-state
/// lookup, session-restore matching) don't have to do the canonicalise
/// dance inline. Returns `false` when either input is empty so a
/// missing working-directory never collides with another missing one.
pub fn paths_match(a: &str, b: &str) -> bool {
    if a.is_empty() || b.is_empty() {
        return false;
    }
    canonicalize_path(a) == canonicalize_path(b)
}

/// Canonicalise a path for cross-platform comparison: lowercase,
/// forward-slashes, drive-colon stripped, trimmed leading and trailing
/// slashes. Mirrors C# `PiSessionDiscovery.CanonicalizePath`.
pub fn canonicalize_path(path: &str) -> String {
    if path.is_empty() {
        return String::new();
    }
    let mut s: String = path
        .chars()
        .map(|c| match c {
            '\\' => '/',
            ':' => '\0',
            other => other,
        })
        .collect();
    s.retain(|c| c != '\0');
    let trimmed = s.trim_start_matches('/').trim_end_matches('/');
    trimmed.to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_and_posix_forms_collapse() {
        assert_eq!(canonicalize_path("C:\\dev\\codescope"), "c/dev/codescope");
        assert_eq!(canonicalize_path("/c/dev/codescope"), "c/dev/codescope");
        assert_eq!(canonicalize_path("c:/dev/codescope"), "c/dev/codescope");
        assert_eq!(canonicalize_path("c:/dev/codescope/"), "c/dev/codescope");
    }

    #[test]
    fn empty_input_yields_empty() {
        assert_eq!(canonicalize_path(""), "");
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(canonicalize_path("C:/Dev/CodeScope"), "c/dev/codescope");
    }

    #[test]
    fn paths_match_collapses_slash_direction_and_case() {
        assert!(paths_match("C:\\dev\\codescope", "c:/dev/codescope"));
        assert!(paths_match("C:/Dev/CodeScope", "/c/dev/codescope"));
        assert!(paths_match("C:/dev/codescope/", "c:/dev/codescope"));
    }

    #[test]
    fn paths_match_rejects_distinct_paths() {
        assert!(!paths_match("C:/dev/codescope", "C:/dev/other"));
    }

    #[test]
    fn paths_match_empty_inputs_never_collide() {
        // Two missing working-directories shouldn't be treated as the
        // "same path" — bug-prone for callers iterating tabs/sessions
        // looking for matches by working dir.
        assert!(!paths_match("", ""));
        assert!(!paths_match("", "C:/dev/codescope"));
        assert!(!paths_match("C:/dev/codescope", ""));
    }
}
