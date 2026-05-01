using System.IO;
using System.Runtime.InteropServices;
using System.Threading;
using System.Windows;
using NoScope.CodeScope.App.Polling;
using NoScope.CodeScope.App.Updates;
using NoScope.CodeScope.Core.Interop;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.ViewModels;
using Microsoft.Extensions.DependencyInjection;
using Microsoft.Extensions.Hosting;
using Microsoft.Extensions.Logging;
using Velopack;

namespace NoScope.CodeScope.App;

/// <summary>
/// WPF entry point. Hosts a generic host for DI/logging and enforces single-instance via a named mutex.
/// </summary>
public partial class App : Application
{
    private static readonly string SingleInstanceMutexName = NoScope.CodeScope.Core.AppPaths.SingleInstanceMutexName;

    private IHost? _host;
    private Mutex? _singleInstanceMutex;
    private ProcessTreeKiller? _appKiller;
    private CancellationTokenSource? _updateCts;

    public App()
    {
        // Velopack hooks must run before any WPF state is constructed. The bootstrap intercepts
        // installer/updater handoff arguments (--veloapp-install/-uninstall/-obsolete/-firstrun)
        // and exits the process for those, so the main UI never spins up during install/update.
        VelopackApp.Build().Run();
    }

    protected override void OnStartup(StartupEventArgs e)
    {
        base.OnStartup(e);

        // Allocate a hidden console before anything else. WPF WinExe apps launch without one,
        // and without a console the parent ConPTY session that EasyWindowsTerminalControl spins
        // up for each tab dies milliseconds after the child shell starts — the shell emits its
        // title sequence, the pty shuts down, and the user sees "Session Terminated" on a black
        // pane. Allocating a console here gives CreateProcess's PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE
        // a real console to re-parent from. The console window itself is hidden immediately.
        EnsureHiddenConsole();

        AppDomain.CurrentDomain.UnhandledException += (_, ev) => LogFatal("AppDomain", ev.ExceptionObject as Exception);
        DispatcherUnhandledException += (_, ev) => { LogFatal("Dispatcher", ev.Exception); ev.Handled = true; };
        System.Threading.Tasks.TaskScheduler.UnobservedTaskException += (_, ev) => { LogFatal("Task", ev.Exception); ev.SetObserved(); };

        _singleInstanceMutex = new Mutex(initiallyOwned: true, SingleInstanceMutexName, out var createdNew);
        if (!createdNew)
        {
            NoScope.CodeScope.Ui.Dialogs.ConfirmDialog.Inform(
                title: "CodeScope is already running",
                body: "Another instance of CodeScope owns the single-instance mutex. Activate the running window instead.");
            Shutdown();
            return;
        }

        _appKiller = new ProcessTreeKiller();
        _appKiller.Adopt(System.Diagnostics.Process.GetCurrentProcess().Handle);

        // One-time async bootstrap before HostBuilder: load persisted ProjectsConfig and
        // construct the AgentRegistry from it. Doing this here (vs. inside an
        // AddSingleton factory) keeps GetAwaiter().GetResult() off the DI critical path
        // and prevents any future blocking-during-resolution surprises. The temporary
        // LoggerFactory is disposed immediately — the host's logger pipeline replaces it.
        AgentRegistry agentRegistry;
        using (var bootstrapLoggerFactory = LoggerFactory.Create(b => b.AddDebug()))
        {
            var bootstrapStore = new ProjectStore(bootstrapLoggerFactory.CreateLogger<ProjectStore>());
            var bootstrapCfg = bootstrapStore.LoadAsync().GetAwaiter().GetResult();
            agentRegistry = AgentRegistry.FromConfig(
                bootstrapCfg.IsSuccess ? bootstrapCfg.Value : new ProjectsConfig());
        }

        _host = Host.CreateDefaultBuilder()
            .ConfigureLogging(log =>
            {
                log.ClearProviders();
                log.AddDebug();
                log.AddSimpleConsole(o =>
                {
                    o.SingleLine = true;
                    o.TimestampFormat = "HH:mm:ss ";
                });
            })
            .ConfigureServices(services =>
            {
                services.AddSingleton<IProjectStore, ProjectStore>();
                // Pre-built from the bootstrap above; keeps the DI graph free of sync-over-async.
                services.AddSingleton<IAgentRegistry>(agentRegistry);
                services.AddSingleton<IGitService, GitService>();
                // Custom toast service — hosted in a top-level Popup HWND inside MainWindow
                // so toasts render above the Microsoft.Terminal.Wpf HwndHost children that
                // fill the workspace. See CodeScope.App/Toasts/ToastHostView.xaml.cs.
                services.AddSingleton<NoScope.CodeScope.App.Toasts.ToastService>();
                services.AddSingleton<NoScope.CodeScope.Ui.Services.IToastService>(
                    sp => sp.GetRequiredService<NoScope.CodeScope.App.Toasts.ToastService>());
                services.AddSingleton<IGitHubPullRequestService, GitHubPullRequestService>();
                services.AddSingleton<IGiteaPullRequestService, GiteaPullRequestService>();
                services.AddSingleton<IPullRequestService, PullRequestService>();
                services.AddSingleton<ISessionManager, SessionManager>();
                services.AddSingleton<ISessionStore, SessionStore>();
                services.AddSingleton<IClaudeTelemetryService, ClaudeTelemetryService>();
                services.AddSingleton<IClaudeSessionDiscovery, ClaudeSessionDiscovery>();
                services.AddSingleton<IPiTelemetryService, PiTelemetryService>();
                services.AddSingleton<IPiSessionDiscovery, PiSessionDiscovery>();
                services.AddSingleton<IOpenCodeTelemetryService, OpenCodeTelemetryService>();
                services.AddSingleton<IOpenCodeSessionDiscovery, OpenCodeSessionDiscovery>();
                services.AddSingleton<ICopilotTelemetryService, CopilotTelemetryService>();
                services.AddSingleton<ICopilotSessionDiscovery, CopilotSessionDiscovery>();
                services.AddSingleton<INotificationService, NotificationService>();
                // Native Windows Action-Center toast on agent turn-complete. Fires only
                // when the main window is minimized (gate lives in the implementation).
                // The compat layer auto-registers an AUMID + COM activator on first use,
                // so unpackaged WPF gets real Win10/11 toasts without a manual shortcut.
                services.AddSingleton<NoScope.CodeScope.Ui.Services.IIdleToastNotifier,
                    NoScope.CodeScope.App.Notifications.WindowsIdleToastNotifier>();
                // Owns SessionTabView lifecycle so the inner HwndHost survives reparent on
                // drag-between-groups. See spec
                // docs/superpowers/specs/2026-04-26-cross-group-terminal-drag-design.md.
                services.AddSingleton<NoScope.CodeScope.Ui.Services.ISessionViewHostPool,
                    NoScope.CodeScope.Ui.Services.SessionViewHostPool>();
                services.AddSingleton<NoScope.CodeScope.Ui.Services.ITaskbarBadgeService,
                    NoScope.CodeScope.Ui.Services.TaskbarBadgeService>();
                // Pollers are registered as singletons so the Refresh command can resolve them
                // from DI; the hosted-service indirection re-uses the same instance.
                services.AddSingleton<WorktreeStatusPoller>();
                services.AddHostedService(sp => sp.GetRequiredService<WorktreeStatusPoller>());
                services.AddSingleton<PullRequestStatusPoller>();
                services.AddHostedService(sp => sp.GetRequiredService<PullRequestStatusPoller>());

                // Dev-only memory watchdog — surfaces working-set creep + live-session count
                // every 5 min so per-session scrollback retention regressions (issue #35)
                // don't go unnoticed during long dev runs. Never registered in production.
                if (NoScope.CodeScope.Core.AppPaths.IsDevMode)
                {
                    services.AddHostedService<NoScope.CodeScope.App.Diagnostics.MemoryWatchdog>();
                }
                services.AddSingleton<SidebarViewModel>(sp =>
                {
                    var git = sp.GetRequiredService<IGitService>();
                    Task<NoScope.CodeScope.Ui.Dialogs.NewProjectResult?> PickNewProject(NoScope.CodeScope.Ui.Dialogs.NewProjectRequest request)
                        => NoScope.CodeScope.Ui.Dialogs.NewProjectDialog.PromptAsync(request, PickFolder, git);

                    return new SidebarViewModel(
                        store: sp.GetRequiredService<ISessionStore>(),
                        logger: sp.GetRequiredService<ILogger<SidebarViewModel>>(),
                        pickNewWorktree: PickNewWorktree,
                        pickNewProject: PickNewProject,
                        pullRequests: sp.GetRequiredService<IPullRequestService>(),
                        toasts: sp.GetRequiredService<NoScope.CodeScope.Ui.Services.IToastService>(),
                        agents: sp.GetRequiredService<IAgentRegistry>(),
                        git: git);
                });
                services.AddSingleton<DiffPanelViewModel>();
                services.AddSingleton<MainViewModel>(sp =>
                {
                    var wtPoller = sp.GetRequiredService<WorktreeStatusPoller>();
                    var prPoller = sp.GetRequiredService<PullRequestStatusPoller>();
                    Task RefreshAll(CancellationToken ct)
                        => Task.WhenAll(wtPoller.RefreshAsync(ct), prPoller.RefreshAsync(ct));

                    var vm = new MainViewModel(
                        sp.GetRequiredService<ISessionManager>(),
                        sp.GetRequiredService<ISessionStore>(),
                        sp.GetRequiredService<IAgentRegistry>(),
                        sp.GetRequiredService<ILogger<MainViewModel>>(),
                        PickFolder,
                        RefreshAll,
                        sp.GetRequiredService<IClaudeTelemetryService>(),
                        sp.GetRequiredService<INotificationService>(),
                        sp.GetRequiredService<IClaudeSessionDiscovery>(),
                        sp.GetRequiredService<NoScope.CodeScope.Ui.Services.IToastService>(),
                        sp.GetRequiredService<NoScope.CodeScope.Ui.Services.ISessionViewHostPool>(),
                        sp.GetRequiredService<IPiTelemetryService>(),
                        sp.GetRequiredService<IPiSessionDiscovery>(),
                        sp.GetRequiredService<IOpenCodeTelemetryService>(),
                        sp.GetRequiredService<IOpenCodeSessionDiscovery>(),
                        sp.GetRequiredService<ICopilotTelemetryService>(),
                        sp.GetRequiredService<ICopilotSessionDiscovery>(),
                        sp.GetRequiredService<NoScope.CodeScope.Ui.Services.IIdleToastNotifier>(),
                        sp.GetRequiredService<NoScope.CodeScope.Ui.Services.ITaskbarBadgeService>());
                    var sidebar = sp.GetRequiredService<SidebarViewModel>();
                    vm.AttachSidebar(sidebar);
                    var diff = sp.GetRequiredService<DiffPanelViewModel>();
                    vm.AttachDiffPanel(diff);
                    // Bridge sidebar selection → diff panel.
                    sidebar.WorktreeSelected += (_, wt) => diff.AttachWorktree(wt);
                    return vm;
                });
                services.AddSingleton<MainWindow>();
                services.AddSingleton<UpdateService>();
            })
            .Build();

        _host.Start();

        var window = _host.Services.GetRequiredService<MainWindow>();
        MainWindow = window;
        window.Show();

        // Kick off the auto-update check 10s after the window is visible, then re-check every
        // 3 hours so long-running sessions still pick up newly published releases without a
        // restart. The 10s initial delay keeps the first check off the startup-critical path
        // (no network blocking first paint) and gives the host loggers a moment to wire up.
        // Fire-and-forget by design: UpdateService swallows its own exceptions. The
        // CancellationToken is cancelled in OnExit so any in-flight check or pending delay
        // is aborted on shutdown — prevents hitting disposed services.
        _updateCts = new CancellationTokenSource();
        var updateToken = _updateCts.Token;
        var updater = _host.Services.GetRequiredService<UpdateService>();
        _ = System.Threading.Tasks.Task.Run(async () =>
        {
            try
            {
                await System.Threading.Tasks.Task.Delay(System.TimeSpan.FromSeconds(10), updateToken).ConfigureAwait(false);
                updateToken.ThrowIfCancellationRequested();
                await updater.CheckAsync().ConfigureAwait(false);
                while (!updateToken.IsCancellationRequested)
                {
                    await System.Threading.Tasks.Task.Delay(System.TimeSpan.FromHours(3), updateToken).ConfigureAwait(false);
                    updateToken.ThrowIfCancellationRequested();
                    await updater.CheckAsync().ConfigureAwait(false);
                }
            }
            catch (System.OperationCanceledException)
            {
                // Expected on app shutdown — _updateCts was cancelled.
                System.Diagnostics.Debug.WriteLine("[App] Update poll loop cancelled.");
            }
        });
    }

    protected override void OnExit(ExitEventArgs e)
    {
        try { _updateCts?.Cancel(); }
        catch (System.ObjectDisposedException)
        {
            // CTS already disposed on a previous shutdown attempt — nothing to cancel.
            System.Diagnostics.Debug.WriteLine("[App] OnExit: update CTS already disposed.");
        }
        _updateCts?.Dispose();
        _updateCts = null;

        _host?.StopAsync().GetAwaiter().GetResult();
        _host?.Dispose();
        _host = null;

        _appKiller?.Dispose();
        _appKiller = null;

        if (_singleInstanceMutex is not null)
        {
            try { _singleInstanceMutex.ReleaseMutex(); }
            catch (ApplicationException ex)
            {
                // Not-owner on a clean shutdown path — traced so it isn't silently dropped.
                System.Diagnostics.Debug.WriteLine($"[App] ReleaseMutex: {ex.Message}");
            }
            _singleInstanceMutex.Dispose();
            _singleInstanceMutex = null;
        }

        base.OnExit(e);
    }

    private static void EnsureHiddenConsole()
    {
        // WinExe apps don't get a console at startup. ConPTY — used by
        // EasyWindowsTerminalControl for each tab — needs the parent process to own a
        // console, otherwise every child pwsh dies milliseconds after start and the tab
        // reads "Session Terminated". Detach any inherited console first (e.g. when the
        // exe was launched from wpf-cli / cmd), then allocate a fresh one and hide it.
        //
        // Critical: `AllocConsole` creates `CONIN$`/`CONOUT$` devices but does NOT rebind
        // the process's STD_INPUT/OUTPUT/ERROR handles when they were already redirected
        // by the parent launcher (bash pipe, wpf-cli, VS run-with-redirect). Child
        // processes inherit those stale pipes and see non-TTY stdin — pwsh exits, claude
        // flips to `--print` and errors out with "Input must be provided…". We re-point
        // the three std handles at the fresh console to force ConPTY children to inherit
        // clean TTY handles. Ref: github.com/microsoft/terminal/issues/11276.
        try
        {
            _ = FreeConsole();
            if (!AllocConsole())
            {
                var err = Marshal.GetLastWin32Error();
                System.Diagnostics.Debug.WriteLine($"[App] AllocConsole failed, last-error={err}");
                LogDiag($"AllocConsole failed, last-error={err}");
                return;
            }
            var hwnd = GetConsoleWindow();
            if (hwnd != IntPtr.Zero) { ShowWindow(hwnd, SW_HIDE); }

            // Rebind STD_INPUT_HANDLE → CONIN$, STD_OUTPUT_HANDLE / STD_ERROR_HANDLE → CONOUT$.
            // SHARE_READ | SHARE_WRITE so children can also open the same console.
            var inHandle = CreateFile("CONIN$",
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
            var outHandle = CreateFile("CONOUT$",
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                IntPtr.Zero, OPEN_EXISTING, 0, IntPtr.Zero);
            var inOk = inHandle != INVALID_HANDLE_VALUE && SetStdHandle(STD_INPUT_HANDLE, inHandle);
            var outOk = outHandle != INVALID_HANDLE_VALUE
                && SetStdHandle(STD_OUTPUT_HANDLE, outHandle)
                && SetStdHandle(STD_ERROR_HANDLE, outHandle);
            LogDiag($"console allocated, hwnd={hwnd}, inOk={inOk}, outOk={outOk}");
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[App] EnsureHiddenConsole: {ex.Message}");
            LogDiag($"EnsureHiddenConsole ex: {ex.Message}");
        }
    }

    private static void LogDiag(string msg)
    {
        try
        {
            var dir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), NoScope.CodeScope.Core.AppPaths.AppFolderName);
            Directory.CreateDirectory(dir);
            File.AppendAllText(Path.Combine(dir, "console.log"),
                $"{DateTime.Now:HH:mm:ss.fff} {msg}{Environment.NewLine}");
        }
        catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"LogDiag failed: {ex}"); }
    }

    private const int SW_HIDE = 0;

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool AllocConsole();

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool FreeConsole();

    [DllImport("kernel32.dll")]
    private static extern IntPtr GetConsoleWindow();

    [DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    private static extern IntPtr CreateFile(
        string lpFileName,
        uint dwDesiredAccess,
        uint dwShareMode,
        IntPtr lpSecurityAttributes,
        uint dwCreationDisposition,
        uint dwFlagsAndAttributes,
        IntPtr hTemplateFile);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool SetStdHandle(int nStdHandle, IntPtr handle);

    private const int STD_INPUT_HANDLE = -10;
    private const int STD_OUTPUT_HANDLE = -11;
    private const int STD_ERROR_HANDLE = -12;
    private const uint GENERIC_READ = 0x80000000;
    private const uint GENERIC_WRITE = 0x40000000;
    private const uint FILE_SHARE_READ = 0x00000001;
    private const uint FILE_SHARE_WRITE = 0x00000002;
    private const uint OPEN_EXISTING = 3;
    private static readonly IntPtr INVALID_HANDLE_VALUE = new(-1);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);

    private static void LogFatal(string source, Exception? ex)
    {
        try
        {
            var dir = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), NoScope.CodeScope.Core.AppPaths.AppFolderName);
            Directory.CreateDirectory(dir);
            var path = Path.Combine(dir, "crash.log");
            var line = $"[{DateTime.Now:O}] {source}: {ex}\n";
            File.AppendAllText(path, line);
        }
        catch (Exception writeEx)
        {
            // Fatal-handler write failed — last-resort debug trace before the process dies.
            System.Diagnostics.Debug.WriteLine($"[App] LogFatal write: {writeEx.Message}");
        }
    }

    private static string? PickFolder()
    {
        var dialog = new Microsoft.Win32.OpenFolderDialog
        {
            Title = "Pick a folder to open in a new tab",
            Multiselect = false,
        };

        return dialog.ShowDialog() == true ? dialog.FolderName : null;
    }

    private static NoScope.CodeScope.Ui.Dialogs.NewWorktreeResult? PickNewWorktree(NoScope.CodeScope.Ui.Dialogs.NewWorktreeRequest request)
        => NoScope.CodeScope.Ui.Dialogs.NewWorktreeDialog.Prompt(request);
}
