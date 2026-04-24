using System.Collections.ObjectModel;
using CommunityToolkit.Mvvm.ComponentModel;

namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Ctrl+K quick-action palette. Holds a flat list of <see cref="PaletteAction"/> and a filter
/// string. The view binds to <see cref="Filtered"/> and the selected action runs on Enter.
/// Fuzzy matching: case-insensitive subsequence match on <see cref="PaletteAction.Display"/>
/// — good enough for &lt;100 actions, cheap, no ranking library needed.
/// </summary>
public sealed partial class CommandPaletteViewModel : ObservableObject
{
    private readonly IReadOnlyList<PaletteAction> _all;

    public CommandPaletteViewModel(IEnumerable<PaletteAction> actions)
    {
        _all = actions.ToArray();
        Filtered = [.. _all];
    }

    public ObservableCollection<PaletteAction> Filtered { get; }

    [ObservableProperty]
    private string _query = string.Empty;

    [ObservableProperty]
    private PaletteAction? _selected;

    partial void OnQueryChanged(string value)
    {
        Filtered.Clear();
        var scored = _all
            .Select(a => (Action: a, Score: Score(a.Display, value)))
            .Where(x => x.Score >= 0)
            .OrderByDescending(x => x.Score);
        foreach (var pair in scored)
        {
            Filtered.Add(pair.Action);
        }
        Selected = Filtered.FirstOrDefault();
    }

    /// <summary>
    /// Scores <paramref name="haystack"/> against <paramref name="needle"/> (case-insensitive):
    ///   - contiguous substring at index 0 ⇒ ~1500 (prefix match)
    ///   - contiguous substring elsewhere ⇒ 1000 + bonus-for-being-early
    ///   - subsequence match ⇒ 100 + per-char bonuses for contiguous runs and word-boundary hits
    ///   - no match ⇒ -1 (caller filters out)
    /// Empty needle returns 0 (match-all, preserves original order via stable sort).
    /// </summary>
    internal static int Score(string haystack, string needle)
    {
        if (string.IsNullOrEmpty(needle)) { return 0; }
        if (string.IsNullOrEmpty(haystack)) { return -1; }

        var h = haystack.ToLowerInvariant();
        var n = needle.ToLowerInvariant();

        var idx = h.IndexOf(n, StringComparison.Ordinal);
        if (idx >= 0)
        {
            return 1000 + (idx == 0 ? 500 : Math.Max(0, 200 - idx));
        }

        var hi = 0;
        var ni = 0;
        var bonus = 0;
        var prevMatchPos = -2;
        while (hi < h.Length && ni < n.Length)
        {
            if (h[hi] == n[ni])
            {
                bonus += hi - prevMatchPos == 1 ? 5 : 1;
                if (hi == 0 || h[hi - 1] is ' ' or '-' or '_' or '.' or '/') { bonus += 3; }
                prevMatchPos = hi;
                ni++;
            }
            hi++;
        }
        return ni == n.Length ? 100 + bonus : -1;
    }
}
