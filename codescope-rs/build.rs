//! Build script — bakes the product version slug + (on Windows)
//! embeds the application icon into the `window` binary.
//!
//! ## Version slug
//!
//! Mirrors C# `VersionInfo.Display`, which reads
//! `AssemblyInformationalVersionAttribute` filled in from
//! `git describe --tags --always --dirty` via
//! `Directory.Build.targets`. We do the same here: at build time
//! we shell out to git, drop the leading `v`, and expose the
//! result as the `CODESCOPE_VERSION_DISPLAY` env var so the
//! caption row can read it via `env!`. Falls back to
//! `CARGO_PKG_VERSION` if git is unavailable (sandboxed
//! tarball builds, etc.).
//!
//! ## Icon
//!
//! Mirrors `<ApplicationIcon>assets\codescope.ico</ApplicationIcon>`
//! from the C# build's `CodeScope.App.csproj`. The resource script
//! at `assets/codescope.rc` references `codescope.ico` under
//! resource ID `1`, which is what Windows looks up when it needs
//! "the" icon for an executable. No-op on non-Windows hosts.

use std::process::Command;

fn main() {
    bake_version_slug();
    // Re-run when the git HEAD or refs change so the slug stays in
    // sync with the latest commit / tag without requiring a clean.
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs/tags");

    #[cfg(windows)]
    embed_resource::compile("assets/codescope.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("compile codescope.rc");
}

fn bake_version_slug() {
    let display = git_describe()
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0".into()));
    println!("cargo:rustc-env=CODESCOPE_VERSION_DISPLAY={display}");
}

/// Run `git describe --tags --always --dirty` from the workspace
/// root and return the trimmed result with a leading `v` / `V`
/// stripped (so `v0.2.6-52-g…` becomes `0.2.6-52-g…`). Returns
/// `None` if git isn't on PATH or exits non-zero.
fn git_describe() -> Option<String> {
    let output = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        // The build runs from `codescope-rs/` but the git repo root
        // is one level up. Keeping the manifest dir as the working
        // directory still works because git walks parents — but be
        // explicit to avoid surprises if the layout shifts later.
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if raw.is_empty() {
        return None;
    }
    let stripped = raw.strip_prefix('v').or_else(|| raw.strip_prefix('V')).unwrap_or(&raw);
    Some(stripped.to_owned())
}
