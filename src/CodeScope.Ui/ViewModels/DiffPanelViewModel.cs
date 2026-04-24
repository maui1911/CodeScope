using System.Collections.ObjectModel;
using System.Text.RegularExpressions;
using System.Windows;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Bottom-panel diff view. Tracks the sidebar's selected worktree and, on refresh, shells to
/// <c>git diff HEAD</c>. The raw patch is split into <see cref="DiffLine"/> rows so the view
/// can colour +/- lines without pulling in a full diff-parsing library.
/// </summary>
public sealed partial class DiffPanelViewModel : ObservableObject
{
    private readonly IGitService _git;
    private readonly ISessionStore _store;
    private readonly ILogger<DiffPanelViewModel> _logger;

    public DiffPanelViewModel(IGitService git, ISessionStore store, ILogger<DiffPanelViewModel> logger)
    {
        _git = git;
        _store = store;
        _logger = logger;
        Lines = [];

        // Re-render on each status tick: cheap since diff runs only when the panel is expanded.
        _store.Changed += (_, change) =>
        {
            if (change is SessionStoreChange.WorktreeStatusUpdated updated
                && _worktree is { } wvm
                && updated.WorktreeId == wvm.Id)
            {
                _ = RefreshAsync();
            }
        };
    }

    public ObservableCollection<DiffLine> Lines { get; }

    [ObservableProperty]
    private bool _isVisible;

    [ObservableProperty]
    private bool _isLoading;

    [ObservableProperty]
    private string _emptyMessage = "Select a worktree to view its diff.";

    /// <summary>Summary for the panel header — "N files · +A -B", empty when no diff loaded.</summary>
    [ObservableProperty]
    private string _summary = string.Empty;

    /// <summary>True when <see cref="Summary"/> has content — drives header visibility.</summary>
    [ObservableProperty]
    private bool _hasSummary;

    private WorktreeViewModel? _worktree;

    public void AttachWorktree(WorktreeViewModel? worktree)
    {
        _worktree = worktree;
        if (IsVisible) { _ = RefreshAsync(); }
    }

    partial void OnIsVisibleChanged(bool value)
    {
        if (value) { _ = RefreshAsync(); }
    }

    [RelayCommand]
    private async Task RefreshAsync()
    {
        if (_worktree is null)
        {
            Lines.Clear();
            Summary = string.Empty;
            HasSummary = false;
            EmptyMessage = "Select a worktree to view its diff.";
            return;
        }

        IsLoading = true;
        try
        {
            var result = await _git.GetDiffAsync(_worktree.Path).ConfigureAwait(true);
            Lines.Clear();
            Summary = string.Empty;
            HasSummary = false;
            if (result.IsFailure)
            {
                _logger.LogDebug("Diff failed for {Path}: {Error}", _worktree.Path, result.Error);
                EmptyMessage = "Failed to read diff.";
                return;
            }

            if (string.IsNullOrEmpty(result.Value))
            {
                EmptyMessage = "No changes against HEAD.";
                return;
            }

            var files = 0;
            var added = 0;
            var removed = 0;
            // Hunk header `@@ -oldStart,oldLen +newStart,newLen @@` seeds per-row line
            // numbers. Counters increment over context/added/removed (added doesn't move
            // old, removed doesn't move new) so each row lands with the correct gutter.
            var oldNo = 0;
            var newNo = 0;
            foreach (var raw in result.Value.Split('\n'))
            {
                var line = raw.TrimEnd('\r');
                var kind = KindOf(line);

                if (kind == DiffLineKind.FileHeader
                    && line.StartsWith("diff ", StringComparison.Ordinal))
                {
                    files += 1;
                }
                else if (kind == DiffLineKind.Hunk)
                {
                    var m = HunkHeader.Match(line);
                    if (m.Success)
                    {
                        oldNo = int.Parse(m.Groups[1].Value) - 1;
                        newNo = int.Parse(m.Groups[2].Value) - 1;
                    }
                }

                int? oldLine = null, newLine = null;
                switch (kind)
                {
                    case DiffLineKind.Added:
                        added += 1;
                        newNo += 1;
                        newLine = newNo;
                        break;
                    case DiffLineKind.Removed:
                        removed += 1;
                        oldNo += 1;
                        oldLine = oldNo;
                        break;
                    case DiffLineKind.Context:
                        oldNo += 1;
                        newNo += 1;
                        oldLine = oldNo;
                        newLine = newNo;
                        break;
                }

                Lines.Add(new DiffLine(line, kind, oldLine, newLine));
            }
            Summary = files == 0 ? string.Empty : $"{files} file{(files == 1 ? "" : "s")} · +{added} −{removed}";
            HasSummary = files > 0;
            EmptyMessage = string.Empty;
        }
        finally
        {
            IsLoading = false;
        }
    }

    private static readonly Regex HunkHeader =
        new(@"^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@", RegexOptions.Compiled);

    /// <summary>Classifies a unified-diff line by its leading sigil. Used by the view for colour.</summary>
    internal static DiffLineKind KindOf(string line)
    {
        if (line.Length == 0) { return DiffLineKind.Context; }
        if (line.StartsWith("+++", StringComparison.Ordinal) || line.StartsWith("---", StringComparison.Ordinal)) { return DiffLineKind.FileHeader; }
        if (line.StartsWith("@@", StringComparison.Ordinal)) { return DiffLineKind.Hunk; }
        if (line.StartsWith("diff ", StringComparison.Ordinal)
            || line.StartsWith("index ", StringComparison.Ordinal)
            || line.StartsWith("new file", StringComparison.Ordinal)
            || line.StartsWith("deleted file", StringComparison.Ordinal)
            || line.StartsWith("similarity ", StringComparison.Ordinal)
            || line.StartsWith("rename ", StringComparison.Ordinal))
        {
            return DiffLineKind.FileHeader;
        }
        return line[0] switch
        {
            '+' => DiffLineKind.Added,
            '-' => DiffLineKind.Removed,
            _ => DiffLineKind.Context,
        };
    }
}

public enum DiffLineKind { Context, Added, Removed, Hunk, FileHeader }

/// <summary>
/// One parsed row of the unified diff. <paramref name="OldLine"/> / <paramref name="NewLine"/>
/// are null for rows that don't belong to the respective gutter (added → no old, removed →
/// no new, hunk/file-header → neither).
/// </summary>
public sealed record DiffLine(string Text, DiffLineKind Kind, int? OldLine = null, int? NewLine = null)
{
    public string OldLineText => OldLine?.ToString() ?? string.Empty;
    public string NewLineText => NewLine?.ToString() ?? string.Empty;
}
