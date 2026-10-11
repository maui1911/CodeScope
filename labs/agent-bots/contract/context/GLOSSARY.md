# Glossary

Exact names, so nobody invents a plausible-sounding wrong one. If a
name you want is not on this list, grep for it before writing it down.

## Domain types (`codescope-core`)

| Name | Module | What it is |
|---|---|---|
| `ProjectsConfig` | `projects` | Root of `projects.json` |
| `Project` | `projects` | One repo or plain folder in the sidebar |
| `Session` | `projects` | One agent tab. Persisted *inside* the project, not in a separate store |
| `Worktree` | `projects` | One tracked git worktree of a project |
| `RetentionPolicy` | `session` | TTL + per-worktree cap for closed sessions |
| `AgentProfile` | `agent_registry` | One CLI agent: argv, resume args, session-id flag, icon, context window |
| `AgentRegistry` | `agent_registry` | Read-only set of profiles |
| `AgentId` | `agent` | Narrow enum: `Claude`, `Codex`, `Copilot`, `OpenCode`, `Pi`, `Gemini` |
| `SessionState` | `telemetry` | `Unknown` / `Idle` / `PendingToolUse` / `Busy` |
| `TelemetrySnapshot` | `telemetry` | Model, tokens, context pct, turn count |
| `GitStatus` | `git` | Branch, detached, added, removed, ahead, behind, has_upstream |
| `WorktreeInfo` | `git` | One row of `git worktree list --porcelain` |
| `BranchInfo` | `git` | One branch |
| `PullRequestInfo` | `pr` | PR number, state, URL |
| `CiStatus` | `pr` | CI rollup for a branch |
| `AppPaths` | `paths` | Resolves config/state dirs, and the dev-mode redirect |

## Terms

- **Primary worktree** — the path the project was added at. Not a
  worktree "session"; the sidebar shows it first.
- **Session ≠ worktree.** A new session opens a tab in the primary
  path. Creating a worktree is a separate, explicit action.
- **Dev mode** — `CODESCOPE_DEV=1`. Redirects every on-disk path to a
  `.Dev` sibling so a dev build can run beside the installed one.
- **Adoption** — attaching telemetry to an agent session that was
  already running when CodeScope found it.
- **Auto-type** — the launch command CodeScope types into a fresh tab.
  Its first token is how a session gets classified to an `AgentId`.

## Historical

`legacy/v0.2.6-final` is the last buildable C# tree. Names like
`MainViewModel`, `SessionManager`, `AgentRegistry.cs` refer to it. It
is reference material for *existing* behaviour only — never a parity
target for new work. On-disk data shapes still match it.
