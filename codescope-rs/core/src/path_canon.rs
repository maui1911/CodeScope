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
}
