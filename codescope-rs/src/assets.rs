//! Static asset source for the gpui app. Currently only serves the
//! status-bar SVG icons (12 × 12 viewBox, stroke="currentColor") so
//! `gpui::svg().path("icons/branch.svg")` can resolve to bytes
//! embedded in the binary via `include_bytes!`.
//!
//! Why an explicit `AssetSource` and not files on disk? gpui's
//! `SvgRenderer` calls `AssetSource::load(path)` to fetch the SVG
//! bytes; without one, every `svg()` element silently no-ops. We want
//! the icons to ship inside the binary (no install-time copying, no
//! "assets dir not found" failure modes), so a hand-rolled static
//! source that maps a known set of paths to `include_bytes!` literals
//! is the cheapest path. Mirrors the icon set used in the C# build's
//! `StatusBarView.xaml` (same path data, just wrapped in an SVG
//! shell). See the SVG files under `assets/icons/`.

use std::borrow::Cow;

use anyhow::Result;
use gpui::{AssetSource, SharedString};

/// Embedded icon assets resolved by relative path (e.g.
/// `"icons/branch.svg"`). The set is closed at compile time and
/// matches the paths the status-bar renderer asks for.
pub struct AppAssets;

macro_rules! icon {
    ($path:literal) => {
        ($path, include_bytes!(concat!("../assets/", $path)).as_slice())
    };
}

/// Static table of `(path, bytes)` pairs. Add a new icon here when
/// adding a new `svg().path("icons/whatever.svg")` call.
const ICONS: &[(&str, &[u8])] = &[
    icon!("icons/branch.svg"),
    icon!("icons/sync.svg"),
    icon!("icons/model.svg"),
    icon!("icons/tokens.svg"),
    icon!("icons/clock.svg"),
    icon!("icons/bell.svg"),
    icon!("icons/worktree.svg"),
    icon!("icons/settings.svg"),
];

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        for (key, bytes) in ICONS {
            if *key == path {
                return Ok(Some(Cow::Borrowed(*bytes)));
            }
        }
        Ok(None)
    }

    fn list(&self, _path: &str) -> Result<Vec<SharedString>> {
        Ok(ICONS
            .iter()
            .map(|(p, _)| SharedString::from(*p))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_icons_load() {
        let assets = AppAssets;
        for (path, _) in ICONS {
            let loaded = assets.load(path).expect("load");
            assert!(loaded.is_some(), "missing bytes for {path}");
        }
    }

    #[test]
    fn unknown_path_returns_none() {
        let assets = AppAssets;
        assert!(assets.load("icons/does-not-exist.svg").unwrap().is_none());
    }

    #[test]
    fn icon_bytes_are_nonempty_svgs() {
        let assets = AppAssets;
        for (path, _) in ICONS {
            let bytes = assets.load(path).unwrap().expect("present");
            assert!(!bytes.is_empty(), "{path} empty");
            // Sanity: ensure it's actually XML SVG markup.
            let text = std::str::from_utf8(&bytes).expect("utf8");
            assert!(text.contains("<svg"), "{path} not an SVG");
            assert!(text.contains("</svg>"), "{path} not closed");
        }
    }
}
