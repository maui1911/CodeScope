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
//! caption row can read it via `env!`.
//!
//! Override order:
//!
//! 1. `CODESCOPE_VERSION` env var — packaging pipelines set this
//!    explicitly when they don't want git-derived strings (or
//!    when git isn't available).
//! 2. `git describe --tags --always --dirty` — normal dev /
//!    release builds. Leading `v` / `V` is stripped so the chrome
//!    can prefix its own `V` consistently.
//! 3. `0.0-unknown` — last-resort fallback so the slug is
//!    obviously a placeholder rather than masquerading as a real
//!    version (`CARGO_PKG_VERSION` would print `0.0.1`, which
//!    would look like a product release).
//!
//! ## Icon
//!
//! Mirrors `<ApplicationIcon>assets\codescope.ico</ApplicationIcon>`
//! from the C# build's `CodeScope.App.csproj`. The resource script
//! at `assets/codescope.rc` references `codescope.ico` under
//! resource ID `1`, which is what Windows looks up when it needs
//! "the" icon for an executable. No-op on non-Windows hosts.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    bake_version_slug();

    #[cfg(windows)]
    embed_resource::compile("assets/codescope.rc", embed_resource::NONE)
        .manifest_optional()
        .expect("compile codescope.rc");
}

fn bake_version_slug() {
    // Always honour the env-var override first so packaging
    // pipelines can pin the slug regardless of what's checked out.
    println!("cargo:rerun-if-env-changed=CODESCOPE_VERSION");
    if let Ok(explicit) = std::env::var("CODESCOPE_VERSION") {
        let v = strip_v_prefix(explicit.trim());
        println!("cargo:rustc-env=CODESCOPE_VERSION_DISPLAY={v}");
        return;
    }

    let display = git_describe().unwrap_or_else(|| "0.0-unknown".into());
    println!("cargo:rustc-env=CODESCOPE_VERSION_DISPLAY={display}");

    // `rerun-if-changed` for the *real* git refs so a fresh commit
    // or tag updates the slug without a `cargo clean`. The naive
    // `../.git/HEAD` form misses two important shapes:
    //
    //   * `.git` is a **file** inside a linked worktree (`git
    //     worktree add` writes a `gitdir:` pointer to the canonical
    //     workdir under `<repo>/.git/worktrees/<name>`).
    //   * Branch-tracked HEAD updates `refs/heads/<branch>` (and
    //     `packed-refs` after a gc), not `HEAD` itself.
    //
    // Resolving via `git rev-parse --git-path` handles both.
    // Failures (no git, not a repo, …) just skip the trigger — the
    // env-var / fallback path above still produces a usable slug.
    for relative in ["HEAD", "packed-refs", "index", "refs/tags"] {
        if let Some(p) = git_path(relative)
            && p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
    }
    // The current branch's actual ref file. Resolves even when
    // packed (in which case the file may not exist; we already
    // watch `packed-refs` above).
    if let Some(symref) = run_git(&["symbolic-ref", "--quiet", "HEAD"])
        && let Some(p) = git_path(&symref)
            && p.exists() {
                println!("cargo:rerun-if-changed={}", p.display());
            }
}

fn strip_v_prefix(s: &str) -> String {
    s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s).to_owned()
}

fn run_git(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if s.is_empty() { None } else { Some(s) }
}

fn git_path(relative: &str) -> Option<PathBuf> {
    let raw = run_git(&["rev-parse", "--git-path", relative])?;
    Some(PathBuf::from(raw))
}

fn git_describe() -> Option<String> {
    // Release tags are `vX.Y.Z` (or `vX.Y.Z-rc.N`). Historical C#
    // `v0.2.X` tags resolve here too on a checkout of that legacy
    // history; stamping the binary with the historical version is
    // correct for that checkout.
    let raw = run_git(&["describe", "--tags", "--always", "--dirty", "--match", "v*"])?;
    Some(strip_v_prefix(&raw))
}
