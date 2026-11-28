# Architecture (bot-facing summary)

This is the orientation a bot reads before touching code. It is a
*summary*, not the source of truth — `docs/ARCHITECTURE.md` and
`docs/DECISIONS.md` are. When they disagree, they win and this file is
stale; say so in your handoff instead of guessing.

## Workspace layout

| Crate / path | Role |
|---|---|
| `src/` (bin `codescope`) | GPUI shell — window, sidebar, tabs, dialogs, updater |
| `core/` (`codescope-core`) | All logic with no UI: projects, sessions, git, telemetry, settings, paths |
| `terminal/` (`codescope-terminal`) | GPUI-native ConPTY/PTY terminal host |
| `vendor/gpui-terminal` | Vendored upstream, excluded from the workspace |

The dependency arrow is `src/ -> core/` and `src/ -> terminal/`. Never
the reverse. Logic that could live in `core/` should live in `core/`,
because that is the half that can be tested.

## The pieces a bot is most likely to touch

- `core/src/projects.rs` — `ProjectsConfig`, `Project`, `Session`,
  `Worktree`. This is the on-disk shape in `projects.json`.
- `core/src/session.rs` — session lifecycle: open, soft-close, reopen,
  hard-remove, rename, retention sweep.
- `core/src/git.rs` — every git call shells out to `git`. No libgit2.
- `core/src/telemetry.rs` — vendor-neutral `SessionState` and
  `TelemetrySnapshot`; per-agent parsers live in `core/src/agents/`.
- `core/src/paths.rs` — `AppPaths::detect()`. All new on-disk state
  must be threaded through here or dev-mode separation breaks.

## Invariants

1. `core/` has no GPUI dependency.
2. Git is a subprocess, never a library binding.
3. On-disk JSON is camelCase and round-trips with the retired C# build.
4. Anything persisted goes through `paths`, never a hardcoded path.
