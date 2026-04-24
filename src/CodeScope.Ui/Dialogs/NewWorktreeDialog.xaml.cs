using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Input;
using System.Windows.Media;
using NoScope.CodeScope.Core.Models;

namespace NoScope.CodeScope.Ui.Dialogs;

public partial class NewWorktreeDialog : Window
{
    private void OnChromeDrag(object sender, MouseButtonEventArgs e)
    {
        // Drag only when the user grabs dialog chrome — not when clicking a field.
        if (e.ChangedButton != MouseButton.Left) { return; }
        if (e.OriginalSource is DependencyObject d && IsInInteractive(d)) { return; }
        DragMove();
    }

    private static bool IsInInteractive(DependencyObject node)
    {
        for (var cur = node; cur is not null; cur = VisualTreeHelper.GetParent(cur))
        {
            if (cur is TextBox or Button or Popup or ListBox or ListBoxItem) { return true; }
        }
        return false;
    }

    public string Branch { get; private set; } = string.Empty;
    public string FolderPath { get; private set; } = string.Empty;
    public string? BaseBranch { get; private set; }
    public bool SpawnSession { get; private set; } = true;

    private readonly string _worktreeRoot;
    private readonly string _projectPath;
    private readonly string _projectName;
    private readonly IReadOnlyList<BranchInfo> _branches;
    private static readonly SolidColorBrush s_focusBrush =
        new(Color.FromArgb(0x8C, 0x00, 0x99, 0xFF)); // rgba(0,153,255,.55)
    private static readonly SolidColorBrush s_toggleOnTrack =
        new(Color.FromArgb(0x59, 0x00, 0x99, 0xFF)); // rgba(0,153,255,.35)
    private static readonly SolidColorBrush s_toggleOffTrack =
        new(Color.FromRgb(0x1F, 0x1F, 0x1F));
    private static readonly SolidColorBrush s_toggleOnThumb =
        new(Color.FromRgb(0x00, 0x99, 0xFF));
    private static readonly SolidColorBrush s_toggleOffThumb =
        new(Color.FromRgb(0x60, 0x60, 0x60));
    private static readonly BranchInfo s_headRow = new("(HEAD)", false, string.Empty, "current");

    private NewWorktreeDialog(NewWorktreeRequest req)
    {
        _worktreeRoot = req.WorktreeRoot ?? req.ProjectPath + ".worktrees";
        _projectPath = req.ProjectPath;
        _projectName = req.ProjectName;
        _branches = req.Branches;

        InitializeComponent();

        EyebrowText.Text = string.IsNullOrWhiteSpace(req.ProjectName)
            ? "PROJECT"
            : req.ProjectName.ToUpperInvariant();

        // Rebuild the filtered list up-front so the dropdown is populated on first open.
        ApplyBranchFilter(string.Empty);

        // Pick the default base: requested → first local match → (HEAD).
        var initial = _branches.FirstOrDefault(b => b.Name == req.DefaultBase)
            ?? _branches.FirstOrDefault(b => !b.IsRemote)
            ?? s_headRow;
        SetSelectedBase(initial);

        ApplySpawnVisual();

        BranchBox.Focus();

        BranchBox.TextChanged += (_, _) =>
        {
            if (string.IsNullOrWhiteSpace(PathBox.Text)
                || PathBox.Text.StartsWith(_worktreeRoot, StringComparison.OrdinalIgnoreCase))
            {
                var safe = Sanitize(BranchBox.Text);
                PathBox.Text = string.IsNullOrEmpty(safe)
                    ? string.Empty
                    : Path.Combine(_worktreeRoot, safe);
            }

            RefreshValidity();
        };

        PathBox.TextChanged += (_, _) => RefreshValidity();

        RefreshValidity();
    }

    public static NewWorktreeResult? Prompt(NewWorktreeRequest request)
    {
        var dlg = new NewWorktreeDialog(request) { Owner = Application.Current?.MainWindow };
        if (dlg.ShowDialog() != true) { return null; }
        return new NewWorktreeResult(dlg.Branch, dlg.FolderPath, dlg.BaseBranch, dlg.SpawnSession);
    }

    private void OnOk(object sender, RoutedEventArgs e)
    {
        Branch = BranchBox.Text?.Trim() ?? string.Empty;
        FolderPath = PathBox.Text?.Trim() ?? string.Empty;
        if (string.IsNullOrEmpty(Branch) || string.IsNullOrEmpty(FolderPath)) { return; }
        DialogResult = true;
        Close();
    }

    private void OnCancel(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }

    private void OnFieldFocused(object sender, KeyboardFocusChangedEventArgs e)
    {
        var chrome = ChromeFor(sender);
        if (chrome is null) { return; }
        chrome.BorderBrush = s_focusBrush;
    }

    private void OnFieldBlurred(object sender, KeyboardFocusChangedEventArgs e)
    {
        var chrome = ChromeFor(sender);
        if (chrome is null) { return; }
        chrome.BorderBrush = Brushes.Transparent;
    }

    private Border? ChromeFor(object sender) => sender switch
    {
        var x when ReferenceEquals(x, BranchBox) => BranchChrome,
        var x when ReferenceEquals(x, PathBox) => PathChrome,
        _ => null,
    };

    // ----- Base-branch dropdown -----

    private void OnBaseTriggerClick(object sender, RoutedEventArgs e)
    {
        BasePopup.IsOpen = !BasePopup.IsOpen;
        if (BasePopup.IsOpen)
        {
            BaseSearch.Focus();
            BaseSearch.SelectAll();
        }
    }

    private void OnBaseSearchChanged(object sender, TextChangedEventArgs e)
    {
        ApplyBranchFilter(BaseSearch.Text);
    }

    private void OnBaseSearchKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Escape) { BasePopup.IsOpen = false; e.Handled = true; return; }
        if (e.Key == Key.Enter)
        {
            if (BaseList.Items.Count > 0)
            {
                var item = (BranchListItem)(BaseList.SelectedItem ?? BaseList.Items[0]!);
                if (item.Info is not null) { SetSelectedBase(item.Info); }
                BasePopup.IsOpen = false;
            }
            e.Handled = true;
        }
        else if (e.Key is Key.Down or Key.Up)
        {
            if (BaseList.Items.Count == 0) { return; }
            var idx = BaseList.SelectedIndex;
            idx = e.Key == Key.Down ? Math.Min(idx + 1, BaseList.Items.Count - 1) : Math.Max(idx - 1, 0);
            if (idx < 0) { idx = 0; }
            BaseList.SelectedIndex = idx;
            BaseList.ScrollIntoView(BaseList.SelectedItem);
            e.Handled = true;
        }
    }

    private void OnBaseItemClick(object sender, MouseButtonEventArgs e)
    {
        if (sender is FrameworkElement fe && fe.DataContext is BranchListItem item && item.Info is not null)
        {
            SetSelectedBase(item.Info);
            BasePopup.IsOpen = false;
        }
    }

    private void ApplyBranchFilter(string? query)
    {
        var q = (query ?? string.Empty).Trim();
        var filtered = new List<BranchListItem>(_branches.Count + 1)
        {
            new(s_headRow, "·") // always keep (HEAD) pinned
        };
        BranchInfo? lastGroupHeaderPushed = null;

        // Local first, then remote — emit group labels as sentinel rows.
        var groups = new[] { (Label: "LOCAL", IsRemote: false), (Label: "REMOTE", IsRemote: true) };
        foreach (var (label, remote) in groups)
        {
            var rows = _branches
                .Where(b => b.IsRemote == remote)
                .Where(b => q.Length == 0 || b.Name.Contains(q, StringComparison.OrdinalIgnoreCase))
                .ToList();
            if (rows.Count == 0) { continue; }
            filtered.Add(new BranchListItem(null, label));
            foreach (var r in rows) { filtered.Add(new BranchListItem(r, null)); lastGroupHeaderPushed = r; }
        }

        BaseList.ItemsSource = filtered;
    }

    private void SetSelectedBase(BranchInfo info)
    {
        BaseBranch = info.Name == s_headRow.Name ? null : info.Name;
        BaseValueText.Text = info.Name;
        BaseMetaText.Text = string.IsNullOrEmpty(info.ShortSha)
            ? info.RelativeDate
            : $"{info.ShortSha} · {info.RelativeDate}";
        RefreshValidity();
    }

    private void RefreshValidity()
    {
        var branch = BranchBox.Text?.Trim() ?? string.Empty;
        var path = PathBox.Text?.Trim() ?? string.Empty;
        var valid = branch.Length >= 2 && path.Length > 0;
        CreateBtn.IsEnabled = valid;

        var name = string.IsNullOrEmpty(branch) ? "…" : branch;
        var folder = string.IsNullOrEmpty(path) ? "…" : Path.GetFileName(path.TrimEnd('\\', '/'));
        var baseLabel = BaseBranch ?? "HEAD";
        FootMeta.Text = $"git worktree add  ·  {baseLabel} → {name}  @  {folder}";
        PathHint.Text = string.IsNullOrEmpty(path) ? string.Empty : path;

        UpdatePathRelation(path);
    }

    /// <summary>
    /// Populates the caption below the WORKTREE FOLDER field: where the folder lives
    /// (parent directory) and how that parent relates to the project's own folder.
    /// The three common shapes are surfaced explicitly so the user knows whether the
    /// new worktree will land next to, inside, or far away from the project.
    /// </summary>
    private void UpdatePathRelation(string path)
    {
        if (string.IsNullOrEmpty(path))
        {
            PathParentText.Text = string.Empty;
            PathRelationText.Text = string.Empty;
            return;
        }

        var parent = Path.GetDirectoryName(path) ?? string.Empty;
        PathParentText.Text = string.IsNullOrEmpty(parent) ? string.Empty : $"in  {parent}";
        PathRelationText.Text = DescribeRelation(parent);
    }

    private string DescribeRelation(string parent)
    {
        if (string.IsNullOrEmpty(parent) || string.IsNullOrEmpty(_projectPath)) { return string.Empty; }

        var project = _projectPath.TrimEnd('\\', '/');
        var projectParent = Path.GetDirectoryName(project) ?? string.Empty;
        var projectLeaf = Path.GetFileName(project);
        var displayName = string.IsNullOrEmpty(_projectName) ? projectLeaf : _projectName;
        parent = parent.TrimEnd('\\', '/');

        if (parent.Equals(project, StringComparison.OrdinalIgnoreCase))
        {
            return $"inside  {displayName}";
        }
        if (parent.Equals(projectParent, StringComparison.OrdinalIgnoreCase))
        {
            return $"sibling of  {displayName}";
        }
        // Default ".worktrees" sibling root (e.g. C:\dev\codescope.worktrees beside C:\dev\codescope).
        if (!string.IsNullOrEmpty(projectParent)
            && parent.StartsWith(projectParent + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
        {
            return $"sibling of  {displayName}";
        }
        if (parent.StartsWith(project + Path.DirectorySeparatorChar, StringComparison.OrdinalIgnoreCase))
        {
            return $"inside  {displayName}";
        }
        return $"detached from  {displayName}";
    }

    private void OnSpawnToggleClick(object sender, RoutedEventArgs e)
    {
        SpawnSession = !SpawnSession;
        ApplySpawnVisual();
    }

    private void ApplySpawnVisual()
    {
        SpawnTrack.Background = SpawnSession ? s_toggleOnTrack : s_toggleOffTrack;
        SpawnThumb.Background = SpawnSession ? s_toggleOnThumb : s_toggleOffThumb;
        SpawnThumb.HorizontalAlignment = SpawnSession
            ? System.Windows.HorizontalAlignment.Right
            : System.Windows.HorizontalAlignment.Left;
        SpawnThumb.Margin = SpawnSession
            ? new Thickness(0, 0, 2, 0)
            : new Thickness(2, 0, 0, 0);
    }

    private static string Sanitize(string branch)
    {
        var safe = branch.Replace('/', '-').Replace('\\', '-');
        foreach (var bad in Path.GetInvalidFileNameChars()) { safe = safe.Replace(bad, '-'); }
        return safe.Trim('-');
    }

    /// <summary>
    /// DataTemplate row for the base-branch popup. Either <see cref="Info"/> is set
    /// (a real branch) or <see cref="GroupLabel"/> is set (sentinel "LOCAL"/"REMOTE" header).
    /// </summary>
    internal sealed record BranchListItem(BranchInfo? Info, string? GroupLabel)
    {
        public bool IsHeader => Info is null;
        public string Name => Info?.Name ?? GroupLabel ?? string.Empty;
        public string Meta => Info is null ? string.Empty :
            string.IsNullOrEmpty(Info.ShortSha) ? Info.RelativeDate : $"{Info.ShortSha} · {Info.RelativeDate}";
    }
}
