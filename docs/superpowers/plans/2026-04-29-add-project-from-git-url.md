# Add project from a git URL — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let users add a CodeScope project either by picking an existing folder *or* by pasting a `.git` URL — with an inline cloning spinner so big repos show progress.

**Architecture:** Add `IGitService.CloneAsync` (shell-out to `git clone`). Replace today's bare folder picker in `SidebarViewModel.AddProjectAsync` with a new `NewProjectDialog` that has a mode toggle and an inline busy state (it owns the `CloneAsync` call + cancellation). The dialog returns either an existing-folder path or the resolved path of a successful clone; the sidebar VM only ever calls `_store.AddProjectAsync` with that path.

**Tech Stack:** .NET 10 / WPF / `CommunityToolkit.Mvvm` / `System.Diagnostics.Process` (via existing `ProcessRunner`). Tests run on xUnit with `Skippable` for git-on-PATH.

**Spec:** `docs/superpowers/specs/2026-04-29-add-project-from-git-url-design.md`.

**File map (created or modified):**

| File | What changes |
|---|---|
| `src/CodeScope.Core/Services/IGitService.cs` | Add `CloneAsync` signature |
| `src/CodeScope.Core/Services/GitService.cs` | Implement `CloneAsync` |
| `tests/CodeScope.Core.Tests/GitServiceCloneTests.cs` | New — 4 tests |
| `src/CodeScope.Ui/Dialogs/NewProjectRequest.cs` | New — request + result records |
| `src/CodeScope.Ui/Dialogs/NewProjectDialog.xaml` | New — XAML UI |
| `src/CodeScope.Ui/Dialogs/NewProjectDialog.xaml.cs` | New — code-behind, owns clone + busy state |
| `src/CodeScope.Ui/ViewModels/SidebarViewModel.cs` | New `_pickNewProject` field + ctor arg |
| `src/CodeScope.Ui/ViewModels/SidebarViewModel.Commands.cs` | Rewire `AddProjectAsync` |
| `src/CodeScope.App/App.xaml.cs` | Register `PickNewProject` delegate; pass to `SidebarViewModel` |

---

### Task 1: Add `CloneAsync` to `IGitService`

**Files:**
- Modify: `src/CodeScope.Core/Services/IGitService.cs`

- [ ] **Step 1: Add the contract**

Add the following method to the `IGitService` interface (place it next to `FetchAllAsync`/`PullAsync` for cohesion — line ~74):

```csharp
/// <summary>
/// Runs <c>git -C &lt;parentDir&gt; clone -- &lt;url&gt; &lt;folderName&gt;</c>. Returns the
/// absolute path of the resulting working tree on success. Fails verbatim with git's stderr
/// on auth errors, network errors, "destination already exists", etc. Cancellation kills
/// the git process; partially-cloned target directories are NOT auto-removed by this method
/// (callers can clean up if they want — see <c>NewProjectDialog</c> for the pattern).
/// </summary>
Task<Result<string>> CloneAsync(string url, string parentDir, string folderName, CancellationToken ct = default);
```

- [ ] **Step 2: Compile to confirm the interface change is well-formed**

Run: `dotnet build src/CodeScope.Core/CodeScope.Core.csproj -c Debug`
Expected: build succeeds (no implementation yet — `GitService` will fail-build in Task 2 if we ran the full solution build now, but `CodeScope.Core.csproj` alone has no implementer in the same project … actually `GitService` lives here too, so this WILL fail). Skip the build until Task 2 lands.

(No commit yet — bundled with Task 2.)

---

### Task 2: Write the failing tests for `GitService.CloneAsync`

**Files:**
- Create: `tests/CodeScope.Core.Tests/GitServiceCloneTests.cs`

- [ ] **Step 1: Write all four tests**

```csharp
using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class GitServiceCloneTests
{
    [SkippableFact]
    public async Task Clone_From_Local_Bare_Repo_Succeeds()
    {
        Skip.If(!IsGitOnPath(), "git is not on PATH");

        using var tmp = new TempDir();
        var src = Path.Combine(tmp.Path, "src.git");
        await RunGit(tmp.Path, $"init --bare \"{src}\"");
        // Seed one commit so HEAD resolves after clone.
        var seed = Path.Combine(tmp.Path, "seed");
        Directory.CreateDirectory(seed);
        await RunGit(seed, "init -b main");
        await RunGit(seed, "config user.email test@test");
        await RunGit(seed, "config user.name test");
        await File.WriteAllTextAsync(Path.Combine(seed, "x.txt"), "hi");
        await RunGit(seed, "add .");
        await RunGit(seed, "commit -m seed");
        await RunGit(seed, $"remote add origin \"{src}\"");
        await RunGit(seed, "push -u origin main");

        var svc = new GitService(NullLogger<GitService>.Instance);
        var dest = Path.Combine(tmp.Path, "dest");
        Directory.CreateDirectory(dest);

        var result = await svc.CloneAsync(src, dest, "repo");

        result.IsSuccess.Should().BeTrue(result.IsFailure ? result.Error : "");
        result.Value.Should().Be(Path.Combine(dest, "repo"));
        File.Exists(Path.Combine(result.Value, ".git", "HEAD")).Should().BeTrue();
        File.Exists(Path.Combine(result.Value, "x.txt")).Should().BeTrue();
    }

    [SkippableFact]
    public async Task Clone_Fails_When_Target_Already_Exists_NonEmpty()
    {
        Skip.If(!IsGitOnPath(), "git is not on PATH");

        using var tmp = new TempDir();
        var dest = Path.Combine(tmp.Path, "parent");
        Directory.CreateDirectory(Path.Combine(dest, "repo"));
        await File.WriteAllTextAsync(Path.Combine(dest, "repo", "block.txt"), "x");

        var svc = new GitService(NullLogger<GitService>.Instance);

        var result = await svc.CloneAsync("https://example.invalid/x.git", dest, "repo");

        result.IsFailure.Should().BeTrue();
        result.Error.Should().NotBeNullOrWhiteSpace();
    }

    [SkippableFact]
    public async Task Clone_Fails_For_Garbage_Url()
    {
        Skip.If(!IsGitOnPath(), "git is not on PATH");

        using var tmp = new TempDir();
        var svc = new GitService(NullLogger<GitService>.Instance);

        var result = await svc.CloneAsync("not a url at all", tmp.Path, "repo");

        result.IsFailure.Should().BeTrue();
        result.Error.Should().NotBeNullOrWhiteSpace();
    }

    [SkippableFact]
    public async Task Clone_With_Cancelled_Token_Fails_Without_Leaving_Target()
    {
        Skip.If(!IsGitOnPath(), "git is not on PATH");

        using var tmp = new TempDir();
        var svc = new GitService(NullLogger<GitService>.Instance);
        using var cts = new CancellationTokenSource();
        cts.Cancel();

        var result = await svc.CloneAsync("https://example.invalid/x.git", tmp.Path, "repo", cts.Token);

        result.IsFailure.Should().BeTrue();
    }

    private static bool IsGitOnPath()
    {
        var paths = (Environment.GetEnvironmentVariable("PATH") ?? string.Empty)
            .Split(Path.PathSeparator, StringSplitOptions.RemoveEmptyEntries);
        var exeNames = OperatingSystem.IsWindows() ? new[] { "git.exe", "git.cmd" } : new[] { "git" };
        return paths.Any(p => exeNames.Any(n => File.Exists(Path.Combine(p, n))));
    }

    private static async Task RunGit(string cwd, string args)
    {
        var psi = new System.Diagnostics.ProcessStartInfo("git", args)
        {
            WorkingDirectory = cwd,
            RedirectStandardError = true,
            RedirectStandardOutput = true,
            UseShellExecute = false,
            CreateNoWindow = true,
        };
        using var p = System.Diagnostics.Process.Start(psi)!;
        await p.WaitForExitAsync();
        if (p.ExitCode != 0)
        {
            throw new InvalidOperationException($"git {args}: {await p.StandardError.ReadToEndAsync()}");
        }
    }

    private sealed class TempDir : IDisposable
    {
        public string Path { get; } = System.IO.Path.Combine(System.IO.Path.GetTempPath(), $"cs-clone-{Guid.NewGuid():N}");
        public TempDir() => Directory.CreateDirectory(Path);
        public void Dispose()
        {
            try { Directory.Delete(Path, recursive: true); } catch { /* best effort */ }
        }
    }
}
```

- [ ] **Step 2: Run the new tests — they must fail because `CloneAsync` does not yet exist**

Run: `dotnet test tests/CodeScope.Core.Tests/CodeScope.Core.Tests.csproj --filter "FullyQualifiedName~GitServiceCloneTests" -c Debug`
Expected: build error — `IGitService` lacks `CloneAsync` (since Task 1's interface addition has nothing implementing it yet). That's the failing-test signal for this slice.

(No commit yet — bundled with Task 3.)

---

### Task 3: Implement `GitService.CloneAsync`

**Files:**
- Modify: `src/CodeScope.Core/Services/GitService.cs`

- [ ] **Step 1: Implement next to `FetchAllAsync` (after `PullAsync`, around line 206)**

```csharp
public async Task<Result<string>> CloneAsync(string url, string parentDir, string folderName, CancellationToken ct = default)
{
    if (string.IsNullOrWhiteSpace(url))
    {
        return Result<string>.Fail("URL is empty");
    }
    if (string.IsNullOrWhiteSpace(parentDir))
    {
        return Result<string>.Fail("Parent directory is empty");
    }
    if (string.IsNullOrWhiteSpace(folderName))
    {
        return Result<string>.Fail("Folder name is empty");
    }

    if (!Directory.Exists(parentDir))
    {
        return Result<string>.Fail($"Parent directory does not exist: {parentDir}");
    }

    var target = Path.Combine(parentDir, folderName);
    if (Directory.Exists(target) && Directory.EnumerateFileSystemEntries(target).Any())
    {
        return Result<string>.Fail($"Destination already exists and is not empty: {target}");
    }

    // Quote both arguments and use `--` so URLs/folder names that start with a dash
    // can't be parsed as flags.
    var args = $"-C \"{parentDir}\" clone -- \"{url}\" \"{folderName}\"";
    var result = await RunAsync(cwd: null, args, ct).ConfigureAwait(false);
    return result.IsSuccess
        ? Result<string>.Ok(target)
        : Result<string>.Fail(result.Error);
}
```

- [ ] **Step 2: Run the four tests — all four must now pass (or skip when git missing)**

Run: `dotnet test tests/CodeScope.Core.Tests/CodeScope.Core.Tests.csproj --filter "FullyQualifiedName~GitServiceCloneTests" -c Debug`
Expected: 4 passed (or skipped on a machine without git).

- [ ] **Step 3: Run the full Core test suite to confirm nothing regressed**

Run: `dotnet test tests/CodeScope.Core.Tests/CodeScope.Core.Tests.csproj -c Debug`
Expected: all green (≥4 new passing on top of the existing baseline).

- [ ] **Step 4: Commit**

```bash
git add src/CodeScope.Core/Services/IGitService.cs \
        src/CodeScope.Core/Services/GitService.cs \
        tests/CodeScope.Core.Tests/GitServiceCloneTests.cs
git commit -m "feat(git): add CloneAsync (#20)"
```

---

### Task 4: Add `NewProjectRequest` / `NewProjectResult` records

**Files:**
- Create: `src/CodeScope.Ui/Dialogs/NewProjectRequest.cs`

- [ ] **Step 1: Create the file**

```csharp
namespace NoScope.CodeScope.Ui.Dialogs;

/// <summary>
/// Input envelope for <see cref="NewProjectDialog.PromptAsync(NewProjectRequest)"/>.
/// </summary>
/// <param name="DefaultCloneParent">Folder pre-filled in the "Parent folder" field of the
/// Clone-from-URL mode. Caller picks: typically the parent of the most-recently-added
/// project, falling back to <c>%USERPROFILE%\source\repos</c>.</param>
public sealed record NewProjectRequest(string DefaultCloneParent);

/// <summary>
/// Result of <see cref="NewProjectDialog"/>. Exactly one of <see cref="ExistingFolder"/>
/// or <see cref="ClonedPath"/> is non-null. <see cref="WasCloned"/> mirrors that for
/// callers that want to vary the success-toast wording.
/// </summary>
/// <param name="ExistingFolder">Set when the user picked "Existing folder".</param>
/// <param name="ClonedPath">Set when the dialog successfully cloned the URL.</param>
/// <param name="WasCloned">True iff <see cref="ClonedPath"/> is set.</param>
public sealed record NewProjectResult(string? ExistingFolder, string? ClonedPath, bool WasCloned);
```

- [ ] **Step 2: Build to confirm it compiles**

Run: `dotnet build src/CodeScope.Ui/CodeScope.Ui.csproj -c Debug`
Expected: build succeeds.

(No commit — bundled with Tasks 5–6.)

---

### Task 5: Build the `NewProjectDialog` XAML

**Files:**
- Create: `src/CodeScope.Ui/Dialogs/NewProjectDialog.xaml`

- [ ] **Step 1: Write the XAML**

The visual style mirrors `NewWorktreeDialog.xaml` (same `Window.Resources` block of token-based styles). Keep it simple — no popup, no toggle widget.

```xml
<Window
    x:Class="NoScope.CodeScope.Ui.Dialogs.NewProjectDialog"
    xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
    Title="Add project"
    Width="540"
    SizeToContent="Height"
    ResizeMode="NoResize"
    WindowStartupLocation="CenterOwner"
    ShowInTaskbar="False"
    WindowStyle="None"
    AllowsTransparency="True"
    Background="Transparent"
    TextElement.Foreground="{DynamicResource Fig.Brush.Ink}"
    TextElement.FontFamily="{DynamicResource Fig.Font.Sans}"
    TextElement.FontSize="{DynamicResource Fig.Size.Body}">
    <Window.Resources>
        <Style x:Key="NP.FieldChrome" TargetType="Border">
            <Setter Property="Height" Value="36" />
            <Setter Property="Background" Value="{DynamicResource Fig.Brush.GlassDark}" />
            <Setter Property="BorderBrush" Value="Transparent" />
            <Setter Property="BorderThickness" Value="1" />
            <Setter Property="CornerRadius" Value="6" />
        </Style>
        <Style x:Key="NP.InnerMonoBox" TargetType="TextBox">
            <Setter Property="FontFamily" Value="{DynamicResource Fig.Font.Mono}" />
            <Setter Property="FontSize" Value="13" />
            <Setter Property="Foreground" Value="{DynamicResource Fig.Brush.Ink}" />
            <Setter Property="Background" Value="Transparent" />
            <Setter Property="BorderThickness" Value="0" />
            <Setter Property="VerticalContentAlignment" Value="Center" />
            <Setter Property="Padding" Value="12,0" />
        </Style>
        <Style x:Key="NP.FieldLabel" TargetType="TextBlock">
            <Setter Property="FontSize" Value="11" />
            <Setter Property="FontWeight" Value="Medium" />
            <Setter Property="Foreground" Value="{DynamicResource Fig.Brush.InkMuted}" />
            <Setter Property="Margin" Value="0,0,0,6" />
        </Style>
        <Style x:Key="NP.PrimaryBtn" TargetType="Button">
            <Setter Property="Height" Value="32" />
            <Setter Property="Padding" Value="14,0" />
            <Setter Property="Background" Value="{DynamicResource Accent.Primary}" />
            <Setter Property="Foreground" Value="White" />
            <Setter Property="FontWeight" Value="Medium" />
            <Setter Property="Cursor" Value="Hand" />
        </Style>
        <Style x:Key="NP.GhostBtn" TargetType="Button" BasedOn="{StaticResource NP.PrimaryBtn}">
            <Setter Property="Background" Value="Transparent" />
            <Setter Property="Foreground" Value="{DynamicResource Fig.Brush.InkMuted}" />
        </Style>
    </Window.Resources>

    <Border Background="{DynamicResource Fig.Brush.Panel}"
            CornerRadius="10"
            BorderBrush="{DynamicResource Fig.Brush.PanelBorder}"
            BorderThickness="1"
            Padding="0">
        <Grid Margin="20,16,20,16">
            <Grid.RowDefinitions>
                <RowDefinition Height="Auto" />  <!-- chrome bar (drag + close)   -->
                <RowDefinition Height="Auto" />  <!-- mode toggle                 -->
                <RowDefinition Height="Auto" />  <!-- existing-folder panel       -->
                <RowDefinition Height="Auto" />  <!-- clone panel                 -->
                <RowDefinition Height="Auto" />  <!-- error                       -->
                <RowDefinition Height="Auto" />  <!-- buttons                     -->
            </Grid.RowDefinitions>

            <!-- Chrome bar: title left + close right; whole row is drag-handle. -->
            <Grid Grid.Row="0" Margin="0,0,0,16" MouseLeftButtonDown="OnChromeDrag">
                <TextBlock Text="ADD PROJECT"
                           FontFamily="{DynamicResource Fig.Font.Mono}"
                           FontSize="10"
                           Foreground="{DynamicResource Accent.Primary}" />
                <Button x:Name="CloseBtn"
                        Content="×"
                        Width="24" Height="24"
                        HorizontalAlignment="Right"
                        Background="Transparent"
                        BorderThickness="0"
                        Foreground="{DynamicResource Fig.Brush.InkMuted}"
                        Cursor="Hand"
                        Click="OnCancel" />
            </Grid>

            <!-- Mode toggle: two radio buttons. -->
            <StackPanel Grid.Row="1" Orientation="Horizontal" Margin="0,0,0,16">
                <RadioButton x:Name="ModeExisting"
                             Content="Existing folder"
                             GroupName="NPMode"
                             IsChecked="True"
                             Margin="0,0,16,0"
                             Checked="OnModeChanged" />
                <RadioButton x:Name="ModeClone"
                             Content="Clone from URL"
                             GroupName="NPMode"
                             Checked="OnModeChanged" />
            </StackPanel>

            <!-- Existing-folder panel. -->
            <StackPanel x:Name="ExistingPanel" Grid.Row="2">
                <TextBlock Text="FOLDER" Style="{StaticResource NP.FieldLabel}" />
                <Grid>
                    <Grid.ColumnDefinitions>
                        <ColumnDefinition Width="*" />
                        <ColumnDefinition Width="Auto" />
                    </Grid.ColumnDefinitions>
                    <Border Grid.Column="0" Style="{StaticResource NP.FieldChrome}">
                        <TextBox x:Name="ExistingPathBox"
                                 Style="{StaticResource NP.InnerMonoBox}"
                                 IsReadOnly="True" />
                    </Border>
                    <Button Grid.Column="1"
                            Content="Browse…"
                            Style="{StaticResource NP.GhostBtn}"
                            Margin="8,0,0,0"
                            Click="OnPickExisting" />
                </Grid>
            </StackPanel>

            <!-- Clone panel. -->
            <StackPanel x:Name="ClonePanel" Grid.Row="3" Visibility="Collapsed">
                <TextBlock Text="GIT URL" Style="{StaticResource NP.FieldLabel}" />
                <Border Style="{StaticResource NP.FieldChrome}" Margin="0,0,0,12">
                    <TextBox x:Name="UrlBox" Style="{StaticResource NP.InnerMonoBox}" />
                </Border>

                <TextBlock Text="PARENT FOLDER" Style="{StaticResource NP.FieldLabel}" />
                <Grid Margin="0,0,0,12">
                    <Grid.ColumnDefinitions>
                        <ColumnDefinition Width="*" />
                        <ColumnDefinition Width="Auto" />
                    </Grid.ColumnDefinitions>
                    <Border Grid.Column="0" Style="{StaticResource NP.FieldChrome}">
                        <TextBox x:Name="ParentBox" Style="{StaticResource NP.InnerMonoBox}" />
                    </Border>
                    <Button Grid.Column="1"
                            Content="Browse…"
                            Style="{StaticResource NP.GhostBtn}"
                            Margin="8,0,0,0"
                            Click="OnPickParent" />
                </Grid>

                <TextBlock Text="FOLDER NAME" Style="{StaticResource NP.FieldLabel}" />
                <Border Style="{StaticResource NP.FieldChrome}">
                    <TextBox x:Name="NameBox" Style="{StaticResource NP.InnerMonoBox}" />
                </Border>
            </StackPanel>

            <!-- Inline error (used by clone failures). -->
            <TextBlock x:Name="ErrorText"
                       Grid.Row="4"
                       Margin="0,12,0,0"
                       TextWrapping="Wrap"
                       FontFamily="{DynamicResource Fig.Font.Mono}"
                       FontSize="11"
                       Foreground="{DynamicResource Status.Brush.Err}"
                       Visibility="Collapsed" />

            <!-- Buttons row. -->
            <Grid Grid.Row="5" Margin="0,20,0,0">
                <Grid.ColumnDefinitions>
                    <ColumnDefinition Width="*" />
                    <ColumnDefinition Width="Auto" />
                </Grid.ColumnDefinitions>

                <!-- Busy indicator (visible only while cloning). -->
                <StackPanel x:Name="BusyPanel"
                            Grid.Column="0"
                            Orientation="Horizontal"
                            Visibility="Collapsed">
                    <ProgressBar Width="120" Height="4" IsIndeterminate="True" Margin="0,0,10,0" />
                    <TextBlock x:Name="BusyText"
                               VerticalAlignment="Center"
                               FontFamily="{DynamicResource Fig.Font.Mono}"
                               FontSize="11"
                               Foreground="{DynamicResource Fig.Brush.InkMuted}" />
                </StackPanel>

                <StackPanel Grid.Column="1" Orientation="Horizontal">
                    <Button x:Name="CancelBtn"
                            Content="Cancel"
                            Style="{StaticResource NP.GhostBtn}"
                            Click="OnCancel" />
                    <Button x:Name="AddBtn"
                            Content="Add"
                            Style="{StaticResource NP.PrimaryBtn}"
                            Margin="8,0,0,0"
                            IsDefault="True"
                            Click="OnAdd" />
                </StackPanel>
            </Grid>
        </Grid>
    </Border>
</Window>
```

(No commit yet — bundled with Task 6.)

---

### Task 6: Implement `NewProjectDialog` code-behind

**Files:**
- Create: `src/CodeScope.Ui/Dialogs/NewProjectDialog.xaml.cs`

- [ ] **Step 1: Write the code-behind**

```csharp
using System.IO;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using NoScope.CodeScope.Core.Services;

namespace NoScope.CodeScope.Ui.Dialogs;

public partial class NewProjectDialog : Window
{
    private readonly Func<string?> _pickFolder;
    private readonly IGitService _git;
    private CancellationTokenSource? _cloneCts;
    private NewProjectResult? _result;

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
    public static async Task<NewProjectResult?> PromptAsync(NewProjectRequest req, Func<string?> pickFolder, IGitService git)
    {
        var dlg = new NewProjectDialog(req, pickFolder, git) { Owner = Application.Current?.MainWindow };
        var ok = dlg.ShowDialog();
        // ShowDialog blocks until DialogResult is set. The clone path uses an async helper
        // that closes the window when done — so by the time we get here, _result is final.
        return ok == true ? dlg._result : null;
    }

    private bool IsCloneMode => ModeClone.IsChecked == true;

    private void OnModeChanged(object sender, RoutedEventArgs e)
    {
        if (!IsLoaded) { return; }
        ApplyMode();
        RefreshAddEnabled();
    }

    private void ApplyMode()
    {
        ExistingPanel.Visibility = IsCloneMode ? Visibility.Collapsed : Visibility.Visible;
        ClonePanel.Visibility = IsCloneMode ? Visibility.Visible : Visibility.Collapsed;
        ErrorText.Visibility = Visibility.Collapsed;
        if (IsCloneMode) { UrlBox.Focus(); } else { /* leave focus alone */ }
    }

    private void OnUrlChanged()
    {
        // Auto-derive folder name from the URL's last segment, stripping ".git".
        // Only overwrite when the user hasn't customised the field (i.e. it's empty
        // or matches the previously-derived value).
        var url = UrlBox.Text?.Trim() ?? string.Empty;
        var derived = DeriveRepoName(url);
        if (string.IsNullOrEmpty(NameBox.Text) || NameBox.Tag is string prev && prev == NameBox.Text)
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
        var slash = u.LastIndexOfAny(new[] { '/', ':' });
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
        }
        else
        {
            AddBtn.IsEnabled = !string.IsNullOrWhiteSpace(ExistingPathBox.Text) && Directory.Exists(ExistingPathBox.Text);
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
        BusyText.Text = text ?? string.Empty;
        AddBtn.IsEnabled = !busy && AddBtn.IsEnabled;
        AddBtn.Visibility = busy ? Visibility.Collapsed : Visibility.Visible;
        ModeExisting.IsEnabled = !busy;
        ModeClone.IsEnabled = !busy;
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
```

- [ ] **Step 2: Build the Ui project to confirm**

Run: `dotnet build src/CodeScope.Ui/CodeScope.Ui.csproj -c Debug`
Expected: build succeeds.

(No commit yet — bundled with Tasks 7–8.)

---

### Task 7: Wire the dialog into `SidebarViewModel`

**Files:**
- Modify: `src/CodeScope.Ui/ViewModels/SidebarViewModel.cs`
- Modify: `src/CodeScope.Ui/ViewModels/SidebarViewModel.Commands.cs`

- [ ] **Step 1: Add the `_pickNewProject` field + ctor argument**

In `SidebarViewModel.cs`, alongside `_pickNewWorktree` (line 28):

```csharp
private readonly Func<NewProjectRequest, Task<NewProjectResult?>> _pickNewProject;
```

Add the ctor parameter (after `pickNewWorktree`):

```csharp
Func<NewProjectRequest, Task<NewProjectResult?>>? pickNewProject = null,
```

Initialize it (next to `_pickNewWorktree = ...`):

```csharp
_pickNewProject = pickNewProject ?? (_ => Task.FromResult<NewProjectResult?>(null));
```

- [ ] **Step 2: Replace `AddProjectAsync` body in `SidebarViewModel.Commands.cs`**

Replace the existing `AddProjectAsync` (lines 14–21) with:

```csharp
[RelayCommand]
private async Task AddProjectAsync()
{
    var request = new NewProjectRequest(DefaultCloneParent());
    var picked = await _pickNewProject(request).ConfigureAwait(true);
    if (picked is null) { return; }

    var path = picked.ExistingFolder ?? picked.ClonedPath;
    if (string.IsNullOrWhiteSpace(path)) { return; }

    var r = await _store.AddProjectAsync(path, displayName: null).ConfigureAwait(true);
    if (r.IsFailure)
    {
        _logger.LogWarning("AddProject failed: {Error}", r.Error);
        ErrToast(picked.WasCloned ? "Cloned, but project add failed" : "Add project failed", r.Error);
        return;
    }

    if (picked.WasCloned)
    {
        Toast("Project cloned", r.Value.Name, ToastSeverity.Ok);
    }
}

private string DefaultCloneParent()
{
    // Most-recently-added project's parent → user's source-repos folder → home.
    var recent = _store.Projects.LastOrDefault(p => !string.IsNullOrWhiteSpace(p.Path));
    if (recent is not null)
    {
        var parent = System.IO.Path.GetDirectoryName(recent.Path.TrimEnd('\\', '/'));
        if (!string.IsNullOrWhiteSpace(parent) && System.IO.Directory.Exists(parent))
        {
            return parent;
        }
    }
    var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
    var fallback = System.IO.Path.Combine(home, "source", "repos");
    return System.IO.Directory.Exists(fallback) ? fallback : home;
}
```

`AddProjectByPathAsync` (drag-drop) is left unchanged on purpose.

- [ ] **Step 3: Build to confirm**

Run: `dotnet build src/CodeScope.Ui/CodeScope.Ui.csproj -c Debug`
Expected: build succeeds.

(No commit yet — bundled with Task 8.)

---

### Task 8: Register `PickNewProject` in `App.xaml.cs`

**Files:**
- Modify: `src/CodeScope.App/App.xaml.cs`

- [ ] **Step 1: Add the helper alongside `PickNewWorktree` (line 387)**

```csharp
private static Task<NoScope.CodeScope.Ui.Dialogs.NewProjectResult?> PickNewProject(NoScope.CodeScope.Ui.Dialogs.NewProjectRequest request)
{
    var git = _host?.Services.GetRequiredService<NoScope.CodeScope.Core.Services.IGitService>()
        ?? throw new InvalidOperationException("Host not started");
    return NoScope.CodeScope.Ui.Dialogs.NewProjectDialog.PromptAsync(request, PickFolder, git);
}
```

(`_host` is already a private static field on `App` — check the top of the file; if it's not, add `private static IHost? _host;` at the field declarations.)

- [ ] **Step 2: Wire it into the `SidebarViewModel` registration (line 135–143)**

Update the registration to pass `PickNewProject` as the new optional ctor arg. Insert it in the position matching the ctor's parameter list:

```csharp
services.AddSingleton<SidebarViewModel>(sp => new SidebarViewModel(
    sp.GetRequiredService<ISessionStore>(),
    sp.GetRequiredService<ILogger<SidebarViewModel>>(),
    PickFolder,
    PickNewWorktree,
    PickNewProject,
    sp.GetRequiredService<IPullRequestService>(),
    sp.GetRequiredService<NoScope.CodeScope.Ui.Services.IToastService>(),
    sp.GetRequiredService<IAgentRegistry>(),
    sp.GetRequiredService<IGitService>()));
```

(The `SidebarViewModel` ctor signature change in Task 7 placed `pickNewProject` immediately after `pickNewWorktree` — confirm parameter order matches before saving.)

- [ ] **Step 3: Build the full solution**

Run: `dotnet build CodeScope.sln -c Debug`
Expected: clean build, no warnings introduced.

- [ ] **Step 4: Run the full test suite**

Run: `dotnet test CodeScope.sln -c Debug`
Expected: all green, with the 4 new `GitServiceCloneTests` running (or skipping when git is missing). Total count rises by 4 from the prior baseline.

- [ ] **Step 5: Smoke-test the dev build manually**

```pwsh
$env:CODESCOPE_DEV = "1"
dotnet run --project src/CodeScope.App
```

Verify in the running app:
1. Click the "+" in the sidebar (or empty-state CTA) — the new dialog opens.
2. *Existing folder* mode → Browse… → pick a folder → Add closes the dialog and the project appears in the sidebar.
3. *Clone from URL* mode → paste any small public repo URL (e.g. `https://github.com/octocat/Hello-World.git`) → Add. Spinner + "Cloning…" appears, dialog stays open. On success: dialog closes, project appears, toast "Project cloned" fires.
4. *Clone with bad URL* (e.g. `https://example.invalid/x.git`) → Add. Spinner runs, then the dialog re-enables fields and renders the git error inline beneath the URL field.
5. *Clone + Cancel mid-flight* on a slow URL → Cancel button cancels the clone and re-enables the form. The target directory should not be left on disk.
6. Drag-drop a folder onto the sidebar — still works, unchanged path.

- [ ] **Step 6: Commit**

```bash
git add src/CodeScope.Ui/Dialogs/NewProjectRequest.cs \
        src/CodeScope.Ui/Dialogs/NewProjectDialog.xaml \
        src/CodeScope.Ui/Dialogs/NewProjectDialog.xaml.cs \
        src/CodeScope.Ui/ViewModels/SidebarViewModel.cs \
        src/CodeScope.Ui/ViewModels/SidebarViewModel.Commands.cs \
        src/CodeScope.App/App.xaml.cs
git commit -m "feat: add-project from git URL with inline cloning state (#20)"
```

---

### Task 9: Update `docs/HANDOFF.md`

**Files:**
- Modify: `docs/HANDOFF.md`

- [ ] **Step 1: Add a session entry**

At the top of the session-list area (right after the cursor / current-focus header), insert a new "Session 24 — Add project from a git URL (shipped)" block summarising:
- Issue #20 closed.
- New `IGitService.CloneAsync` + 4 tests.
- New `NewProjectDialog` with mode toggle (Existing folder / Clone from URL) and inline busy state.
- `SidebarViewModel.AddProjectAsync` rewired through the dialog; drag-drop path unchanged.
- Files: list the same set as the commit.

Keep it under 25 lines — this is the new top-of-handoff entry, not a deep dive.

- [ ] **Step 2: Update the **Last updated** / **Branch** / **Head** lines at the top of HANDOFF.md to point at the new branch + commit SHA.**

- [ ] **Step 3: Commit**

```bash
git add docs/HANDOFF.md
git commit -m "docs: handoff for #20 add-project from git URL"
```

---

### Task 10: Push branch and open PR

- [ ] **Step 1: Push the branch**

```bash
git push -u origin feat/20-clone-from-url
```

- [ ] **Step 2: Open the PR**

```bash
gh pr create --title "feat: add project from a git URL (#20)" --body "$(cat <<'EOF'
## Summary
- Adds an "Add project" dialog with two modes: pick an existing folder (today's behaviour) or clone from a git URL.
- New `IGitService.CloneAsync` shells out to `git clone` and is owned by the dialog.
- Cloning shows an inline spinner + "Cloning…" caption; Cancel kills the git process; failures render the stderr inline so the user can fix and retry without re-typing.

Closes #20.

## Test plan
- [ ] `dotnet test CodeScope.sln -c Debug` — full suite green; 4 new `GitServiceCloneTests`.
- [ ] Smoke: existing-folder mode adds a project as before.
- [ ] Smoke: clone-from-URL with a real repo lands and adds.
- [ ] Smoke: clone with bad URL surfaces error inline; dialog re-enables.
- [ ] Smoke: cancel during clone returns cleanly; partial dir cleaned.
EOF
)"
```

---

## Self-Review

**Spec coverage:**
- *NewProjectDialog with two modes* → Tasks 5–6.
- *`IGitService.CloneAsync`* → Tasks 1–3.
- *Inline busy state with spinner + cancel* → Task 6 (`SetBusy`, `_cloneCts`).
- *Auto-derived folder name* → Task 6 (`OnUrlChanged` / `DeriveRepoName`).
- *URL validation* → Task 6 (`IsValidGitUrl`).
- *Default parent folder = most-recent project's parent → `%USERPROFILE%\source\repos`* → Task 7 (`DefaultCloneParent`).
- *Pre-flight: target must not exist non-empty* → Task 3 (CloneAsync) + Task 6 (dialog pre-check before showing busy).
- *Drag-drop unchanged* → Task 7 explicitly leaves `AddProjectByPathAsync` alone.
- *4 Core tests (happy / target-exists / garbage URL / cancellation)* → Task 2.

**Placeholder scan:** all code blocks present; no "TBD"/"TODO"/"add appropriate handling".

**Type consistency:** `NewProjectResult(ExistingFolder, ClonedPath, WasCloned)` is consistent across Tasks 4, 6, 7. `Func<NewProjectRequest, Task<NewProjectResult?>>` is identical in `SidebarViewModel`, `App.xaml.cs`, and `NewProjectDialog.PromptAsync`. `IGitService.CloneAsync(url, parentDir, folderName, ct)` signature matches across interface, implementation, dialog call, and tests.
