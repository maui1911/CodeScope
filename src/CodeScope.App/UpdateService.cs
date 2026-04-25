using System.Threading.Tasks;
using Microsoft.Extensions.Logging;
using NoScope.CodeScope.Ui.Services;
using Velopack;
using Velopack.Sources;

namespace NoScope.CodeScope.App;

/// <summary>
/// Background updater backed by Velopack + GitHub releases (the same channel the release.yml
/// workflow publishes to). On success it surfaces a non-blocking <see cref="IToastService"/>
/// toast to signal that the update finished downloading, then later offers restart via a
/// confirmation dialog. The check is best-effort — any exception (offline, rate-limited, not
/// an installed build) is swallowed with a warning log.
/// </summary>
public sealed class UpdateService
{
    // Public GitHub releases channel — `vpk upload github --channel win` from release.yml.
    private const string RepoUrl = "https://github.com/maui1911/CodeScope";
    private const string Channel = "win";

    private readonly ILogger<UpdateService> _logger;
    private readonly IToastService _toasts;

    public UpdateService(ILogger<UpdateService> logger, IToastService toasts)
    {
        _logger = logger;
        _toasts = toasts;
    }

    /// <summary>
    /// Fire-and-forget entry point. Checks once, downloads if newer, surfaces the toast.
    /// No-op under <c>CODESCOPE_DEV=1</c> (the dev build runs from <c>dotnet run</c> and has no
    /// Velopack manifest — <see cref="UpdateManager"/> would throw on CheckForUpdatesAsync).
    /// </summary>
    public async Task CheckAsync()
    {
        if (NoScope.CodeScope.Core.AppPaths.IsDevMode)
        {
            _logger.LogDebug("UpdateService: skipped (CODESCOPE_DEV=1)");
            return;
        }

        try
        {
            var source = new GithubSource(RepoUrl, accessToken: null, prerelease: false);
            var mgr = new UpdateManager(source, new UpdateOptions { ExplicitChannel = Channel });

            // IsInstalled flips false when running loose from a publish folder (CI smoke test,
            // local `dotnet run --configuration Release`, Velopack's squirrel bootstrap hasn't
            // staged a Current\ yet). Skip — nothing we can `ApplyUpdatesAndRestart` onto.
            if (!mgr.IsInstalled)
            {
                _logger.LogDebug("UpdateService: not an installed build, skipping update check");
                return;
            }

            var info = await mgr.CheckForUpdatesAsync().ConfigureAwait(false);
            if (info is null)
            {
                _logger.LogDebug("UpdateService: up to date");
                return;
            }

            _logger.LogInformation("UpdateService: downloading update {Version}", info.TargetFullRelease.Version);
            await mgr.DownloadUpdatesAsync(info).ConfigureAwait(false);

            var version = info.TargetFullRelease.Version.ToString();
            ShowUpdateReadyToast(mgr, info, version);
        }
        catch (System.Exception ex)
        {
            _logger.LogWarning(ex, "UpdateService: check failed");
        }
    }

    private void ShowUpdateReadyToast(UpdateManager mgr, UpdateInfo info, string version)
    {
        // Two-step prompt so the user isn't yanked into a modal the moment they launch the
        // app: (1) a Success snackbar as the quiet signal that an update finished downloading,
        // then (2) a non-blocking ConfirmDialog a few seconds later asking whether to restart.
        // The snackbar is fire-and-forget; the dialog is the actual decision point. Users can
        // defer by clicking Later — the update stays staged and Velopack applies it on the
        // next clean exit regardless, so a "skip" never loses the download.
        var app = System.Windows.Application.Current;
        if (app?.Dispatcher is { } d && !d.CheckAccess())
        {
            d.Invoke(() => ShowUpdateReadyToast(mgr, info, version));
            return;
        }

        _toasts.Show(new ToastRequest(
            ToastSeverity.Ok,
            Title: $"CodeScope {version} ready",
            Message: "Update downloaded — restart to install.",
            Duration: System.TimeSpan.FromSeconds(10),
            Id: "update-ready"));

        _ = System.Threading.Tasks.Task.Run(async () =>
        {
            try
            {
                await System.Threading.Tasks.Task.Delay(System.TimeSpan.FromSeconds(3)).ConfigureAwait(false);

                var dispatcher = app?.Dispatcher;
                if (dispatcher is null || dispatcher.HasShutdownStarted || dispatcher.HasShutdownFinished)
                {
                    return;
                }

                await dispatcher.InvokeAsync(() =>
                {
                    var restart = NoScope.CodeScope.Ui.Dialogs.ConfirmDialog.Confirm(
                        title: $"CodeScope {version} is ready",
                        body: "Restart now to finish installing. Open sessions will be closed and their transcripts resumed automatically on the next launch.",
                        confirmLabel: "Restart",
                        cancelLabel: "Later");
                    if (!restart) { return; }
                    try { mgr.ApplyUpdatesAndRestart(info); }
                    catch (System.Exception ex) { _logger.LogWarning(ex, "ApplyUpdatesAndRestart failed"); }
                });
            }
            catch (System.Exception ex)
            {
                _logger.LogWarning(ex, "Unable to show update ready prompt");
            }
        });
    }
}
