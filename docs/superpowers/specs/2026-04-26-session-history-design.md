# Session History per Worktree — Design

**Date:** 2026-04-26
**Branch target:** new feature branch off `main`
**Status:** implemented on `feature/session-history-per-worktree`; the *Problem* section describes pre-implementation state (auto-resume since removed — see ADR-0013).

## Problem

Closing a session today calls `SessionStore.SoftCloseSessionAsync`, which sets
`Session.ClosedAt` and persists it. The data is preserved, but the only way to
get a closed session back is the implicit auto-resume in
`MainViewModel.NewSessionAsync` (lines 370–384): clicking *New session* on a
worktree that has a matching soft-closed session restores it instead of
minting a fresh one. This means:

- The user has no UI to see *which* sessions are closed for a given worktree.
- Older closed sessions are unreachable — auto-resume picks the first match
  per `(worktree, agent)`, so a second closed Claude conversation on the same
  worktree gets shadowed.
- The "go back to that conversation I closed yesterday" workflow is invisible.

## Goals

1. **Per-worktree history** that the user can see and pick from in the
   sidebar.
2. **Worktree-scoped lifecycle** — when a worktree is removed, its history
   goes with it (matches the existing cascade in `RemoveWorktreeAsync`).
3. **Explicit reopen** — replace the implicit auto-resume in *New session*
   with an explicit "Reopen" action driven by the history surface. *New
   session* always mints fresh after this change.

## Non-goals

- No global "recently closed" surface. History is always per-worktree.
- No retention cap in this pass (worktree-remove is the cleanup).
- No keyboard shortcut. The worktree context menu provides a one-click
  "reopen most recent" for the keyboard-equivalent ergonomics.
- No preservation of sessions whose worktree was deleted. Worktree removal
  cascades, period.

## Design

### Architecture (UI-only — no Core changes)

> **Implementation note:** during implementation a new
> `SessionStoreChange.SessionSoftClosed` event was added to `CodeScope.Core`
> to give `SidebarViewModel.RemoveSession` a race-free signal that
> distinguishes a soft-close demotion from a hard-remove. The original design
> re-queried the store inside the `SessionRemoved` handler, which opened a
> small race window; the dedicated event closes it. All other claims below
> remain accurate.

All the data plumbing already exists:
`Session.ClosedAt`, `SessionStore.SoftCloseSessionAsync` /
`RestoreSessionAsync`, persisted via `ProjectStore`, with cascading
worktree-remove cleanup at `SessionStore.cs:444`.

Changes are confined to `CodeScope.Ui`:

- **`SidebarViewModel.StoreSync`** — alongside the existing
  `where ClosedAt is null` projection, add a second projection that maps the
  soft-closed sessions onto a new `WorktreeViewModel.History`
  `ObservableCollection<HistorySessionViewModel>`. The `SessionAdded` /
  `SessionRemoved` events handle restore and hard-remove; a new
  `SessionSoftClosed` event (added to Core during implementation) handles
  demotion from live to history race-free.
- **`WorktreeViewModel`** — new `History` collection plus
  `IsHistoryExpanded` (default `false`).
- **`SidebarView.xaml`** — extra disclosure level inside the worktree row,
  rendered only when `History.Count > 0`.
- **`MainViewModel.NewSessionAsync`** — remove the auto-resume block
  (lines 370–384). New session always mints fresh.
- **`MainViewModel.ReopenClosedSessionAsync(string sessionId)`** — new
  command. Wraps `RestoreSessionAsync` and spawns a resume-flavoured
  descriptor. Reuses the body of `TryRestoreSessionAsync` (line 559),
  refactored so the worktree lookup comes from the caller.
- **Worktree context menu** — extra item *Reopen most recent closed
  session*, enabled only when history is non-empty.
- **`CloseWorktreeSessionsAsync` / `RestoreClosedWorktreeSessionsAsync`**
  (the worktree-remove cascade rollback) — untouched. Different code path
  with its own snapshot/rollback semantics.
- **Drag-between-groups** (`MainViewModel.MoveTabToGroup`) — untouched.
  Verified to live on a different code path that does not soft-close /
  restore; the current Claude `--resume <id>` cross-group behavior keeps
  working.

### Sidebar surface

```
▾ Project
  ▾ Worktree (feature/foo · main)
      ● open session — claude (active)
      ● open session — shell
      ▾ History (3)
          ○ "scrappy refactor" — claude · 2h ago
          ○ "experimental tests" — claude · yesterday
          ○ shell session — 3d ago
```

- `History (N)` disclosure is **collapsed by default** and **only rendered
  when `Count > 0`** — zero visual cost on fresh worktrees.
- History rows reuse the open-session row template with two visual deltas:
  - dim opacity (~0.55, lining up with existing muted-state tokens —
    final token chosen at implementation time, documented if it deviates)
  - status dot rendered as outline (no activity FSM — it's a dead session)
  - relative timestamp suffix (`2h ago` / `yesterday` / `3d ago` /
    `Mar 14`) — reuse the notifications-popover formatter if available,
    otherwise a small local helper
- **Sort:** descending by `ClosedAt` (most recent on top).

### Interactions

| Action on history row | Result |
|---|---|
| Double-click | Reopen (default action) |
| Single-click | Select-only — no spawn (matches open-session-row behaviour, prevents accidental reopen on right-click) |
| Right-click → **Reopen** | Same as double-click. Bold/default item. |
| Right-click → **Rename** | Reuses `RenameSessionAsync` (already works on closed sessions). |
| Right-click → **Remove from history** | Hard delete via `RemoveSessionAsync`. Not recoverable. |

| Action on worktree row | Result |
|---|---|
| Right-click → **Reopen most recent closed session** | Opens the top history entry. Disabled when history is empty. |

### Shell sessions in history

Shell sessions have no `AgentSessionId` (nothing to resume). They **do**
appear in history, and reopening = respawn pwsh in
`stored.WorktreePath` with the original `DisplayName` carried over. No
`--resume` flag, no transcript (there was none to begin with). This keeps
the model consistent: anything you closed, you can find back.
Implementation: in `ReopenClosedSessionAsync`, branch on
`stored.AgentId == ShellSentinel` and call
`_sessionManager.CreateShellSession(stored.WorktreePath, stored.Id)`.

### Behaviour change to document

The implicit auto-resume in *New session* goes away. For returning users
the workflow becomes: *Reopen* via history (or the worktree context-menu
shortcut) instead of *New session*. Document in the commit message and
add an entry to `docs/DECISIONS.md`.

## Testing

- **Core (TDD-style):** add one regression test to `SessionStoreTests`
  asserting that `RemoveWorktreeAsync` cascades and removes that
  worktree's closed sessions too. Existing soft-close / restore tests
  (lines 606–689) stay as-is.
- **UI:** per project convention (Core-only test project), no automated
  UI tests. Smoke via `dotnet build` / `dotnet test` plus a wpf-cli pass
  to verify:
  - history node renders only when count > 0
  - double-click + right-click → Reopen restores the tab and resumes the
    Claude transcript
  - drag-between-groups for an open Claude tab still works (unchanged)
  - removing a worktree clears its history (cascade)

## Implementation order

One commit per step, kept narrow:

1. Add the `RemoveWorktreeAsync` cascade-clears-closed-sessions regression
   test.
2. Remove the auto-resume block in `MainViewModel.NewSessionAsync` and add
   a `docs/DECISIONS.md` entry.
3. Wire `WorktreeViewModel.History` + `SidebarViewModel.StoreSync`
   projection.
4. Add the `SidebarView.xaml` history disclosure + dim styling.
5. Add `ReopenClosedSessionAsync` + double-click + history-row context
   menu (Reopen / Rename / Remove from history).
6. Add the worktree context-menu *Reopen most recent closed session* item.
7. Update `docs/HANDOFF.md`.

## Risks / open ends

- **Dim-opacity token choice** finalised at implementation time; documented
  in the commit if a new token has to be introduced rather than reusing an
  existing muted-foreground token.
- **Drag-between-groups regression** is unlikely (different code path) but
  explicitly verified in the wpf-cli smoke pass.
- **Concurrent edge case:** if a user closes a session and another fast
  click triggers reopen before the persistence round-trip, the existing
  `SoftCloseSessionAsync` rollback (`SessionStore.cs:208–222`) and
  idempotency guard already cover this; no new locking needed.
