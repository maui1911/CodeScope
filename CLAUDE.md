# CodeScope — Claude Code Rules

## Project

Cross-platform native app (Rust + [GPUI](https://www.gpui.rs/)) for
orchestrating parallel AI coding agent CLI sessions with Git worktree
isolation. Ships on Windows, macOS (arm64 + x64), and Linux x64.

> The original .NET 10 / WPF implementation was retired on 2026-05-14;
> see [ADR-0022](docs/DECISIONS.md) and
> [docs/MIGRATION-csharp-to-rust.md](docs/MIGRATION-csharp-to-rust.md).
> The last buildable C# tree is tagged `legacy/v0.2.6-final`. If you
> are reading historical PRs, expect `src/CodeScope.*/` references that
> no longer exist on `main`.

## Technology decisions (non-negotiable)

- Rust (stable toolchain) — workspace at repo-root `Cargo.toml`
- [GPUI](https://www.gpui.rs/) for the application shell — do not
  suggest egui, iced, or web frontends
- `codescope-terminal` (GPUI-native ConPTY/PTY) for terminal hosting —
  do not suggest xterm.js + WebView2 or wrapping VS Code's terminal
- Shell out to `git` — do not suggest `git2`/libgit2 bindings
- `serde_json` for on-disk state — do not introduce other JSON crates
- `self_update` crate for in-app update detection + apply; Inno Setup
  for the Windows installer. Velopack was retired in #244 after three
  release-cycle crashes; do not reintroduce it.

## Design reference

UI/UX decisions live in `docs/DESIGN.md` and `docs/DECISIONS.md`.
Mirror established patterns in the existing GPUI views and the
`AppShell` / `MainViewModel` parallel when adding features; document
intentional deviations in `docs/DECISIONS.md`.

## Style

- Idiomatic Rust 2024 (workspace pins `edition = "2024"` — needs
  Rust 1.85+). Run `cargo fmt` before committing
- `cargo clippy --workspace --all-targets` clean on changed files
- Prefer `?` over `match`/`unwrap` for error propagation; reserve
  `unwrap` for proven invariants with a one-line `// SAFETY:` style
  comment explaining why
- Module names `snake_case`, types `CamelCase`
- 4-space indent, 100 char line length guideline (Rust default)

## Running side-by-side with an installed build

The user develops CodeScope *inside* CodeScope — the installed build
hosts the sessions that work on this repo, so closing it to test a dev
build would kill every other project's session. Dev runs therefore
need to coexist with the installed app.

Start the dev build with the `CODESCOPE_DEV` env var set:

```pwsh
# Windows (PowerShell)
$env:CODESCOPE_DEV = "1"
cargo run --bin codescope
```

```bash
# macOS / Linux
CODESCOPE_DEV=1 cargo run --bin codescope
```

`codescope_core::paths::AppPaths::detect()` resolves the env var once
at process start and redirects (paths come from `AppPaths` accessor
methods — `projects_file()`, `layout_file()`, `window_file()`,
`settings_file()`):

- single-instance mutex → `Global\CodeScope.SingleInstance.Dev`
  (Windows only)
- Windows: `%APPDATA%\CodeScope\` → `%APPDATA%\CodeScope.Dev\`
  (`projects.json`, `settings.json`);
  `%LOCALAPPDATA%\CodeScope\` → `%LOCALAPPDATA%\CodeScope.Dev\`
  (`layout.json`, `window.json`, `console.log`, `crash.log`)
- Linux: `~/.config/CodeScope/` → `~/.config/CodeScope.Dev/` (config);
  `~/.local/state/CodeScope/` → `~/.local/state/CodeScope.Dev/` (state)
- macOS: `~/Library/Application Support/CodeScope/` →
  `~/Library/Application Support/CodeScope.Dev/` (both config and
  state — Apple's HIG keeps them together)
- window title → `CodeScope [dev] — …`

The Dev store is independent — projects opened in the dev build are
separate from the installed build's projects. Typical loop: keep the
installed build running with real work, launch dev with a subset of
projects (or different ones) to exercise changes. Claude telemetry
tails at `~/.claude/projects/…` are shared by design — two
FSWatchers, no state conflict.

When adding new on-disk state, thread it through the `paths` module so
dev-mode separation keeps working.

## Workflow

- **First, on any fresh session, read `docs/HANDOFF.md`** — it holds
  the cursor (what we were doing), current build/test status, roadmap
  state, open rough edges, and suggested next entry points. It is
  updated at the end of each session.
- Read `docs/DECISIONS.md` before proposing architecture changes
- Write the test first for `codescope-core` services (TDD for logic,
  not UI). Tests live next to the code they cover, behind
  `#[cfg(test)]`
- Keep commits focused; one concern per commit
- Never commit new work directly to `main` — always create a feature
  branch and open a PR (even for small changes). `main` is protected;
  pushing to it bypasses review
- At the **end** of every working session, update `docs/HANDOFF.md`
  with the new cursor, commit SHAs, and anything a fresh session would
  need
- Never leave `TODO` / `FIXME` without a linked issue number
