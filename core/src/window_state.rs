//! Persisted window position + size + maximised state.
//!
//! Stored at [`AppPaths::window_file`] (under `%LOCALAPPDATA%` because
//! it's machine-local — copying a `%APPDATA%` home roaming-profile
//! between PCs with different monitor layouts shouldn't drag stale
//! window coordinates along). The Rust port writes the same shape the
//! C# build expects: `x`, `y`, `width`, `height`, `maximised`.

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::AppPaths;

/// Last-known window geometry. Pixel-coords on whatever display the
/// window was last on; the OS clamps them when the display layout
/// changes (e.g. external monitor unplugged).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct WindowState {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub maximised: bool,
}

impl Default for WindowState {
    fn default() -> Self {
        // Sensible first-launch size — wide enough for the sidebar
        // (240) + a usable terminal (~800).
        Self {
            x: 100,
            y: 100,
            width: 1280,
            height: 800,
            maximised: false,
        }
    }
}

impl WindowState {
    pub fn load(paths: &AppPaths) -> Result<Option<Self>> {
        Self::load_from(&paths.window_file())
    }

    pub fn load_from(path: &Path) -> Result<Option<Self>> {
        match std::fs::read(path) {
            Ok(bytes) if bytes.is_empty() => Ok(None),
            Ok(bytes) => {
                let state: Self = serde_json::from_slice(&bytes)
                    .with_context(|| format!("parse {}", path.display()))?;
                // Reject implausible sizes — saved state from a
                // 4K → laptop swap could give us an absurd width.
                // The OS would clamp anyway, but bail early so we
                // don't trust obviously-broken values.
                if state.width < 320 || state.height < 240 {
                    return Ok(None);
                }
                Ok(Some(state))
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err).with_context(|| format!("read {}", path.display())),
        }
    }

    pub fn save(&self, paths: &AppPaths) -> Result<()> {
        paths.ensure_dirs()?;
        self.save_to(&paths.window_file())
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let mut json = serde_json::to_string_pretty(self)
            .context("serialise window state")?;
        json.push('\n');
        std::fs::write(path, json)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_yields_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(WindowState::load_from(&path).unwrap().is_none());
    }

    #[test]
    fn round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.json");
        let state = WindowState {
            x: 250,
            y: 175,
            width: 1600,
            height: 900,
            maximised: true,
        };
        state.save_to(&path).unwrap();
        let loaded = WindowState::load_from(&path).unwrap().unwrap();
        assert_eq!(loaded.x, 250);
        assert!(loaded.maximised);
    }

    #[test]
    fn implausibly_small_state_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.json");
        std::fs::write(&path, r#"{"x":0,"y":0,"width":50,"height":40}"#).unwrap();
        assert!(WindowState::load_from(&path).unwrap().is_none());
    }
}
