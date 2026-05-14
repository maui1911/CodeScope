# C# build retirement — design

> Date: 2026-05-14
> Status: Draft → ready for plan
> Scope: retire the C# (.NET 10 / WPF) implementation of CodeScope, promote
>        the Rust port (`codescope-rs/`) to the canonical and only build.

## Problem

The repo carries two parallel implementations:

* `src/CodeScope.{App,Core,Ui}/` — the original .NET 10 / WPF build, last
  released as `v0.2.6` on 2026-05-08.
* `codescope-rs/` — the Rust + GPUI port, currently at `0.3.0-rc.5` with
  PRs #205 (session rehydrate) and #206 (prerelease-aware update check)
  pending merge / merged.

The Rust port is now the user's daily driver. The C# build is no longer
being developed — recent commits (#194 onward, ≈ 20 in a row) are all
`(rs)`-namespaced. Keeping both implementations alive in the same tree
costs: duplicated release pipelines (`release.yml` + `rs--release.yml`),
divergent build settings (`Directory.Build.{props,targets}`), parity
docs that get stale within days (`PARITY-AUDIT.md` last touched at
PR #132), and a mental burden of "where's the canonical answer for X"
on every change.

## Decision

Retire C# entirely. Rust becomes the only implementation. Done in three
sequential PRs so each step is small enough to review and the
intermediate states remain a working build.

### Trigger

Pure soak gate: ~7–10 days of `0.3.0-rc.5` (and `rc.6` once #206 lands)
running as the user's daily driver without surfacing a kritisch issue
that demands a fallback to the C# build. No formal feature parity audit —
the user is the only user, and the only blockers tracked recently
(session rehydrate, auto-update) are already fixed.

### Scope decisions made up front

| Question | Decision |
| --- | --- |
| Audience | Solo dogfooding — only the user needs Rust to be comfortable. |
| Daily driver today | Rust port (already running as installed `codescope-rs.exe`). |
| Velopack pack id after cutover | Rename `codescope-rs` → `codescope`. Clean break. |
| Tag namespace after cutover | Rename `rs-v*` → `v*`. |
| Folder layout after cutover | Flatten `codescope-rs/*` → repo root (standard Rust workspace idiom). |
| Old `v0.2.X` GitHub releases | Leave in place as history. Floor-filter on `update_check` keeps them out of the live update path. |
| Preflight release before `v0.3.0` | Skipped. One-time manual reinstall is acceptable solo. |

## PR breakdown

### PR cutover-1 — docs only, declare canonical

No code changes, no build changes. Sets the narrative and creates
the safety net for cutover-2.

* New: `docs/DECISIONS.md` — ADR-0022 "Rust port becomes canonical
  CodeScope; C# build retired".
* New: `docs/MIGRATION-csharp-to-rust.md` — historical note: why and how.
* Update: `CLAUDE.md` — replace "Technology decisions (non-negotiable)"
  block with the Rust stack. Remove "mirror C# implementation" workflow
  rules. Remove `dotnet run` / `CodeScope.App` references. Side-by-side
  dev section stays (CODESCOPE_DEV) — that's Rust-side.
* Update: `docs/HANDOFF.md` — banner at top: "Rust is canonical; C#
  archived at tag `legacy/v0.2.6-final`."
* Update: `README.md` — install link and build instructions point at the
  Rust release / pipeline. Remove `.csproj` / `dotnet build` sections.
* Update: `docs/PARITY-AUDIT.md` — closing stamp; mark historical.
* Update: `docs/ARCHITECTURE.md` — rewrite from the Rust perspective.
* Update: `docs/DESIGN.md` — mark WPF / `Wpf.Ui` references as historical.
* Git: tag `legacy/v0.2.6-final` on the merge commit of cutover-1 (the
  last commit where the C# code is still present and buildable). Push.

### PR cutover-2 — delete C#, keep `codescope-rs/` in place

Repo remains Rust-only and still builds via `cargo build
--manifest-path codescope-rs/Cargo.toml`. No restructure in this step.

* Delete: `src/CodeScope.App/`, `src/CodeScope.Core/`, `src/CodeScope.Ui/`
  (incl. `bin/` / `obj/`).
* Delete: `tests/` (C# xUnit suite).
* Delete: `CodeScope.sln`, `Directory.Build.props`, `Directory.Build.targets`.
* Delete: `.github/workflows/release.yml` (C# release pipeline).
* Delete: C# artefacts under `artifacts/`. Rust output stays.
* Delete: `docs/screenshots/` C# screenshots waar er Rust equivalents zijn.
* Update: `.gitignore` — strip C#-specific patterns (`*.csproj.user`,
  top-level `bin/` / `obj/` rules).
* Update: `CLAUDE.md` — final removal of any `src/CodeScope.*` reference.
* Update: `README.md` — section-level cleanup of any remaining C# blocks.

Build verification before merge:
* `cargo build --workspace --manifest-path codescope-rs/Cargo.toml` ✓
* `cargo test --workspace --manifest-path codescope-rs/Cargo.toml` ✓
* `grep -r "src/CodeScope" .github/ codescope-rs/` returns nothing.
* `grep -r "Directory.Build" .github/ codescope-rs/` returns nothing.
* `grep -r "CodeScope.sln" .github/ codescope-rs/` returns nothing.
* In GitHub branch protection: verify the C# `release` workflow is not
  listed as a required check; remove if it is.

### PR cutover-3 — restructure + pack id rename

Highest-risk PR. Move Rust tree to repo root, rename pack id, rename tag
namespace.

**Move (`git mv` to preserve history):**

* `codescope-rs/*` → repo root: `src/`, `core/`, `terminal/`, `examples/`,
  `assets/`, `wix/`, `scripts/`, `vendor/`, `Cargo.toml`, `Cargo.lock`,
  `build.rs`, `dist-workspace.toml`.

**Rename file:**

* `.github/workflows/rs--release.yml` → `.github/workflows/release.yml`.

**Content edits:**

* `Cargo.toml` (workspace root): `members = ["./core", "./terminal"]`
  (paths were `codescope-rs/core` etc.).
* `.github/workflows/release.yml`:
  - Tag pattern: `rs-v*` → `v*` (every occurrence).
  - `packVersion="${GITHUB_REF_NAME#rs-v}"` → `"${GITHUB_REF_NAME#v}"`.
  - Remove `working-directory: codescope-rs` lines (we are at root now).
  - `--packId codescope-rs` → `--packId codescope`.
* `build.rs`:
  - `git describe --match "rs-v*"` → `--match "v*"`.
  - `strip_rs_v_prefix` helper → use `strip_v_prefix` (or inline).
* `core/src/update_check.rs`:
  - `RUST_TAG_PREFIX` constant removed.
  - `evaluate` filter: accept any `v*` tag with version `>= 0.3.0` (new
    floor constant) so the historical `v0.2.X` C# releases don't show up
    as available updates. Adjust existing tests.

**User-side action (one-off):**

Uninstall current `codescope-rs`. Download new `codescope` installer
from the first post-cutover release (typically the `v0.3.0-rc.7` or
`v0.3.0` final tag). The `%APPDATA%\CodeScope\projects.json` is
unchanged; the new install picks up the existing session catalog.

**Build verification before merge:**

* `cargo build --workspace` (no `--manifest-path`) — must work from repo
  root.
* `cargo test --workspace` ✓
* `cargo clippy --workspace --all-targets` (existing warnings excluded).
* `grep -rn "codescope-rs/" .github/ scripts/ build.rs Cargo.toml` →
  expect zero hits outside historical comments.
* Diff-walk every line of `.github/workflows/release.yml` to confirm no
  stale `rs-v` / `codescope-rs/` reference remains.

## Risks and rollback per PR

| PR | Risk | Rollback |
| --- | --- | --- |
| cutover-1 | Doc inconsistency between ADR-0022 and other docs. Low impact. | `git revert <sha>`. Tag `legacy/v0.2.6-final` stays. |
| cutover-2 | Branch protection / required check pointing at deleted C# `release.yml` blocks merges. Settings change is silent until first push. | `git revert <sha>`. Branch protection updated. C# code recoverable via tag. |
| cutover-3 | Workflow path typos break release. Pack id rename strands the local install (no auto-update from old → new). | Revert PR; hot-fix release as `codescope` with version above any test build to re-establish lineage. Worst case: second manual reinstall. |

## Non-goals

* Public announcement / blog post for external users. There are none.
* Auto-migration tooling that bridges the C# install to the Rust install
  in-process. The `%APPDATA%\CodeScope\projects.json` is already shared
  by design (the Rust port reads C# JSON shape); no migration code
  needed.
* Renaming the GitHub repository itself. Stays `maui1911/CodeScope`.
* Deleting the historical `v0.2.X` C# releases from GitHub. They remain
  as archive entries; the `update_check` floor filter keeps them out of
  the live update flow.

## Testing summary

| Step | What | When |
| --- | --- | --- |
| Soak | rc.5 → rc.6 daily-driver, no critical issues | before cutover-1 |
| Doc lint | grep historical C# refs in `docs/` post-merge | in cutover-1 review |
| Build + tests | `cargo build/test/clippy --workspace` | in every PR |
| Workflow path check | grep `codescope-rs/`, `rs-v`, `src/CodeScope` | in cutover-2 and -3 review |
| Pack id smoke | uninstall + reinstall after cutover-3's first release | once post-cutover-3 |

## Out of scope for this design

* Visual design / branding refresh (logo, screenshots in README): handled
  separately if needed.
* Migration of the Issue tracker labels (`(rs)` prefix on every issue).
  Cosmetic, separate cleanup PR if desired.

## Implementation order

1. Wait out the soak (`rc.5` → `rc.6`).
2. Land PR cutover-1.
3. Tag `legacy/v0.2.6-final` on the cutover-1 merge commit.
4. Land PR cutover-2.
5. Land PR cutover-3.
6. Push first post-cutover tag (`v0.3.0-rc.7` or `v0.3.0`), confirm
   release pipeline produces `codescope-*-Setup.exe`.
7. Locally: uninstall old `codescope-rs`, install new `codescope`.
8. Verify `projects.json` still loads, sessions rehydrate, update_check
   floor filter keeps `v0.2.X` releases out of the bell.
