using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using NoScope.CodeScope.Core;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Ui.Dialogs;

public partial class NewProjectDialog : Window
{
    private readonly Func<string?> _pickFolder;
    private readonly IGitService _git;
    private CancellationTokenSource? _cloneCts;
    private NewProjectResult? _result;
    private bool _isCloneMode;

    private NewProjectDialog(NewProjectRequest req, Func<string?> pickFolder, IGitService git)
    {
        _pickFolder = pickFolder;
        _git = git;

        InitializeComponent();

        ParentBox.Text = req.DefaultCloneParent;
        UrlBox.TextChanged += (_, _) => OnUrlChanged();
        ParentBox.TextChanged += (_, _) => RefreshAddEnabled();
        NameBox.TextChanged += (_, _) => RefreshAddEnabled();
        ExistingPathBox.TextChanged += (_, _) => RefreshAddEnabled();

        ApplyMode();
        RefreshAddEnabled();
    }

    /// <summary>
    /// Shows the dialog modally. When the user picks "Clone from URL" and clicks Add the
    /// dialog awaits <c>git clone</c> with an inline spinner; on success the dialog closes
    /// and the returned result carries the resolved local path.
    /// </summary>
    public static Task<NewProjectResult?> PromptAsync(NewProjectRequest req, Func<string?> pickFolder, IGitService git)
    {
        var dlg = new NewProjectDialog(req, pickFolder, git) { Owner = Application.Current?.MainWindow };
        // ShowDialog blocks until DialogResult is set. The clone path uses an async helper
        // (OnAdd) that closes the window when done — by the time ShowDialog returns, _result is final.
        var ok = dlg.ShowDialog();
        return Task.FromResult(ok == true ? dlg._result : null);
    }

    private bool IsCloneMode => _isCloneMode;

    private void OnModeExistingClick(object sender, RoutedEventArgs e)
    {
        if (_isCloneMode == false) { return; }
        _isCloneMode = false;
        ApplyMode();
        RefreshAddEnabled();
    }

    private void OnModeCloneClick(object sender, RoutedEventArgs e)
    {
        if (_isCloneMode) { return; }
        _isCloneMode = true;
        ApplyMode();
        RefreshAddEnabled();
    }

    private void ApplyMode()
    {
        ExistingPanel.Visibility = IsCloneMode ? Visibility.Collapsed : Visibility.Visible;
        ClonePanel.Visibility = IsCloneMode ? Visibility.Visible : Visibility.Collapsed;
        ErrorText.Visibility = Visibility.Collapsed;

        // Drive the segmented-toggle visual via Tag (consumed by NP.SegBtn style triggers).
        ModeExistingBtn.Tag = IsCloneMode ? null : "active";
        ModeCloneBtn.Tag = IsCloneMode ? "active" : null;

        if (IsCloneMode) { UrlBox.Focus(); } else { /* leave focus alone */ }
    }

    private void OnUrlChanged()
    {
        // Auto-derive folder name from the URL's last segment, stripping ".git".
        // Only overwrite when the user hasn't customised the field (i.e. it's empty
        // or matches the previously-derived value).
        var url = UrlBox.Text?.Trim() ?? string.Empty;
        var derived = DeriveRepoName(url);
        if (string.IsNullOrEmpty(NameBox.Text) || (NameBox.Tag is string prev && prev == NameBox.Text))
        {
            NameBox.Text = derived;
            NameBox.Tag = derived;
        }
        RefreshAddEnabled();
    }

    internal static string DeriveRepoName(string url)
    {
        if (string.IsNullOrWhiteSpace(url)) { return string.Empty; }
        var u = url.Trim().TrimEnd('/');
        if (u.EndsWith(".git", StringComparison.OrdinalIgnoreCase))
        {
            u = u.Substring(0, u.Length - 4);
        }
        // Take the last segment after '/' or ':' (covers SCP-style 'git@host:owner/repo').
        var slash = u.LastIndexOfAny(['/', ':']);
        var leaf = slash >= 0 ? u.Substring(slash + 1) : u;
        // Strip path-invalid chars defensively.
        foreach (var bad in Path.GetInvalidFileNameChars()) { leaf = leaf.Replace(bad, '-'); }
        return leaf;
    }

    internal static bool IsValidGitUrl(string url)
    {
        if (string.IsNullOrWhiteSpace(url)) { return false; }
        var u = url.Trim();
        if (u.StartsWith("http://", StringComparison.OrdinalIgnoreCase)
            || u.StartsWith("https://", StringComparison.OrdinalIgnoreCase)
            || u.StartsWith("ssh://", StringComparison.OrdinalIgnoreCase))
        {
            return u.Length > 8;
        }
        if (u.StartsWith("git@", StringComparison.OrdinalIgnoreCase))
        {
            // Require host + ':' + path.
            var colon = u.IndexOf(':');
            return colon > "git@".Length && colon < u.Length - 1;
        }
        return false;
    }

    private void RefreshAddEnabled()
    {
        if (IsCloneMode)
        {
            var urlOk = IsValidGitUrl(UrlBox.Text);
            var parentOk = !string.IsNullOrWhiteSpace(ParentBox.Text) && Directory.Exists(ParentBox.Text);
            var nameOk = !string.IsNullOrWhiteSpace(NameBox.Text)
                && NameBox.Text.IndexOfAny(Path.GetInvalidFileNameChars()) < 0;
            AddBtn.IsEnabled = urlOk && parentOk && nameOk;

            // Footer summary — empty values render as "…" placeholders.
            var url = string.IsNullOrWhiteSpace(UrlBox.Text) ? "…" : UrlBox.Text.Trim();
            var name = string.IsNullOrWhiteSpace(NameBox.Text) ? "…" : NameBox.Text.Trim();
            FootMeta.Text = $"git clone · {url} → {name}";
        }
        else
        {
            AddBtn.IsEnabled = !string.IsNullOrWhiteSpace(ExistingPathBox.Text) && Directory.Exists(ExistingPathBox.Text);

            var path = string.IsNullOrWhiteSpace(ExistingPathBox.Text) ? "…" : ExistingPathBox.Text.Trim();
            FootMeta.Text = $"add project · {path}";
        }
    }

    private void OnPickExisting(object sender, RoutedEventArgs e)
    {
        var picked = _pickFolder();
        if (!string.IsNullOrWhiteSpace(picked))
        {
            ExistingPathBox.Text = picked;
        }
    }

    private void OnPickParent(object sender, RoutedEventArgs e)
    {
        var picked = _pickFolder();
        if (!string.IsNullOrWhiteSpace(picked))
        {
            ParentBox.Text = picked;
        }
    }

    private async void OnAdd(object sender, RoutedEventArgs e)
    {
        if (!AddBtn.IsEnabled) { return; }

        ErrorText.Visibility = Visibility.Collapsed;

        if (!IsCloneMode)
        {
            _result = new NewProjectResult(ExistingFolder: ExistingPathBox.Text.Trim(), ClonedPath: null, WasCloned: false);
            DialogResult = true;
            Close();
            return;
        }

        var url = UrlBox.Text.Trim();
        var parent = ParentBox.Text.Trim();
        var name = NameBox.Text.Trim();

        var target = Path.Combine(parent, name);
        if (Directory.Exists(target) && Directory.EnumerateFileSystemEntries(target).Any())
        {
            ShowError($"Destination already exists: {target}");
            return;
        }

        SetBusy(true, $"Cloning {name}…");
        _cloneCts = new CancellationTokenSource();
        Result<string> result;
        try
        {
            result = await _git.CloneAsync(url, parent, name, _cloneCts.Token).ConfigureAwait(true);
        }
        // Broad catch is intentional: OnAdd is async void (WPF event handler),
        // so an uncaught exception here would crash the AppDomain.
        catch (Exception ex)
        {
            result = Result<string>.Fail(ex.Message);
        }
        finally
        {
            _cloneCts.Dispose();
            _cloneCts = null;
        }

        if (result.IsSuccess)
        {
            _result = new NewProjectResult(ExistingFolder: null, ClonedPath: result.Value, WasCloned: true);
            DialogResult = true;
            Close();
            return;
        }

        // Failed or cancelled — clean up a partial target dir so a retry isn't blocked.
        TryDeleteDir(target);
        SetBusy(false, null);
        ShowError(result.Error);
    }

    private void OnCancel(object sender, RoutedEventArgs e)
    {
        if (_cloneCts is { } cts)
        {
            // Cloning is in flight: cancel it, but do NOT close the window — the OnAdd
            // continuation will return us to the editable state.
            try { cts.Cancel(); } catch { /* already disposed */ }
            return;
        }
        DialogResult = false;
        Close();
    }

    private void OnChromeDrag(object sender, MouseButtonEventArgs e)
    {
        if (e.ChangedButton == MouseButton.Left) { DragMove(); }
    }

    private void SetBusy(bool busy, string? text)
    {
        BusyPanel.Visibility = busy ? Visibility.Visible : Visibility.Collapsed;
        FootMeta.Visibility = busy ? Visibility.Collapsed : Visibility.Visible;
        BusyText.Text = text ?? string.Empty;
        AddBtn.Visibility = busy ? Visibility.Collapsed : Visibility.Visible;
        ModeExistingBtn.IsEnabled = !busy;
        ModeCloneBtn.IsEnabled = !busy;
        UrlBox.IsEnabled = !busy;
        ParentBox.IsEnabled = !busy;
        NameBox.IsEnabled = !busy;
        ExistingPathBox.IsEnabled = !busy;
        if (busy) { ErrorText.Visibility = Visibility.Collapsed; }
        else { RefreshAddEnabled(); }
    }

    private void ShowError(string text)
    {
        ErrorText.Text = text;
        ErrorText.Visibility = Visibility.Visible;
    }

    private static void TryDeleteDir(string path)
    {
        try
        {
            if (Directory.Exists(path)) { Directory.Delete(path, recursive: true); }
        }
        catch { /* best effort — partially-cloned trees can hold .pack files in use */ }
    }
}
