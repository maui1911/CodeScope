# Taskbar overlay badge — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement issue #18 — show a green dot on the Windows taskbar icon when ≥1 agent tab is registered and all idle, a red badge with the busy-count digit when ≥1 busy, "9⁺" above 9, no overlay when there are no agent tabs.

**Architecture:** Aggregate `TabStatus` across `MainViewModel.AllTabs`, exclude `shell` profile, push the count into a new `ITaskbarBadgeService` which rasterises a 16×16 `BitmapSource` via `DrawingVisual` + `RenderTargetBitmap` and assigns it to `MainWindow.TaskbarItemInfo.Overlay`. Reuses the existing per-tab `Status` `PropertyChanged` plumbing in `MainViewModel.StatusBar.cs` — no new event subscriptions needed.

**Tech Stack:** .NET 10, C# 14, WPF (`System.Windows.Shell.TaskbarItemInfo`, `RenderTargetBitmap`, `DrawingVisual`, `FormattedText`), CommunityToolkit.Mvvm, NSubstitute / xUnit / FluentAssertions for tests.

**Spec:** `docs/superpowers/specs/2026-04-29-taskbar-overlay-badge-design.md`

**File structure**

| Path                                                                      | Action  | Responsibility                                                                                            |
| ------------------------------------------------------------------------- | ------- | --------------------------------------------------------------------------------------------------------- |
| `src/CodeScope.Ui/Services/ITaskbarBadgeService.cs`                       | create  | One-method contract: `void Apply(int busyCount, int agentTabCount);`                                      |
| `src/CodeScope.Ui/Services/TaskbarBadgeService.cs`                        | create  | WPF rasteriser; sets `MainWindow.TaskbarItemInfo.Overlay` + `Description`                                 |
| `src/CodeScope.App/MainWindow.xaml`                                       | modify  | Add `<Window.TaskbarItemInfo><TaskbarItemInfo/></Window.TaskbarItemInfo>` once                            |
| `src/CodeScope.App/App.xaml.cs`                                           | modify  | Register `ITaskbarBadgeService` singleton; pass to `MainViewModel`                                        |
| `src/CodeScope.Ui/ViewModels/MainViewModel.cs`                            | modify  | Optional ctor param `ITaskbarBadgeService? taskbarBadge`; field `_taskbarBadge`                           |
| `src/CodeScope.Ui/ViewModels/MainViewModel.TaskbarBadge.cs`               | create  | Partial: `BusyAgentCount`, `AgentTabCount` derived props + `RecomputeTaskbarBadge()`                       |
| `src/CodeScope.Ui/ViewModels/MainViewModel.StatusBar.cs`                  | modify  | One-line addition inside `RaiseStatusBarChanged` to also call `RecomputeTaskbarBadge()`                    |
| `tests/CodeScope.Ui.Tests/MainViewModelTaskbarBadgeTests.cs`              | create  | Aggregator unit tests — count logic, shell exclusion, recompute on Status change                          |

---

### Task 1: Service contract and skeleton

**Files:**
- Create: `src/CodeScope.Ui/Services/ITaskbarBadgeService.cs`
- Create: `src/CodeScope.Ui/Services/TaskbarBadgeService.cs`

- [ ] **Step 1: Create the interface**

`src/CodeScope.Ui/Services/ITaskbarBadgeService.cs`:

```csharp
namespace NoScope.CodeScope.Ui.Services;

/// <summary>
/// Updates the application's taskbar overlay icon to reflect aggregate agent activity.
/// </summary>
public interface ITaskbarBadgeService
{
    /// <summary>
    /// Apply a new badge state. The service decides the visual:
    /// <list type="bullet">
    ///   <item><c>agentTabCount == 0</c> → no overlay (cleared).</item>
    ///   <item><c>busyCount == 0 &amp;&amp; agentTabCount &gt; 0</c> → green dot.</item>
    ///   <item><c>busyCount &gt;= 1</c> → red disc with the busy digit; <c>busyCount &gt; 9</c> renders <c>9⁺</c>.</item>
    /// </list>
    /// </summary>
    void Apply(int busyCount, int agentTabCount);
}
```

- [ ] **Step 2: Create the implementation skeleton (no rendering yet)**

`src/CodeScope.Ui/Services/TaskbarBadgeService.cs`:

```csharp
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace NoScope.CodeScope.Ui.Services;

/// <summary>
/// Default <see cref="ITaskbarBadgeService"/> implementation. Looks up the active
/// <see cref="Application.MainWindow"/> on each call and assigns
/// <c>TaskbarItemInfo.Overlay</c> / <c>Description</c>. Rendering is pure WPF —
/// <see cref="DrawingVisual"/> rasterised via <see cref="RenderTargetBitmap"/>.
/// </summary>
public sealed class TaskbarBadgeService : ITaskbarBadgeService
{
    public void Apply(int busyCount, int agentTabCount)
    {
        var win = Application.Current?.MainWindow;
        if (win is null) { return; }
        if (win.TaskbarItemInfo is null) { win.TaskbarItemInfo = new System.Windows.Shell.TaskbarItemInfo(); }

        if (agentTabCount == 0)
        {
            win.TaskbarItemInfo.Overlay = null;
            win.TaskbarItemInfo.Description = string.Empty;
            return;
        }

        if (busyCount == 0)
        {
            win.TaskbarItemInfo.Overlay = BuildOverlay(digit: null, plus: false, fillKey: "Signal.Ok");
            win.TaskbarItemInfo.Description = "All agents idle";
            return;
        }

        var capped = busyCount > 9 ? "9" : busyCount.ToString();
        win.TaskbarItemInfo.Overlay = BuildOverlay(digit: capped, plus: busyCount > 9, fillKey: "Signal.Warn");
        win.TaskbarItemInfo.Description = $"{busyCount} agents working";
    }

    private static BitmapSource BuildOverlay(string? digit, bool plus, string fillKey)
    {
        // Filled in Task 3.
        var rt = new RenderTargetBitmap(16, 16, 96, 96, PixelFormats.Pbgra32);
        rt.Freeze();
        return rt;
    }
}
```

- [ ] **Step 3: Build to verify both files compile**

Run: `dotnet build src/CodeScope.Ui/CodeScope.Ui.csproj -c Debug`
Expected: PASS, no errors.

- [ ] **Step 4: Commit**

```bash
git add src/CodeScope.Ui/Services/ITaskbarBadgeService.cs src/CodeScope.Ui/Services/TaskbarBadgeService.cs
git commit -m "feat(ui): scaffold ITaskbarBadgeService"
```

---

### Task 2: Aggregator partial on `MainViewModel`

**Files:**
- Create: `src/CodeScope.Ui/ViewModels/MainViewModel.TaskbarBadge.cs`
- Modify: `src/CodeScope.Ui/ViewModels/MainViewModel.cs` (ctor + field only)
- Modify: `src/CodeScope.Ui/ViewModels/MainViewModel.StatusBar.cs` (one extra call)

- [ ] **Step 1: Add the failing test file**

Create `tests/CodeScope.Ui.Tests/MainViewModelTaskbarBadgeTests.cs`:

```csharp
using FluentAssertions;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.Services;
using NoScope.CodeScope.Ui.ViewModels;
using NSubstitute;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class MainViewModelTaskbarBadgeTests
{
    [Fact]
    public void EmptyWorkspace_TriggersClear()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var vm = NewMainViewModel(badge);

        // No tabs added — first refresh fires from ctor / hook setup.
        vm.RecomputeTaskbarBadgeForTests();

        badge.Received().Apply(0, 0);
    }

    [Fact]
    public void IdleAgentTab_GreenSignal()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var vm = NewMainViewModel(badge);
        vm.Tabs.Add(NewAgentTab("claude", TabStatus.Idle));

        vm.RecomputeTaskbarBadgeForTests();

        badge.Received().Apply(0, 1);
    }

    [Fact]
    public void TwoBusyOfThree_RedTwo()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var vm = NewMainViewModel(badge);
        vm.Tabs.Add(NewAgentTab("claude", TabStatus.Busy));
        vm.Tabs.Add(NewAgentTab("codex", TabStatus.Busy));
        vm.Tabs.Add(NewAgentTab("pi", TabStatus.Idle));

        vm.RecomputeTaskbarBadgeForTests();

        badge.Received().Apply(2, 3);
    }

    [Fact]
    public void TwelveBusy_RawCountPassedThrough()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var vm = NewMainViewModel(badge);
        for (var i = 0; i < 12; i++) { vm.Tabs.Add(NewAgentTab("claude", TabStatus.Busy)); }

        vm.RecomputeTaskbarBadgeForTests();

        badge.Received().Apply(12, 12);
    }

    [Fact]
    public void ShellTab_NotCounted()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var vm = NewMainViewModel(badge);
        vm.Tabs.Add(NewAgentTab(MainViewModel.ShellSentinel, TabStatus.Busy));
        vm.Tabs.Add(NewAgentTab("claude", TabStatus.Idle));

        vm.RecomputeTaskbarBadgeForTests();

        badge.Received().Apply(0, 1);
    }

    [Fact]
    public void StatusFlip_TriggersRecompute()
    {
        var badge = Substitute.For<ITaskbarBadgeService>();
        var vm = NewMainViewModel(badge);
        var tab = NewAgentTab("claude", TabStatus.Idle);
        vm.Tabs.Add(tab);
        badge.ClearReceivedCalls();

        tab.Status = TabStatus.Busy;

        badge.Received().Apply(1, 1);
    }

    private static MainViewModel NewMainViewModel(ITaskbarBadgeService badge)
    {
        var sm = Substitute.For<ISessionManager>();
        var store = Substitute.For<ISessionStore>();
        var agents = Substitute.For<IAgentRegistry>();
        agents.GetAll().Returns(Array.Empty<AgentProfile>());
        var vm = new MainViewModel(
            sm, store, agents, NullLogger<MainViewModel>.Instance,
            pickFolder: () => null,
            taskbarBadge: badge);
        return vm;
    }

    private static SessionTabViewModel NewAgentTab(string agentId, TabStatus status)
    {
        var desc = new SessionDescriptor(
            Id: Guid.NewGuid().ToString(),
            DisplayName: "tab",
            WorkingDirectory: "C:/work",
            AgentProfile: agentId == MainViewModel.ShellSentinel
                ? null
                : new AgentProfile(Id: agentId, DisplayName: agentId, Command: agentId,
                    Args: Array.Empty<string>(), ResumeArgs: Array.Empty<string>(),
                    ResumeByIdArgs: Array.Empty<string>(), SessionIdFlag: null, Icon: null,
                    ContextWindowTokens: 0));
        var tab = new SessionTabViewModel(desc) { AgentId = agentId };
        tab.Status = status;
        return tab;
    }
}
```

> **Note:** the exact `SessionDescriptor` / `AgentProfile` ctor shapes vary by codebase generation — match the records' current signatures if compiler complains. The test depends only on `agentId` (or null) being attached to the tab so the aggregator can filter.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `dotnet test tests/CodeScope.Ui.Tests/CodeScope.Ui.Tests.csproj --filter "FullyQualifiedName~MainViewModelTaskbarBadgeTests" -c Debug`
Expected: FAIL — compile errors (`taskbarBadge` ctor param, `RecomputeTaskbarBadgeForTests` method don't exist yet).

- [ ] **Step 3: Add the partial file with derived properties**

Create `src/CodeScope.Ui/ViewModels/MainViewModel.TaskbarBadge.cs`:

```csharp
using System.Linq;
using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Window-level aggregation that drives <see cref="ITaskbarBadgeService"/>. Reuses the
/// existing tab-status hooks in <see cref="MainViewModel"/>.<see cref="HookStatusBarSources"/>;
/// the call site lives inside <see cref="RaiseStatusBarChanged"/> so we don't subscribe twice
/// to the same <see cref="SessionTabViewModel.Status"/> events.
/// </summary>
public sealed partial class MainViewModel
{
    /// <summary>Number of <see cref="AllTabs"/> whose <see cref="SessionTabViewModel.Status"/> is <see cref="TabStatus.Busy"/>, excluding shell tabs.</summary>
    public int BusyAgentCount => AllTabs.Count(IsAgentBusy);

    /// <summary>Number of <see cref="AllTabs"/> bound to a real agent profile (not shell).</summary>
    public int AgentTabCount => AllTabs.Count(IsAgentTab);

    private static bool IsAgentTab(SessionTabViewModel t)
        => !string.IsNullOrEmpty(t.AgentId)
           && !string.Equals(t.AgentId, ShellSentinel, System.StringComparison.OrdinalIgnoreCase);

    private static bool IsAgentBusy(SessionTabViewModel t)
        => IsAgentTab(t) && t.Status == TabStatus.Busy;

    private void RecomputeTaskbarBadge()
    {
        _taskbarBadge?.Apply(BusyAgentCount, AgentTabCount);
        OnPropertyChanged(nameof(BusyAgentCount));
        OnPropertyChanged(nameof(AgentTabCount));
    }

    /// <summary>Test-only entry point — production code reaches the same path through <c>RaiseStatusBarChanged</c>.</summary>
    internal void RecomputeTaskbarBadgeForTests() => RecomputeTaskbarBadge();
}
```

- [ ] **Step 4: Add the field + ctor param to `MainViewModel.cs`**

In `src/CodeScope.Ui/ViewModels/MainViewModel.cs`, add a field below the existing private fields (around line 40):

```csharp
private readonly ITaskbarBadgeService? _taskbarBadge;
```

Add a new optional ctor parameter to the existing constructor signature. The current ctor (around line 42) ends with `IIdleToastNotifier? idleNotifier = null)` — append a new optional param after it:

```csharp
        IIdleToastNotifier? idleNotifier = null,
        ITaskbarBadgeService? taskbarBadge = null)
```

Inside the ctor body (after the existing `_idleNotifier = idleNotifier;` line), add:

```csharp
        _taskbarBadge = taskbarBadge;
```

- [ ] **Step 5: Wire the recompute call in `MainViewModel.StatusBar.cs`**

Inside `RaiseStatusBarChanged()` (in `src/CodeScope.Ui/ViewModels/MainViewModel.StatusBar.cs`), add a single call at the end (after the last `OnPropertyChanged(...)`):

```csharp
    private void RaiseStatusBarChanged()
    {
        // ... existing OnPropertyChanged calls unchanged ...
        OnPropertyChanged(nameof(StatusTurnDurationVisible));
        RecomputeTaskbarBadge();
    }
```

> Why here: `HookStatusBarSources` already subscribes to per-tab `Status` `PropertyChanged`, group `Tabs.CollectionChanged`, and `Groups.CollectionChanged`. Every transition that should refresh the badge already fires `RaiseStatusBarChanged`. No new hooks needed.

- [ ] **Step 6: Run the tests to verify they pass**

Run: `dotnet test tests/CodeScope.Ui.Tests/CodeScope.Ui.Tests.csproj --filter "FullyQualifiedName~MainViewModelTaskbarBadgeTests" -c Debug`
Expected: PASS — six tests green.

> If the `StatusFlip_TriggersRecompute` test fails because `RaiseStatusBarChanged` isn't reachable in unit tests (no Sidebar attached), call `HookStatusBarSources()` once at the top of the test helper after `vm.AttachSidebar(...)`. If `MainViewModel` doesn't expose a public `Sidebar` factory, leave that single test relying on the `RecomputeTaskbarBadgeForTests()` direct call and remove the auto-fire assertion — flag it in the commit message.

- [ ] **Step 7: Commit**

```bash
git add src/CodeScope.Ui/ViewModels/MainViewModel.TaskbarBadge.cs src/CodeScope.Ui/ViewModels/MainViewModel.cs src/CodeScope.Ui/ViewModels/MainViewModel.StatusBar.cs tests/CodeScope.Ui.Tests/MainViewModelTaskbarBadgeTests.cs
git commit -m "feat(ui): MainViewModel aggregates BusyAgentCount → ITaskbarBadgeService"
```

---

### Task 3: Implement the WPF rasteriser

**Files:**
- Modify: `src/CodeScope.Ui/Services/TaskbarBadgeService.cs`

- [ ] **Step 1: Replace the placeholder `BuildOverlay`**

Replace the body of `TaskbarBadgeService` with the full rendering implementation:

```csharp
using System.Globalization;
using System.Windows;
using System.Windows.Media;
using System.Windows.Media.Imaging;

namespace NoScope.CodeScope.Ui.Services;

public sealed class TaskbarBadgeService : ITaskbarBadgeService
{
    public void Apply(int busyCount, int agentTabCount)
    {
        var win = Application.Current?.MainWindow;
        if (win is null) { return; }
        if (win.TaskbarItemInfo is null) { win.TaskbarItemInfo = new System.Windows.Shell.TaskbarItemInfo(); }

        if (agentTabCount == 0)
        {
            win.TaskbarItemInfo.Overlay = null;
            win.TaskbarItemInfo.Description = string.Empty;
            return;
        }

        if (busyCount == 0)
        {
            win.TaskbarItemInfo.Overlay = BuildOverlay(digit: null, plus: false, fillKey: "Signal.Ok");
            win.TaskbarItemInfo.Description = "All agents idle";
            return;
        }

        var capped = busyCount > 9 ? "9" : busyCount.ToString(CultureInfo.InvariantCulture);
        win.TaskbarItemInfo.Overlay = BuildOverlay(digit: capped, plus: busyCount > 9, fillKey: "Signal.Warn");
        win.TaskbarItemInfo.Description = busyCount == 1 ? "1 agent working" : $"{busyCount} agents working";
    }

    private static BitmapSource BuildOverlay(string? digit, bool plus, string fillKey)
    {
        var fill = (Application.Current?.TryFindResource(fillKey) as Brush) ?? Brushes.Red;
        var ring = new SolidColorBrush(Color.FromArgb(102, 0, 0, 0)); // ~40% black
        ring.Freeze();

        var visual = new DrawingVisual();
        using (var dc = visual.RenderOpen())
        {
            // Disc + ring centred at (8,8). Inner radius 7 for the fill, outer 7.5 for a 1 px contrast ring.
            dc.DrawEllipse(fill, null, new Point(8, 8), 7, 7);
            dc.DrawEllipse(null, new Pen(ring, 1), new Point(8, 8), 7.5, 7.5);

            if (digit is not null)
            {
                var typeface = new Typeface(
                    new FontFamily("Segoe UI Variable, Segoe UI"),
                    FontStyles.Normal, FontWeights.Bold, FontStretches.Normal);

                var digitText = new FormattedText(
                    digit,
                    CultureInfo.InvariantCulture, FlowDirection.LeftToRight,
                    typeface, emSize: 10, Brushes.White, pixelsPerDip: 1.0)
                { TextAlignment = TextAlignment.Center };

                // Centre the digit slightly left of (8,8) when "+" is also drawn so the pair reads centred.
                var dx = plus ? 7.0 : 8.0;
                var dy = 8.0 - (digitText.Height / 2);
                dc.DrawText(digitText, new Point(dx, dy));

                if (plus)
                {
                    var plusText = new FormattedText(
                        "+",
                        CultureInfo.InvariantCulture, FlowDirection.LeftToRight,
                        typeface, emSize: 6, Brushes.White, pixelsPerDip: 1.0)
                    { TextAlignment = TextAlignment.Center };
                    var px = 12.5;
                    var py = 4.5 - (plusText.Height / 2);
                    dc.DrawText(plusText, new Point(px, py));
                }
            }
        }

        var rt = new RenderTargetBitmap(16, 16, 96, 96, PixelFormats.Pbgra32);
        rt.Render(visual);
        rt.Freeze();
        return rt;
    }
}
```

- [ ] **Step 2: Build to verify**

Run: `dotnet build src/CodeScope.Ui/CodeScope.Ui.csproj -c Debug`
Expected: PASS, no errors.

- [ ] **Step 3: Commit**

```bash
git add src/CodeScope.Ui/Services/TaskbarBadgeService.cs
git commit -m "feat(ui): TaskbarBadgeService renders 16x16 overlay via DrawingVisual"
```

---

### Task 4: Wire DI + XAML

**Files:**
- Modify: `src/CodeScope.App/MainWindow.xaml`
- Modify: `src/CodeScope.App/App.xaml.cs`

- [ ] **Step 1: Declare `TaskbarItemInfo` in MainWindow**

In `src/CodeScope.App/MainWindow.xaml`, immediately after the closing `</ui:FluentWindow.Resources>` tag (or anywhere inside `<ui:FluentWindow>` outside `Resources`), add:

```xml
    <ui:FluentWindow.TaskbarItemInfo>
        <TaskbarItemInfo />
    </ui:FluentWindow.TaskbarItemInfo>
```

> The element is empty by design — the service mutates `Overlay` / `Description` at runtime. Declaring it once in XAML guarantees `Window.TaskbarItemInfo` is non-null on first paint, so the service's `if (win.TaskbarItemInfo is null) ...` branch only fires for design-time / test scenarios.

- [ ] **Step 2: Register the service in DI**

In `src/CodeScope.App/App.xaml.cs`, add a registration alongside the existing UI singletons (around line 127, near `ISessionViewHostPool`):

```csharp
                services.AddSingleton<NoScope.CodeScope.Ui.Services.ITaskbarBadgeService,
                    NoScope.CodeScope.Ui.Services.TaskbarBadgeService>();
```

In the `MainViewModel` factory just below (around line 152), append the new service to the constructor argument list — the call ends with `sp.GetRequiredService<NoScope.CodeScope.Ui.Services.IIdleToastNotifier>())`. Add a comma after that argument and add:

```csharp
                        sp.GetRequiredService<NoScope.CodeScope.Ui.Services.ITaskbarBadgeService>());
```

- [ ] **Step 3: Build the full solution**

Run: `dotnet build CodeScope.sln -c Debug`
Expected: PASS — solution compiles cleanly.

- [ ] **Step 4: Run the full test suite**

Run: `dotnet test CodeScope.sln -c Debug`
Expected: PASS — every existing test plus the six new ones in `MainViewModelTaskbarBadgeTests`.

- [ ] **Step 5: Commit**

```bash
git add src/CodeScope.App/MainWindow.xaml src/CodeScope.App/App.xaml.cs
git commit -m "feat(app): wire TaskbarBadgeService into MainWindow + DI"
```

---

### Task 5: Hand-verification (no auto-test)

**Files:** none

- [ ] **Step 1: Run the dev build side-by-side**

```pwsh
$env:CODESCOPE_DEV = "1"
dotnet run --project src/CodeScope.App
```

- [ ] **Step 2: Verify the matrix**

Tick each visually against the dev build's taskbar icon:

- Dev build launched with no projects → no overlay.
- Add a project, start a Claude session → green dot within ~500 ms of first telemetry.
- Type a prompt that triggers a long response → dot turns red with `1`.
- Open a second tab in another worktree, start it → red `2`.
- Let one finish → red `1`.
- Let both finish → green dot.
- Close both tabs → no overlay.
- (Optional) Open ten busy sessions to confirm `9⁺` rendering — readable at 100 % DPI.

If anything misrenders, the most likely culprit is `BuildOverlay` font sizing on a high-DPI display; flag it in HANDOFF as a follow-up rather than blocking ship.

- [ ] **Step 3: Update HANDOFF**

Append a session entry to `docs/HANDOFF.md` describing the new feature, the touched files, the commit SHAs (from Tasks 1–4), and any rough edges discovered during hand-verification.

- [ ] **Step 4: Commit HANDOFF**

```bash
git add docs/HANDOFF.md
git commit -m "docs(handoff): taskbar overlay badge shipped"
```

---

## Open follow-ups (out of scope)

- Per-agent colour coding on the badge (e.g. claude-blue, codex-green) — workspace-wide aggregate is the established direction.
- Click handler on the overlay — `TaskbarItemInfo.Overlay` does not expose a click event upstream.
- 32×32 super-sampled render path for 200 %+ taskbars — only if hand-verification flags aliasing.
- Plural-grammar polish on `Description` for screen readers (`"1 agent working"` vs `"2 agents working"` is already handled, but tests don't cover it).

