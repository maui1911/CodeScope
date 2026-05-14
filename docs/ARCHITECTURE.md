# CodeScope — Architecture

> Updated 2026-05-14 for the Rust + GPUI build. The earlier .NET 10 /
> WPF architecture is preserved at tag `legacy/v0.2.6-final`; see
> [ADR-0022](DECISIONS.md) and
> [MIGRATION-csharp-to-rust.md](MIGRATION-csharp-to-rust.md).

## Crates

```
┌──────────────────────────────────────────────────────┐
│ codescope-rs        (GPUI shell, AppShell, dialogs)  │
├──────────────────────────────────────────────────────┤
│ codescope-terminal  (GPUI-native ConPTY/PTY view)    │
├──────────────────────────────────────────────────────┤
│ codescope-core      (models + services, no UI)       │
└──────────────────────────────────────────────────────┘
```

Dependency direction is strict: `codescope-rs` depends on
`codescope-core` and `codescope-terminal`; `codescope-core` has zero UI
references (no GPUI, no windowing, no terminal). The workspace lives
at `codescope-rs/Cargo.toml`. Cutover-3 flattens this to repo root.

## Runtime shape

```
┌──────────────┬────────────────────────────────────────┐
│              │  [Tab 1]  [Tab 2]  [Tab 3]  [+]        │
│  Sidebar     ├────────────────────────────────────────┤
│              │                                         │
│  Projects    │     codescope-terminal                  │
│  ├ Proj A    │     (pwsh -NoExit -Command "claude")    │
│  │  ├ main   │                                         │
│  │  └ feat-x │                                         │
│              ├────────────────────────────────────────┤
│              │  main • 3 changes • PR #42 (checks ✓)  │
└──────────────┴────────────────────────────────────────┘
```

The window is composed of one or more **groups**. Each group owns its
own tab strip and a single active terminal view. Layout (group widths,
which tab is active in each group, collapsed sidebar nodes) persists
to `layout.json`; the project / worktree / session tree persists to
`projects.json` and is the source of truth for rehydration
([ADR-0021](DECISIONS.md)).

## Core services

`codescope-core` modules (under `codescope-rs/core/src/`):

- `projects` — reads/writes `%APPDATA%\CodeScope\projects.json` via
  `serde_json`. Schema versioned by the `version` field; migrations
  are additive. JSON shape is byte-compatible with the retired C# build.
- `agents` — resolves agent profiles (command, args, resume flag) from
  config. Mirrors the C# `IAgentRegistry`.
- `git` — shells out to `git` via `std::process::Command`. Async-aware
  through the GPUI background executor; results returned as
  `Result<T, …>` for non-exceptional failures.
- `session_manager` — owns the lifecycle of running terminal sessions.
  Spawns the shell in a directory, hands the pty to
  `codescope-terminal`, and tears the process tree down on tab close
  (Win32 job object on Windows; `setpgid` + `killpg` on Unix).
- `claude_telemetry`, `claude_discovery`, `notifications`,
  `update_check`, `layout`, `paths`, `settings`, `window_state`,
  `attachments` — one module per concern, all UI-free.

`codescope-rs/src/` hosts the GPUI shell (`app.rs` → `AppShell`,
`view.rs`, `sidebar.rs`, dialogs, status bar). Render-state mutations
go through `cx.notify()` rather than an MVVM relay-command layer.

## Process tree cleanup

A tab hosts a shell (`pwsh.exe` on Windows; user's `SHELL` on Unix),
which spawns agent children (`claude.exe`, `codex`, etc.). Killing the
parent does not reliably kill its children.

- **Windows:** the spawned process is associated with a Win32 **job
  object** flagged `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. Closing the
  job handle on tab dispose terminates the whole tree.
- **Unix:** each tab is its own process group via `setpgid`; tab close
  calls `killpg(SIGTERM)` followed by `SIGKILL` on timeout.

Both paths live in `codescope-terminal`.

## Config schema

`%APPDATA%\CodeScope\projects.json` on Windows (resolved by
`AppPaths::projects_file()`; the config root is
`~/.config/CodeScope/` on Linux and
`~/Library/Application Support/CodeScope/` on macOS — see
`codescope-rs/core/src/paths.rs`):

```json
{
  "version": 1,
  "agents": [
    { "id": "...", "displayName": "...", "command": "...",
      "resumeArgs": [], "newSessionArgs": [], "isDefault": true }
  ],
  "projects": [
    { "id": "...", "name": "...", "path": "...",
      "defaultBranch": "main", "worktreeRoot": "...",
      "sessions": [
        { "id": "...", "worktreePath": "...", "branch": "...",
          "agentId": "...", "lastOpened": "2026-04-21T10:32:15Z",
          "closedAt": null }
      ] }
  ]
}
```

Unknown fields are preserved on roundtrip (`serde_json` with a
`flatten`ed `extra: Map<String, Value>`). `version` gates migrations.

`%LOCALAPPDATA%\CodeScope\layout.json` carries placement
(`session_placements: Vec<SessionPlacement>` — see
[ADR-0021](DECISIONS.md)).

## Threading and async

- UI work runs on the GPUI main thread; mutations go via
  `cx.spawn` / `cx.background_executor()` for off-thread work and
  `cx.update` to land results back on the UI.
- Git calls, telemetry polls, and update checks run on
  `cx.background_executor()`. They are cancellation-aware where the
  source crate supports it; otherwise short timeouts.
- File-system watchers (`notify` crate) feed channels consumed by the
  UI through `cx.spawn`.
- No blocking syscalls on the main thread — `Notification::show`
  (notify-rust) is one example punted to the background executor
  because the underlying COM / NSUserNotification call blocks.

## Error handling

- `Result<T, …>` for expected failures (git nonzero exit, file
  missing, JSON parse error).
- `anyhow::Error` at the shell layer for surfaced-to-user errors;
  typed errors inside `codescope-core` modules.
- `panic` only for genuinely exceptional cases (invalid config,
  programmer error) and is captured by the crash-log handler that
  writes `%LOCALAPPDATA%\CodeScope\crash.log`.
- Logging: `tracing` with a console sink in debug and a rolling file
  sink (`console.log`, 10 MB cap) in release.
