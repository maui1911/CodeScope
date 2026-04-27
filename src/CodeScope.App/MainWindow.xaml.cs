using System.Collections.Specialized;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using NoScope.CodeScope.App.Persistence;
using NoScope.CodeScope.App.Toasts;
using NoScope.CodeScope.Ui.Services;
using NoScope.CodeScope.Ui.ViewModels;
using NoScope.CodeScope.Ui.Views;
using Wpf.Ui.Controls;

namespace NoScope.CodeScope.App;

/// <summary>Root window. Resolves its VM from DI and hydrates persisted sessions on load.</summary>
public partial class MainWindow : FluentWindow
{
    private readonly MainViewModel _viewModel;
    private readonly ToastService _toasts;
    private readonly IIdleToastNotifier _idleNotifier;

    // Cached per-VM view instances. Re-created EditorGroupViews would force the hosted
    // EasyTerminalControl HWND to tear down and respawn pwsh, so we hold on to them for
    // the lifetime of the VM and only mutate Grid.Column on rearrange.
    private readonly Dictionary<EditorGroupViewModel, GroupStripView> _strips = [];
    private readonly Dictionary<EditorGroupViewModel, EditorGroupView> _workspaces = [];

    // Minimum pixel width per group so a drag can't squash a workspace below the
    // terminal's usable column count.
    private const double GroupMinWidth = 200.0;

    // Reserved right-padding on the rightmost GroupStripView so its `+` / tabs never
    // end up under the caption cluster overlay (SplitRight + Min + Max + Close, four
    // 46px buttons stacked on the top-right of the title row). Keep in sync with the
    // CaptionButton style in MainWindow.xaml.
    private const double CaptionClusterWidth = 4 * 46.0;

    public MainWindow(MainViewModel viewModel, ToastService toasts, IIdleToastNotifier idleNotifier)
    {
        _viewModel = viewModel;
        _toasts = toasts;
        _idleNotifier = idleNotifier;
        DataContext = viewModel;

        InitializeComponent();

        // Click on a "turn complete" Action-Center toast → restore the window and select
        // the session it pointed at. Activated is already dispatcher-marshalled by the
        // notifier, so we can touch UI directly.
        _idleNotifier.Activated += OnIdleToastActivated;
        Closed += (_, _) => _idleNotifier.Activated -= OnIdleToastActivated;

        // The toast host binds Items off the service; the service is the single
        // dispatcher-marshalled owner of the visible-toast collection.
        ToastHost.DataContext = _toasts;

        // Apply persisted geometry (size, position, maximised state) before the window renders.
        WindowGeometryStore.Apply(this, WindowGeometryStore.Load());

        _viewModel.Groups.CollectionChanged += OnGroupsChanged;
        _viewModel.GroupWidthsReset += (_, _) => RebuildGroupLayout();

        Loaded += OnLoaded;
        Closing += (_, _) =>
        {
            // Capture the group layout before the VM is torn down — saved mapping is
            // reapplied on next start so tabs hydrate directly into their owning group
            // (no cross-group re-parent → no terminal respawn).
            var (count, focused, map, widths) = _viewModel.CaptureLayout();
            LayoutStore.Save(new LayoutStore.Layout(count, focused, map, widths));
            WindowGeometryStore.Save(this);
        };
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        try
        {
            // Restore the group layout before hydration so each tab mounts in the
            // correct group from the first frame.
            if (LayoutStore.Load() is { } layout)
            {
                _viewModel.PrepareLayoutFromPersistence(layout.GroupCount, layout.FocusedGroupIndex, layout.SessionToGroup, layout.GroupWidths);
            }
            // Initial population — PrepareLayoutFromPersistence already raised
            // GroupWidthsReset, but that happened before this handler was wired on the
            // CollectionChanged path (the widths-reset event is, hooked in the ctor).
            // Regardless, a rebuild now gives a single authoritative layout.
            RebuildGroupLayout();
            await _viewModel.InitializeAsync();

            // Opt-in toast sampler — surfaces one of each severity ~600ms after the
            // first render so the design QA workflow (wpf-cli screenshot vs. Playwright
            // screenshot of the spec HTML) doesn't need to drive a git operation to see
            // a toast. Set CODESCOPE_TOAST_SAMPLE=1 alongside CODESCOPE_DEV=1 to fire it.
            if (NoScope.CodeScope.Core.AppPaths.IsDevMode
                && Environment.GetEnvironmentVariable("CODESCOPE_TOAST_SAMPLE") == "1")
            {
                _ = SeedDevToastsAsync();
            }
        }
        catch (Exception ex)
        {
            NoScope.CodeScope.Ui.Dialogs.ConfirmDialog.Inform(
                title: "Failed to initialize",
                body: ex.Message);
        }
    }

    private void OnGroupsChanged(object? sender, NotifyCollectionChangedEventArgs e)
    {
        // Structural change; MainViewModel also re-syncs GroupWidths in the same pass
        // and raises GroupWidthsReset, but we rebuild here too so view updates don't
        // depend on event ordering.
        RebuildGroupLayout();
    }

    /// <summary>
    /// Rebuilds <see cref="StripsHost"/> and <see cref="WorkspacesHost"/> to match
    /// <c>MainViewModel.Groups</c> and <c>MainViewModel.GroupWidths</c>. Cached views
    /// are retained across rebuilds so the terminal HWNDs never leave their host.
    /// </summary>
    private void RebuildGroupLayout()
    {
        var groups = _viewModel.Groups;
        var widths = _viewModel.GroupWidths;
        var n = groups.Count;
        if (n == 0) { return; }

        // Drop cached views whose VM is no longer in Groups — typical after CloseGroup.
        PruneCache(_strips, groups, StripsHost);
        PruneCache(_workspaces, groups, WorkspacesHost);

        // 2N-1 columns: groups at even indices, 1px splitter gutters at odd indices.
        BuildColumnDefinitions(StripsHost, n, widths);
        BuildColumnDefinitions(WorkspacesHost, n, widths);

        // Strip cells. No splitter/drag-grip on the strip row — only the workspace
        // owns the drag handle; the strip passively mirrors column widths.
        PlaceGroupViews(
            host: StripsHost,
            cache: _strips,
            groups: groups,
            factory: vm => new GroupStripView { DataContext = vm },
            addSplitters: false);

        // Reserve space for the caption cluster on the rightmost strip only — its `+`
        // button used to end up under the caption overlay on the last group. Earlier
        // strips keep a zero margin so tabs align flush with their workspace columns.
        for (var i = 0; i < groups.Count; i++)
        {
            if (_strips.TryGetValue(groups[i], out var strip))
            {
                strip.Margin = i == groups.Count - 1
                    ? new Thickness(0, 0, CaptionClusterWidth, 0)
                    : default;
            }
        }

        // Workspace cells + splitters.
        PlaceGroupViews(
            host: WorkspacesHost,
            cache: _workspaces,
            groups: groups,
            factory: vm => new EditorGroupView { DataContext = vm },
            addSplitters: true);
    }

    private static void PruneCache<TView>(
        Dictionary<EditorGroupViewModel, TView> cache,
        System.Collections.Generic.IList<EditorGroupViewModel> live,
        Panel host)
        where TView : FrameworkElement
    {
        var gone = cache.Keys.Where(k => !live.Contains(k)).ToList();
        foreach (var k in gone)
        {
            if (cache.TryGetValue(k, out var view))
            {
                host.Children.Remove(view);
            }
            cache.Remove(k);
        }
    }

    private static void BuildColumnDefinitions(Grid host, int groupCount, System.Collections.Generic.IList<double> widths)
    {
        host.ColumnDefinitions.Clear();
        for (var i = 0; i < groupCount; i++)
        {
            var w = i < widths.Count && widths[i] > 0 ? widths[i] : 1.0;
            host.ColumnDefinitions.Add(new ColumnDefinition
            {
                Width = new GridLength(w, GridUnitType.Star),
                MinWidth = GroupMinWidth,
            });
            if (i < groupCount - 1)
            {
                // 1px splitter gutter; filled in WorkspacesHost with a GridSplitter,
                // left empty in StripsHost so the column tracks width sympathetically.
                host.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1) });
            }
        }
    }

    private void PlaceGroupViews<TView>(
        Grid host,
        Dictionary<EditorGroupViewModel, TView> cache,
        System.Collections.Generic.IList<EditorGroupViewModel> groups,
        Func<EditorGroupViewModel, TView> factory,
        bool addSplitters)
        where TView : FrameworkElement
    {
        // Drop only splitters from children; group views stay mounted so no HWND shuffle.
        for (var i = host.Children.Count - 1; i >= 0; i--)
        {
            if (host.Children[i] is GridSplitter) { host.Children.RemoveAt(i); }
        }

        for (var i = 0; i < groups.Count; i++)
        {
            var vm = groups[i];
            if (!cache.TryGetValue(vm, out var view))
            {
                view = factory(vm);
                cache[vm] = view;
                host.Children.Add(view);
            }
            Grid.SetColumn(view, i * 2);

            if (addSplitters && i < groups.Count - 1)
            {
                var splitter = new GridSplitter
                {
                    Width = 1,
                    HorizontalAlignment = HorizontalAlignment.Stretch,
                    VerticalAlignment = VerticalAlignment.Stretch,
                    ResizeBehavior = GridResizeBehavior.PreviousAndNext,
                    ResizeDirection = GridResizeDirection.Columns,
                    Background = (System.Windows.Media.Brush)FindResource("Surface.Border"),
                    ShowsPreview = false,
                };
                Grid.SetColumn(splitter, i * 2 + 1);
                splitter.DragCompleted += OnWorkspaceSplitterDragCompleted;
                host.Children.Add(splitter);
            }
        }
    }

    /// <summary>
    /// Captures the star weights WPF resolved after a GridSplitter drag, writes them
    /// back into <see cref="MainViewModel.GroupWidths"/>, and mirrors them onto
    /// <see cref="StripsHost"/> so the strips keep tracking the workspaces.
    /// Persisted on window close via <see cref="LayoutStore"/>.
    /// </summary>
    private void OnWorkspaceSplitterDragCompleted(object sender, DragCompletedEventArgs e)
    {
        var ws = WorkspacesHost;
        var strips = StripsHost;
        var groups = _viewModel.Groups;
        var widths = _viewModel.GroupWidths;

        // Even-indexed columns are group columns. Pull the post-drag star values and
        // normalise so the list sums to roughly Groups.Count (preserves a "1.0 per group"
        // baseline when users reset by splitting a new group).
        var stars = new List<double>(groups.Count);
        for (var i = 0; i < groups.Count; i++)
        {
            var col = i * 2;
            if (col >= ws.ColumnDefinitions.Count) { stars.Add(1.0); continue; }
            var w = ws.ColumnDefinitions[col].Width;
            stars.Add(w.IsStar && w.Value > 0 ? w.Value : Math.Max(1.0, ws.ColumnDefinitions[col].ActualWidth));
        }

        // Sync the MainViewModel list in-place. Do NOT raise GroupWidthsReset — the
        // workspace columns already carry the authoritative widths, and a full rebuild
        // would churn the cache for no visual change.
        for (var i = 0; i < stars.Count && i < widths.Count; i++) { widths[i] = stars[i]; }

        // Push the same star values to strip columns so the tab strip tops stay aligned.
        for (var i = 0; i < groups.Count; i++)
        {
            var col = i * 2;
            if (col < strips.ColumnDefinitions.Count)
            {
                strips.ColumnDefinitions[col].Width = new GridLength(stars[i], GridUnitType.Star);
            }
        }
    }

    /// <summary>
    /// Classic DragMove + double-click-to-maximise on the top-row caption borders
    /// (brand cell + strip host). Interactive children (tab strips, group buttons,
    /// TitleBar caption glyphs) handle their own clicks, so this bubble only fires
    /// when the user clicks empty caption space.
    /// </summary>
    private void OnCaptionDrag(object sender, System.Windows.Input.MouseButtonEventArgs e)
    {
        if (e.ChangedButton != System.Windows.Input.MouseButton.Left) { return; }
        if (e.ClickCount == 2)
        {
            WindowState = WindowState == WindowState.Maximized ? WindowState.Normal : WindowState.Maximized;
            e.Handled = true;
            return;
        }
        try { DragMove(); }
        catch (InvalidOperationException ex)
        {
            // Mouse released before DragMove latched — expected race, trace for diagnostics.
            System.Diagnostics.Debug.WriteLine($"[MainWindow] DragMove: {ex.Message}");
        }
    }

    private void OnIdleToastActivated(object? sender, string agentSessionId)
    {
        // Toast click grants the app foreground rights, so a plain Activate() reliably
        // promotes us above other top-levels — no SetForegroundWindow / Topmost dance.
        if (WindowState == WindowState.Minimized) { WindowState = WindowState.Normal; }
        Activate();
        _viewModel.ActivateSessionByAgentSessionId(agentSessionId);
    }

    private void OnCaptionMinimize(object sender, System.Windows.RoutedEventArgs e)
        => WindowState = WindowState.Minimized;

    private void OnCaptionMaxRestore(object sender, System.Windows.RoutedEventArgs e)
        => WindowState = WindowState == WindowState.Maximized ? WindowState.Normal : WindowState.Maximized;

    private void OnCaptionClose(object sender, System.Windows.RoutedEventArgs e) => Close();

    private async Task SeedDevToastsAsync()
    {
        await Task.Delay(600).ConfigureAwait(true);
        _toasts.Show(new NoScope.CodeScope.Ui.Services.ToastRequest(
            NoScope.CodeScope.Ui.Services.ToastSeverity.Info,
            "Switching to claude-opus-4",
            "Current turn finishes on sonnet-4-5, then swaps."));
        await Task.Delay(150).ConfigureAwait(true);
        _toasts.Show(new NoScope.CodeScope.Ui.Services.ToastRequest(
            NoScope.CodeScope.Ui.Services.ToastSeverity.Ok,
            "Branch pushed",
            "feat/reporting → origin/feat/reporting · 3 commits.",
            Actions: [new NoScope.CodeScope.Ui.Services.ToastAction("Open PR", () => { }, IsPrimary: true)]));
        await Task.Delay(150).ConfigureAwait(true);
        _toasts.Show(new NoScope.CodeScope.Ui.Services.ToastRequest(
            NoScope.CodeScope.Ui.Services.ToastSeverity.Warn,
            "Context near cap",
            "186k / 200k on feat/reporting. Compact or branch off the next message.",
            Actions: [new NoScope.CodeScope.Ui.Services.ToastAction("Compact now", () => { }, IsPrimary: true)]));
        await Task.Delay(150).ConfigureAwait(true);
        _toasts.Show(new NoScope.CodeScope.Ui.Services.ToastRequest(
            NoScope.CodeScope.Ui.Services.ToastSeverity.Err,
            "Push failed",
            "origin rejected: non-fast-forward. Pull main and rebase before pushing again.",
            Actions:
            [
                new NoScope.CodeScope.Ui.Services.ToastAction("View output", () => { }, IsPrimary: true),
                new NoScope.CodeScope.Ui.Services.ToastAction("Retry", () => { }),
            ]));
    }
}
