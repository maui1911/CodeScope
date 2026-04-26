# Status-dots bugfix pass — design

> **Update during implementation:** the green state was originally drafted as
> `Ready`; renamed to `Idle` after a quick UX pass — the word matches the
> existing `StatusLabel` slug and is the conventional term for "not doing
> anything." `Busy` (red) is unchanged.


## Problem

The status dots in the sidebar, tab strip, and bottom bar don't read coherently:

1. **Selected sidebar row hides the agent state.** `SidebarView.xaml` has an `IsSelected → Accent.Primary` trigger that overrides the idle/wait fill. Selection is already conveyed by bold text + `#141414` row background + the 2px accent rail, so the dot just disappears as a status surface whenever a worktree is focused.

2. **"Active" never reaches the sidebar.** `WorktreeViewModel.DotState` returns only `rest` / `idle` / `wait`. The `TabStatus.Active` value is sidebar-blind, so a Composing agent shows green in the tree.

3. **Background tabs that are composing show as idle.** `MainViewModel.ApplyActivityToStatus` (`MainViewModel.cs:1191`) maps `Composing → Active` only when the tab is the selected one; otherwise it falls back to `Idle`. A background tab actively generating tokens reads as green/idle.

4. **"Wait" / "Active" are confusing labels.** Today: `PendingToolUse` (agent running a tool) → red pulsing "wait", `Composing` (agent generating) → blue "active" only on the focused tab, `Idle` → green. The user's mental model groups both agent-working states together and reads green as "agent is done, your turn".

## Goals

- Two visible agent states only — there is no daily-use distinction worth a third color.
- Same semantics on every surface (sidebar, tab strip, bottom bar).
- Selection visuals never hide state visuals.
- The pulse stays — it makes a working agent feel alive.

## Non-goals

- `OverviewCardState` (Active/Idle/Waiting) is **out of scope**. That enum is derived from worktree-dirtiness and PR CI status, not agent activity. Renaming it would conflate two unrelated state machines.
- PR CI failure indication in the sidebar `StatusLabel` slug is touched only enough to stop colliding with the agent state (split into a separate slug); no broader CI rollup work.

## Design

### State model

| State | Color | Pulse | Trigger |
|---|---|---|---|
| `Rest` | dim grey `#FF2A2A2A` | no | no session attached (sidebar only) |
| `Busy` | `Signal.Warn` (red) | yes | `Composing` **or** `PendingToolUse` |
| `Idle` | `Signal.Ok` (green) | no | `Idle` (turn finished, awaiting your input) |

Pulse timing: existing 1.4s halo storyboard, retargeted from `wait` to `busy`.

### Renames

- `TabStatus.Active`, `TabStatus.Wait` → **deleted**.
- `TabStatus.Idle` is kept as the green state name (covers "turn finished, awaiting your input").
- `TabStatus.Busy` → new value (covers former Active + Wait).

### Activity → status mapping (`MainViewModel.ApplyActivityToStatus`)

```
ClaudeActivityState.Composing      → TabStatus.Busy
ClaudeActivityState.PendingToolUse → TabStatus.Busy
ClaudeActivityState.Idle           → TabStatus.Idle
ClaudeActivityState.Unknown        → unchanged (preserves prior status)
```

Drops the `isSelected` branch. A background tab that is composing now also reads Busy.

### Surfaces

**`TabStatusToBrushConverter`** — collapses to two cases: `Busy → Signal.Warn`, default → `Signal.Ok`. Drops the `Accent.Primary` branch.

**`GroupStripView.xaml`** — pulse `DataTrigger` retargeted from `Status=Wait` to `Status=Busy`. No other tab-strip changes; selected-tab styling already lives on the pill background + font weight, not the dot.

**`SidebarView.xaml` worktree dot** —
- Remove the `IsSelected → Accent.Primary` trigger (lines ~356–358).
- States become `rest` / `idle` / `busy`. `busy` keeps the pulsing halo (color stays `Signal.Warn`).
- Selection styling on the row (bold text, `#141414` bg, accent rail) is unchanged.

**`WorktreeViewModel`** —
- `DotState` returns `rest` / `idle` / `busy`. `HasWaitingSession` → `HasBusySession` (any child whose `Status == TabStatus.Busy`). PR CI failure no longer bumps the dot — it gets its own slug below.
- `StatusLabel` slug rules:
  - any session Busy → `"busy"`
  - PR CI failure → `"ci!"` (was folded into "wait")
  - dirty → `"chg"`
  - ahead/behind → `"↑N ↓N"` form
  - else → `"idle"`

**`ProjectViewModel.HasWaitingChild`** → `HasBusyChild`. The collapsed-project rollup dot keeps its red color and `Signal.Warn` fill; only the property name changes.

**Status bar (`MainViewModel.StatusBar.cs:233-235` + `StatusBarView.xaml`)** —
- `StatusAgentActive` → removed.
- `StatusAgentIdle` count is kept (re-mapped to `t.Status == TabStatus.Idle`).
- `StatusAgentWait` → `StatusAgentBusy`.
- The activity-state slug helper at `MainViewModel.StatusBar.cs:195` collapses to `Busy → "busy"`, default → `"idle"`.

### Tests

The following tests need updates (rename + new Composing→Busy expectation):
- `tests/CodeScope.Ui.Tests/TabStatusTests.cs` — covers the converter.
- `tests/CodeScope.Ui.Tests/WorktreeViewModelTests.cs` — `DotState`, `HasBusySession`, `StatusLabel` slug.
- `tests/CodeScope.Ui.Tests/ProjectViewModelTests.cs` — `HasBusyChild` rollup.
- `tests/CodeScope.Ui.Tests/OverviewCardViewModelTests.cs` — only if it references `TabStatus`; the Overview enum itself is unchanged.

New test cases:
- Background tab in `Composing` state → `TabStatus.Busy` (regression for the `isSelected` branch).
- Sidebar selected worktree row → dot color tracks `DotState`, *not* selection.

## Open risks

- Some users may have built muscle memory around blue=selected. Selection is still legible (bold + bg + rail), but worth a quick visual smoke after the change to confirm the row still reads as selected without the dot.
- The `"ci!"` slug is new — existing localization-style assumptions (none today) should be checked. No localization layer exists, so the slug ships as-is.
