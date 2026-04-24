# CodeScope — Claude Code Rules

## Project

Native Windows app (.NET 10, WPF) for orchestrating parallel AI coding agent CLI sessions with Git worktree isolation.

## Technology decisions (non-negotiable)

- `.NET 10` / `C# 14` — do not downgrade
- WPF — do not suggest WinUI 3, MAUI, or Avalonia
- `EasyWindowsTerminalControl` — do not suggest xterm.js + WebView2
- Shell out to `git` — do not suggest LibGit2Sharp
- `System.Text.Json` — do not suggest Newtonsoft.Json
- `CommunityToolkit.Mvvm` — do not suggest Prism, Caliburn.Micro, ReactiveUI

## Design reference

UI/UX decisions are documented in `docs/DESIGN.md` and `docs/DECISIONS.md`. Mirror established patterns in the existing Views/ViewModels when adding features; document intentional deviations in `docs/DECISIONS.md`.

## Style

- Nullable reference types enabled; respect annotations
- File-scoped namespaces
- Primary constructors where they clean up code
- Target-typed `new()` where obvious
- `var` always unless the type isn't obvious from the right-hand side
- 4-space indent, 120 char line length guideline

## Running side-by-side with an installed build

The user develops CodeScope *inside* CodeScope — the installed v0.1.0+ build
hosts the sessions that work on this repo, so closing it to test a dev build
would kill every other project's session. Dev runs therefore need to coexist
with the installed app.

Start the dev build with the `CODESCOPE_DEV` env var set:

```pwsh
$env:CODESCOPE_DEV = "1"
dotnet run --project src/CodeScope.App
```

`NoScope.CodeScope.Core.AppPaths` resolves the env var once at process start
and redirects:

- single-instance mutex → `Global\CodeScope.SingleInstance.Dev`
- `%APPDATA%\CodeScope\` → `%APPDATA%\CodeScope.Dev\` (projects.json)
- `%LOCALAPPDATA%\CodeScope\` → `%LOCALAPPDATA%\CodeScope.Dev\` (layout.json,
  window.json, console.log, crash.log)
- window title → `CodeScope [dev] — …`

The Dev store is independent — projects opened in the dev build are separate
from the installed build's projects. Typical loop: keep v0.1.0 running with
real work, launch dev with a subset of projects (or different ones) to
exercise changes. Claude telemetry tails at `~/.claude/projects/…` are shared
by design — two FSWatchers, no state conflict.

When adding new on-disk state, thread it through `AppPaths.AppFolderName` so
dev-mode separation keeps working.

## Workflow

- **First, on any fresh session, read `docs/HANDOFF.md`** — it holds the cursor (what we were doing), current build/test status, roadmap state, open rough edges, and suggested next entry points. It is updated at the end of each session.
- Read `docs/DECISIONS.md` before proposing architecture changes
- Write the test first for `Core` services (TDD for logic, not UI)
- Keep commits focused; one concern per commit
- At the **end** of every working session, update `docs/HANDOFF.md` with the new cursor, commit SHAs, and anything a fresh session would need
- Never leave `TODO` / `FIXME` without a linked issue number
