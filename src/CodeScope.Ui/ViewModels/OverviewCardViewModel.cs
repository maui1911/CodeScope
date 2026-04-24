using System.Collections.ObjectModel;
using System.IO;
using System.Windows.Media;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// One card in the Overview grid — mirrors a single running session plus a snapshot of its
/// worktree metadata. Preview lines are built from the worktree status we already cache,
/// since we don't mirror live terminal output.
/// </summary>
public sealed partial class OverviewCardViewModel : ObservableObject
{
    public OverviewCardViewModel(
        string projectName,
        WorktreeViewModel worktree,
        SessionTabViewModel session,
        OverviewCardState state,
        int siblingSessions,
        string? agentDisplayName)
    {
        ProjectName = projectName;
        Worktree = worktree;
        Session = session;
        State = state;
        SiblingSessions = siblingSessions;
        AgentDisplayName = string.IsNullOrWhiteSpace(agentDisplayName) ? "shell" : agentDisplayName;
        PreviewLines = new ObservableCollection<OverviewPreviewLine>();
        RebuildPreview();
    }

    public string ProjectName { get; }

    public WorktreeViewModel Worktree { get; }

    public SessionTabViewModel Session { get; }

    public OverviewCardState State { get; }

    public int SiblingSessions { get; }

    public string AgentDisplayName { get; }

    public string DisplayTitle => $"{ProjectName} · {Worktree.DisplayBranch}";

    public string BranchLabel => Worktree.DisplayBranch;

    /// <summary>
    /// Drives the sdot fill: accent for active, Signal.Ok for idle, Signal.Warn for waiting.
    /// </summary>
    public string StateDotResourceKey => State switch
    {
        OverviewCardState.Active => "Accent.Primary",
        OverviewCardState.Waiting => "Signal.Warn",
        _ => "Signal.Ok",
    };

    /// <summary>
    /// Token key for the ring halo behind the sdot. Only the active dot gets the accent ring;
    /// the others render a flat pip.
    /// </summary>
    public string StateDotRingResourceKey => State switch
    {
        OverviewCardState.Active => "Accent.Ring",
        OverviewCardState.Waiting => "Accent.Ring",
        _ => "Surface.Canvas",
    };

    /// <summary>"focus" badge text next to the title — only the first card shows it in the mock. Unused for now.</summary>
    public string TypeBadge => string.Empty;

    /// <summary>
    /// Derived "changes" chip from the cached worktree status. A clean worktree reads "changes: 0"
    /// with a muted border; dirty/ahead/behind promotes to the accent-ringed chip shown in the mock.
    /// </summary>
    public string ChangesLabel
    {
        get
        {
            var score = (Worktree.IsDirty ? 1 : 0) + (Worktree.Ahead > 0 ? 1 : 0) + (Worktree.Behind > 0 ? 1 : 0);
            return $"changes: {score}";
        }
    }

    public bool HasChanges => Worktree.IsDirty || Worktree.Ahead > 0 || Worktree.Behind > 0;

    /// <summary>
    /// Footer right-hand slug — uses the session count on the worktree as a surrogate for the
    /// mock's "elapsed" marker. We don't track agent response time yet; this keeps the slot used
    /// without inventing fake timestamps.
    /// </summary>
    public string ElapsedLabel => SiblingSessions > 1
        ? $"{SiblingSessions} sess"
        : "—";

    public ObservableCollection<OverviewPreviewLine> PreviewLines { get; }

    private void RebuildPreview()
    {
        PreviewLines.Clear();

        var agent = AgentDisplayName;
        PreviewLines.Add(new OverviewPreviewLine(
            $"> {agent} session · {Worktree.DisplayBranch}",
            OverviewPreviewKind.Prompt));

        var basename = string.IsNullOrWhiteSpace(Worktree.Path)
            ? Worktree.DisplayBranch
            : Path.GetFileName(Worktree.Path.TrimEnd('\\', '/'));
        PreviewLines.Add(new OverviewPreviewLine($"path   {basename}", OverviewPreviewKind.Muted));

        var statusBits = new List<string> { Worktree.IsDirty ? "dirty" : "clean" };
        if (Worktree.Ahead > 0 || Worktree.Behind > 0)
        {
            statusBits.Add(Worktree.AheadBehindText);
        }
        PreviewLines.Add(new OverviewPreviewLine(
            $"status {string.Join(" · ", statusBits)}",
            Worktree.IsDirty ? OverviewPreviewKind.Warn : OverviewPreviewKind.Ok));

        var prText = Worktree.HasPullRequest && Worktree.PullRequest is { } pr
            ? $"pr     {Worktree.PrBadgeText} · CI {Worktree.CiGlyph}"
            : "pr     no open PR";
        PreviewLines.Add(new OverviewPreviewLine(prText, OverviewPreviewKind.Muted));

        PreviewLines.Add(new OverviewPreviewLine(
            $"sessions on this worktree: {SiblingSessions}",
            OverviewPreviewKind.Muted));

        var tail = State switch
        {
            OverviewCardState.Active => "active · agent running",
            OverviewCardState.Waiting => "awaiting input · review required",
            _ => "idle · awaiting next prompt",
        };
        PreviewLines.Add(new OverviewPreviewLine(tail, State switch
        {
            OverviewCardState.Active => OverviewPreviewKind.Accent,
            OverviewCardState.Waiting => OverviewPreviewKind.Warn,
            _ => OverviewPreviewKind.Muted,
        }));
    }

    public event EventHandler<SessionTabViewModel>? OpenRequested;

    [RelayCommand]
    private void Open() => OpenRequested?.Invoke(this, Session);
}

/// <summary>Single rendered line inside a card's mini-terminal pane.</summary>
public sealed record OverviewPreviewLine(string Text, OverviewPreviewKind Kind);

/// <summary>Drives the per-line Foreground brush so the mini terminal reads as code, not UI.</summary>
public enum OverviewPreviewKind
{
    Prompt,
    Accent,
    Muted,
    Faint,
    Ok,
    Warn,
}
