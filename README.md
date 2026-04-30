# CodeScope

**A native Windows command center for AI coding agents.**

CodeScope runs Claude Code, Codex, Copilot, OpenCode, Pi and any other CLI agent as parallel, worktree-isolated sessions inside a single WPF window — with tabbed terminals, split editor groups, a live git overview, and a keyboard-first command palette.

![CodeScope main window](docs/screenshots/hero.png)

---

## Why

Agents are fast enough that one at a time is now the bottleneck. Running five parallel `claude` sessions from Windows Terminal means juggling five shells, five branches, and five mental stacks. CodeScope gives each agent its own git worktree, its own tab, its own notification status, and surfaces the state of every session at a glance:

- **Worktree isolation** — every session starts with `git worktree add -b` under the hood, so agents never step on each other's branches.
- **Native Windows** — WPF + Wpf.Ui chrome, ConPTY-backed terminals via `EasyWindowsTerminalControl`. No Electron, no WebView2.
- **Bring your own agent** — anything on `PATH` works: `claude`, `codex`, `opencode`, raw `pwsh`. Zero translation layer.

## Features

### Split editor groups

Snap a worktree into its own workspace with `Ctrl+\`. Two agents, side by side, each with their own terminal and tab strip.

![Two editor groups side by side](docs/screenshots/split-groups.png)

### Command palette

`Ctrl+K` / `Ctrl+Shift+P` opens the palette. Every menu action is keyboard-reachable — no mouse required for the common paths.

![Command palette](docs/screenshots/command-palette.png)

### Overview

`Ctrl+Shift+O` flips the workspace into a grid view: every worktree across every project, with its active sessions, dirty state, open PR and CI status at a glance. Filter by `Active` / `Idle` / `Waiting` to find the agent that needs your attention.

![Overview grid](docs/screenshots/overview.png)

### Also in the box

- **Sidebar tree** — Projects → Worktrees → Sessions, drag-and-drop to add a project, F2 to rename.
- **Per-tab Claude telemetry** — turns, last-activity, context-window usage pulled straight from the `~/.claude/projects` JSONL tail.
- **Pull-request awareness** — GitHub and Gitea, polled on a back-off, CI rollup on each worktree row.
- **Notifications deck** — agent "waiting on user" states surface as Windows toasts; unread counter in the status bar.
- **Multi-pane layout persisted** — groups, tabs, widths, focus — all restored on restart.

## Install

Grab the latest release from the [**Releases page**](https://github.com/maui1911/CodeScope/releases/latest):

| Asset | What |
|---|---|
| **`CodeScope-win-Setup.exe`** | One-click installer (recommended). Installs to `%LocalAppData%\CodeScope`, auto-updates via Velopack. |
| **`CodeScope-win-Portable.zip`** | Standalone zip — extract and run `CodeScope.exe`. No install, no auto-update. |

### Requirements

- Windows 10 22H2+ or Windows 11
- `git` on `PATH`
- At least one agent CLI on `PATH` (`claude`, `codex`, `copilot`, `opencode`, `pi`, etc.)

> **No .NET SDK needed** — the release is self-contained.

## Build from source

Only needed if you want to contribute or hack on CodeScope itself.

```pwsh
# Prerequisites: .NET 10 SDK (10.0.100+), git
dotnet restore
dotnet build
dotnet run --project src/CodeScope.App
```

Run the tests:

```pwsh
dotnet test
```

Publish a release binary:

```pwsh
dotnet publish src/CodeScope.App -c Release -r win-x64 `
    -p:PublishSingleFile=true -p:PublishReadyToRun=true
```

## Keyboard shortcuts

| | |
|-|-|
| `Ctrl+T` | New session in focused group |
| `Ctrl+W` | Close tab (twice to collapse an empty group) |
| `Ctrl+\` | Split the focused group right |
| `Alt+←` / `Alt+→` | Cycle focus between groups |
| `Ctrl+Tab` / `Ctrl+Shift+Tab` | Next / previous tab |
| `Ctrl+1`…`Ctrl+9` | Jump to tab by index |
| `Ctrl+K`, `Ctrl+Shift+P` | Command palette |
| `Ctrl+Shift+O` | Toggle overview |
| `Ctrl+D` | Toggle diff panel |
| `Ctrl+F` | Focus sidebar filter |
| `Ctrl+Shift+Enter` | Open selected worktree in a new group |
| `F2` | Rename session / worktree |
| `F5` | Refresh all |

## Repository layout

```
src/
  CodeScope.App/     WPF host — App.xaml, MainWindow, DI composition root
  CodeScope.Core/    Pure logic — services, models, no UI references
  CodeScope.Ui/      ViewModels, Views, Dialogs, Converters
tests/
  CodeScope.Core.Tests/
docs/
  DESIGN.md          Design tokens
  DECISIONS.md       ADRs
  HANDOFF.md         Rolling cursor between working sessions
  screenshots/       README imagery
```

The `NoScope.CodeScope.*` CLR namespace pairs with the `CodeScope` binary name.

## Status

Pre-1.0. The core session / worktree / PR workflows are stable and used daily; the design system is in its final pass and the release story (Velopack, auto-update) is the current milestone.

See `docs/HANDOFF.md` for the live status cursor and `docs/DECISIONS.md` for the architectural record.

## Contributing

- Conventional commits (`feat:`, `fix:`, `chore:`, `refactor:`, `test:`, `docs:`).
- Read `CLAUDE.md` and `docs/DECISIONS.md` before proposing architecture changes.
- PRs / issues / code / comments in English.

## License

[**FSL-1.1-ALv2**](LICENSE) — Functional Source License, Version 1.1, with Apache 2.0 future grant.

You can read, fork, self-host, and contribute today. Commercial use that competes with CodeScope is restricted while the license is active; each release automatically converts to Apache 2.0 two years after it's published.
