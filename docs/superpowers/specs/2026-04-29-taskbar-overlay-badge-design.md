# Taskbar overlay badge — design

> Issue: [#18](https://github.com/maui1911/CodeScope/issues/18) — "bolletje groen / rood bij taskbar icon"
> Date: 2026-04-29
> Status: Draft → ready for plan

## Problem

The CodeScope window can host many parallel agent sessions. When the user
alt-tabs away or minimises the window, the only signals that an agent is busy
or has just gone idle are (a) the toast on turn-complete (#16) and (b) the
in-app status dots — neither visible while the app is buried under other
windows. The user wants a Teams-style taskbar overlay: red badge with a
single-digit count of busy agents, green dot when ≥1 agent is registered and
all are idle, no overlay when there are no agent sessions.

## Behaviour

| Aggregate state                                  | Overlay shown                                  |
| ------------------------------------------------ | ---------------------------------------------- |
| 0 agent tabs (only shell tabs, or no tabs)       | none — `TaskbarItemInfo.Overlay = null`        |
| ≥1 agent tab, all `TabStatus.Idle`               | green dot, no number                           |
| 1–9 agent tabs `TabStatus.Busy`                  | red dot with the count digit                   |
| 10+ agent tabs `TabStatus.Busy`                  | red dot with `9` plus a small superscript `+`  |

"Busy" reuses the existing `TabStatus.Busy` rollup — i.e.
`ClaudeActivityState ∈ { Composing, PendingToolUse }`. Shell sessions and
soft-closed history rows are not counted; only live tabs whose
`Descriptor.AgentProfile.Id != "shell"`.

`TaskbarItemInfo.Description` is set in parallel to the overlay and used by
screen readers / hover tooltips: `"3 agents working"` / `"All agents idle"`
/ empty.

Updates fire live (no debounce) — the existing in-app dot rollup is already
push-driven from the same `Status` change events, and the taskbar should
match that cadence.

## Architecture

### `MainViewModel` aggregator (existing class, three new members)

```csharp
[ObservableProperty] private int _busyAgentCount;
[ObservableProperty] private int _agentTabCount;

private void RecomputeTaskbarBadge()
{
    var agents = 0;
    var busy = 0;
    foreach (var tab in Tabs)
    {
        if (tab.Descriptor?.AgentProfile?.Id is null or "shell") continue;
        agents++;
        if (tab.Status == TabStatus.Busy) busy++;
    }
    AgentTabCount = agents;
    BusyAgentCount = busy;
    _taskbarBadge?.Apply(busy, agents);
}
```

Triggers (existing wires already exist for the in-app dot rollup):
- `Tabs.CollectionChanged` (covers add/remove across all editor groups)
- per-tab `Status` `PropertyChanged`
- per-tab `Descriptor` change (rare — agent adoption / restore)

No recompute on drag-between-groups: `Tabs` membership is unchanged when
`MoveTabToGroup` reparents a tab.

### `TaskbarBadgeService` (new, in `CodeScope.Ui.Services`)

```csharp
public interface ITaskbarBadgeService
{
    void Apply(int busyCount, int agentTabCount);
}
```

Singleton; resolves the active `MainWindow` lazily via
`Application.Current.MainWindow`. Sets `MainWindow.TaskbarItemInfo.Overlay`
and `.Description`.

Three branches:

1. `agentTabCount == 0` → `Overlay = null; Description = ""`.
2. `agentTabCount > 0 && busyCount == 0` → green dot via
   `BuildOverlay(digit: null, plus: false, fill: Signal.Ok)`;
   description `"All agents idle"`.
3. `busyCount >= 1` → red dot via
   `BuildOverlay(digit: Math.Min(busyCount, 9).ToString(), plus: busyCount > 9, fill: Signal.Warn)`;
   description `"{busyCount} agents working"` (plural always — single-agent
   `"1 agent working"` polish acceptable but not blocking).

`BuildOverlay` is a `DrawingVisual` → `RenderTargetBitmap` rasteriser, 16×16
at 96 DPI:

- Filled `EllipseGeometry`, centre (8,8), radius 7, brush from theme
  resource (`Signal.Ok` / `Signal.Warn` — same brushes the in-app dots use).
- 1 px outer ring at 40 % black for taskbar contrast (centre 8,8, radius 7.5).
- Digit (when present): `FormattedText`, Segoe UI Variable / Inter, weight
  700, font size ~10 px, white, centred on (7,8) when `plus`, (8,8) otherwise.
- Plus (when `plus`): `FormattedText("+")`, weight 700, font size ~6 px,
  white, anchored at (12.5, 4.5) — top-right superscript inside the disc.
- `Freeze()` the resulting `BitmapSource` before assignment.

Mockup locked in `scratch/badge-mockup.html` (visual companion screenshot).

If 200 % DPI shows visible aliasing, the follow-up is to render at 32×32 and
let Windows downscale. Flagged but not blocking — Windows accepts any
`ImageSource` and 16×16 is the historical contract.

### Wiring

- `App.xaml.cs` — register `ITaskbarBadgeService → TaskbarBadgeService` as
  a singleton; pass to `MainViewModel` ctor (optional param, like
  `ISessionViewHostPool`).
- `MainWindow.xaml` — `<Window.TaskbarItemInfo><TaskbarItemInfo/></Window.TaskbarItemInfo>`.
- `MainViewModel` — call `RecomputeTaskbarBadge()` from the same handlers
  that already fire for the in-app dot rollup; no new event subscriptions.

## Tests

`tests/CodeScope.Ui.Tests/MainViewModelTaskbarBadgeTests.cs` — count logic
only (the rasteriser is hand-verified WPF rendering, consistent with the
project's "no UI tests" convention):

- 0 agent tabs → `Apply(0, 0)`.
- 1 agent tab, idle → `Apply(0, 1)`.
- 3 agent tabs, 2 busy → `Apply(2, 3)`.
- 12 agent tabs, 12 busy → `Apply(12, 12)` (service decides "9+" — VM passes
  raw count).
- Shell-only tab is excluded from `AgentTabCount` and `BusyAgentCount`.
- Soft-closed history rows are excluded (they are not in `Tabs`).
- Recompute fires when a tab's `Status` flips from Idle → Busy and back.

`ITaskbarBadgeService` is mocked with NSubstitute; tests assert the
arguments to `Apply`, not the rendered bitmap.

## Out of scope

- Per-agent colour coding (e.g. claude-blue, codex-green). The badge is a
  workspace-wide aggregate; per-agent signal is the in-app dots' job.
- Click handlers on the overlay (e.g. "click red badge → focus first busy
  tab"). Windows' `TaskbarItemInfo.Overlay` doesn't expose a click event;
  the user already has the toast (#16) for jump-to-session.
- Jump-list integration. Separate feature.
- Sound or tray-icon flash on transitions. Toasts cover this.

## Risks / open items

- **High DPI rendering** — 16×16 source might alias on 200 %+ taskbars.
  Acceptance criterion: legible on the user's 100 % display; revisit only
  if hand-test shows blurriness.
- **Many `Status` change events during a token stream.** The in-app dots
  already absorb this with no perf issue — same path, same load.
- **Agent profile id `"shell"` as the exclusion key.** If a future agent
  profile uses a different "no-agent" sentinel, the rule needs updating.
  Single source of truth lives in `AgentRegistry.BuildDefaults`.

## Acceptance

- Open CodeScope with no projects → no overlay.
- Open a project, start a Claude session → green dot appears within ~500 ms
  of the first telemetry sample.
- Type a prompt → dot turns red with `1`.
- Open a second tab, both busy → `2`.
- One tab finishes → `1`.
- Both finish → green.
- Close every tab → no overlay.
- 10+ busy → `9⁺` (single-digit width, super-script "+").
