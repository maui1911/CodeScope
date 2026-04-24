using CommunityToolkit.Mvvm.ComponentModel;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// A single tab / sidebar session row. Mirrors a persisted <c>Session</c> plus its live descriptor.
/// </summary>
public sealed partial class SessionTabViewModel : ObservableObject
{
    public SessionTabViewModel(SessionDescriptor descriptor, string? projectId, string? agentId, string? displayNameOverride, string? icon = null)
    {
        Descriptor = descriptor;
        ProjectId = projectId;
        AgentId = agentId;
        Icon = string.IsNullOrWhiteSpace(icon) ? "●" : icon;
        _displayName = string.IsNullOrWhiteSpace(displayNameOverride) ? descriptor.Title : displayNameOverride;
    }

    public SessionDescriptor Descriptor { get; private set; }

    /// <summary>
    /// Swaps the backing descriptor (e.g. flipping a fresh-session launch to a
    /// <c>--continue</c> resume when a tab is moved across groups and its terminal
    /// has to respawn). Raises property-change on <see cref="CommandLine"/> so the
    /// XAML <c>StartupCommandLine</c> binding picks up the new args on reload.
    /// </summary>
    public void Rebind(SessionDescriptor descriptor)
    {
        Descriptor = descriptor;
        OnPropertyChanged(nameof(CommandLine));
    }

    /// <summary>Full command line to launch in the ConPTY host, bound by the terminal view.</summary>
    public string CommandLine => Descriptor.ShellArgs.Count == 0
        ? Descriptor.Shell
        : $"{Descriptor.Shell} {string.Join(' ', Descriptor.ShellArgs)}";

    public string? ProjectId { get; }

    public string? AgentId { get; }

    /// <summary>Glyph shown next to the tab name. Defaults to '●' for shell sessions.</summary>
    public string Icon { get; }

    [ObservableProperty]
    private string _displayName;

    [ObservableProperty]
    private bool _isActive;

    /// <summary>
    /// Semantic session state driving the status dot on the tab row.
    /// Active = focused and streaming, Idle = last response delivered, Wait = agent paused on a question.
    /// Wait-state detection still needs pty-output wiring; for now callers only flip Active ↔ Idle.
    /// </summary>
    [ObservableProperty]
    private TabStatus _status = TabStatus.Idle;

    /// <summary>Cumulative billable tokens for this session (input + output + cache-creation).</summary>
    [ObservableProperty]
    private int _tokensUsed;

    /// <summary>Number of assistant turns observed for this session.</summary>
    [ObservableProperty]
    private int _turnCount;

    /// <summary>
    /// Wall-clock duration of the most recent user → assistant turn, in seconds.
    /// 0 when no pair has been observed yet.
    /// </summary>
    [ObservableProperty]
    private double _lastTurnDurationSec;

    /// <summary>
    /// Context-window capacity detected from the transcript's <c>message.model</c> field, or 0
    /// when the id was unrecognised — in that case the status bar falls back to
    /// <c>AgentProfile.ContextWindowTokens</c>.
    /// </summary>
    [ObservableProperty]
    private int _contextWindowTokens;

    /// <summary>Alias kept for XAML bindings that reference Title (e.g. window chrome).</summary>
    public string Title => DisplayName;

    partial void OnDisplayNameChanged(string value) => OnPropertyChanged(nameof(Title));
}

