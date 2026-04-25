# CodeScope — Architecture

## Layers

```
┌──────────────────────────────────────────┐
│ CodeScope.App    (WPF host, DI, windows) │
├──────────────────────────────────────────┤
│ CodeScope.Ui     (ViewModels + Views)    │
├──────────────────────────────────────────┤
│ CodeScope.Core   (models + services)     │
└──────────────────────────────────────────┘
```

Dependency direction is strict: `App -> Ui -> Core`. `Core` has zero UI references (no WPF, no Wpf.Ui, no terminal control).

## Runtime shape

```
┌──────────────┬────────────────────────────────────────┐
│              │  [Tab 1]  [Tab 2]  [Tab 3]  [+]        │
│  Sidebar     ├────────────────────────────────────────┤
│  (Fase 2)    │                                         │
│              │     EasyWindowsTerminalControl         │
│  Projects    │     (pwsh -NoExit -Command "claude")   │
│  ├ Proj A    │                                         │
│  │  ├ main   │                                         │
│  │  └ feat-x │                                         │
│              ├────────────────────────────────────────┤
│              │  main • 3 changes • PR #42 (checks ✓)  │
└──────────────┴────────────────────────────────────────┘
```

## Core services

- `IProjectStore` — reads/writes `%APPDATA%\CodeScope\projects.json` (System.Text.Json). Schema versioned via `version` field; migrations are additive.
- `IAgentRegistry` — resolves agent profiles (command, args, resume flag) from config.
- `IGitService` — shells out to `git` via `Process.Start`. All methods async + cancellable. Returns `Result<T>` for non-exceptional failures.
- `ISessionManager` — owns the lifecycle of running terminal sessions. Knows how to spawn pwsh in a directory, wire a pseudo-console via `EasyWindowsTerminalControl`, and kill the whole process tree on dispose using a Win32 job object.

## Process tree cleanup

A tab hosts `pwsh.exe`, which spawns agent children (`claude.exe`, etc.). Killing the parent pwsh does not reliably kill its children. Fix: associate the whole launched process with a Win32 **job object** flagged `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. When the job handle is closed (on tab dispose), Windows terminates the entire tree.

See `CodeScope.Core/Interop/ProcessTreeKiller.cs`.

## Config schema

`%APPDATA%\CodeScope\projects.json`:

```json
{
  "version": 1,
  "agents": [ { "id": "...", "displayName": "...", "command": "...", "resumeArgs": [], "newSessionArgs": [], "isDefault": true } ],
  "projects": [ { "id": "...", "name": "...", "path": "...", "defaultBranch": "main", "worktreeRoot": "...", "sessions": [ { "id": "...", "worktreePath": "...", "branch": "...", "agentId": "...", "lastOpened": "2026-04-21T10:32:15Z" } ] } ]
}
```

Unknown fields are preserved on roundtrip. `version` gates migrations.

## Threading and async

- UI thread: WPF dispatcher only.
- Git calls: async, cancellable via `CancellationToken`, debounced for status polling.
- `ConfigureAwait(false)` in Core; UI layer uses default.
- No `.Result` / `.Wait()` anywhere.

## Error handling

- `Result<T>` pattern for expected failures (git nonzero exit, file missing).
- Exceptions only for genuinely exceptional cases (invalid config, programmer error).
- All `ILogger<T>` — console sink in Debug, file sink (rolling 10 MB) in Release.
