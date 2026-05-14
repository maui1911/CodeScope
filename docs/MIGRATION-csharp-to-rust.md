# Migration: C# (.NET 10 / WPF) → Rust (GPUI)

> Date: 2026-05-14
> Status: historical note — written at the start of the C# build retirement.
> Related: [ADR-0022](DECISIONS.md), spec at
> `docs/superpowers/specs/2026-05-14-csharp-build-retirement-design.md`.

## Why this happened

CodeScope shipped as a .NET 10 / WPF app from `v0.1.0` (2026-03) through
`v0.2.6` (2026-05-08). The Rust port (`codescope-rs/`) started in session
30 as a learning exercise / cross-platform experiment, was promoted to a
1:1 functional parity port by session 33, and reached daily-driver
quality at `0.3.0-rc.5` (session 41).

Three forces pushed the cutover:

1. **Single-developer signal.** Recent work was already Rust-only —
   roughly the last twenty merged PRs (`#194` onward) are `(rs)`-tagged,
   and the C# tree had not been touched in weeks.
2. **Cross-platform.** Velopack feeds now publish for Windows, macOS
   (arm64 + x64), and Linux x64 from one Rust pipeline. WPF is
   Windows-only by construction.
3. **Maintenance tax.** Two parallel release pipelines
   (`release.yml` + `rs--release.yml`), two build-property layers
   (`Directory.Build.{props,targets}` + Cargo workspace), and a parity
   audit doc (`docs/PARITY-AUDIT.md`) that needed manual upkeep on every
   feature touch.

## What was kept

* **`%APPDATA%\CodeScope\projects.json`** — same JSON shape, same path.
  The Rust port reads and writes the C# schema unchanged. Existing
  session catalogs roll forward across the install switch with no
  migration step.
* **`%LOCALAPPDATA%\CodeScope\layout.json`** — same shape with one
  forward-compatible addition: `session_placements` replaces the
  legacy `open_tabs` array (one-shot migration on first load, see
  [ADR-0021](DECISIONS.md)).
* **All user-visible flows** — sidebar tree, tab strip, split groups,
  command palette, overview grid, new-worktree dialog, new-project /
  clone dialog, context menus, idle toasts. Layouts and labels match
  the C# build line for line.
* **Velopack auto-update infrastructure** — same release-feed concept,
  different channels (`win` / `osx-arm64` / `osx-x64` / `linux-x64`).
* **Keyboard shortcuts** — every mapping from the WPF `KeyBindings`
  block carried over identically.

## What was dropped

* **`EasyWindowsTerminalControl`** (WPF ConPTY host) — replaced by
  `codescope-terminal`, a GPUI-native ConPTY/PTY view that shares
  ANSI parsing with the rest of GPUI's terminal stack.
* **`Wpf.Ui` theme system + XAML resource dictionaries** — replaced by
  a small Rust theme module (`codescope-rs/src/theme.rs`) that mirrors
  the same token names.
* **`CommunityToolkit.Mvvm` relay-command plumbing** — GPUI uses direct
  view methods + `cx.notify()`; no MVVM indirection.
* **`Directory.Build.{props,targets}`, `CodeScope.sln`, all
  `.csproj`** — replaced by the Cargo workspace at
  `codescope-rs/Cargo.toml`.
* **C# `release.yml` workflow** — replaced by `rs--release.yml`, later
  renamed back to `release.yml` in cutover-3.

## Install hand-off

The Rust build ships as `codescope-rs-win-Setup.exe` (Velopack pack id
`codescope-rs`) through cutover-2. In cutover-3 the pack id is renamed
to `codescope`, which strands the auto-update lineage exactly once:
users on the old `codescope-rs` install need to uninstall and reinstall
from the first post-rename release. The `%APPDATA%\CodeScope\` data
directory is unchanged across that switch.

## Where the old code lives

The last commit with the C# source tree intact is tagged
`legacy/v0.2.6-final` on the cutover-1 merge commit. Historical
`v0.2.X` GitHub releases remain published as archive entries; the
`update_check` floor filter keeps them out of the live update path.

## Reference timeline

| Phase | Tag / PR | Notes |
| --- | --- | --- |
| C# canonical | `v0.1.0` … `v0.2.6` | WPF build, single-platform |
| Rust port begins | session 30 | learning exercise |
| Rust port = parity goal | session 33 | "C# wins on conflict" rule |
| Rust port = daily driver | `rs-v0.3.0-rc.5` | session 41 |
| Cutover-1 (this PR) | `legacy/v0.2.6-final` tag | docs declare Rust canonical |
| Cutover-2 | TBD | C# source removed |
| Cutover-3 | TBD | tree flattened, pack id renamed |
