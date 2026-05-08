using System.Windows;
using System.Windows.Controls;
using System.Windows.Data;
using System.Windows.Media;
using System.Windows.Shapes;

namespace NoScope.CodeScope.Ui.Views;

/// <summary>
/// Shared builders for the dynamic dark context menus used by the sidebar tree and
/// the tab strip. The styling lives globally in <c>Styles/ContextMenuStyles.xaml</c>;
/// this factory just produces MenuItems wired to the right icons, shortcuts, and
/// header/group/danger Tag conventions those styles key off.
/// </summary>
internal static class ContextMenuFactory
{
    public static MenuItem BuildItem(string header, string iconKey, string? shortcut, Action onClick)
    {
        var mi = new MenuItem
        {
            Header = header,
            Icon = IconFor(iconKey),
            InputGestureText = shortcut ?? string.Empty,
        };
        mi.Click += (_, _) => onClick();
        return mi;
    }

    public static MenuItem BuildGroupLabel(string text)
        => new()
        {
            Header = text,
            Tag = "group",
        };

    public static Separator BuildSeparator() => new();

    public static MenuItem BuildContextHeader(string dotBrushKey, string title, string subtitle)
    {
        var grid = new Grid();
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var dot = new Ellipse
        {
            Width = 6,
            Height = 6,
            VerticalAlignment = VerticalAlignment.Center,
            Fill = Application.Current?.TryFindResource(dotBrushKey) as Brush ?? Brushes.DeepSkyBlue,
        };
        Grid.SetColumn(dot, 0);
        grid.Children.Add(dot);

        var titleText = new TextBlock
        {
            Text = title,
            Margin = new Thickness(8, 0, 0, 0),
            VerticalAlignment = VerticalAlignment.Center,
            FontFamily = (FontFamily?)Application.Current?.TryFindResource("Fig.Font.Mono") ?? new FontFamily("Consolas"),
            FontSize = 11,
            Foreground = (Brush?)Application.Current?.TryFindResource("Text.Primary") ?? Brushes.White,
        };
        Grid.SetColumn(titleText, 1);
        grid.Children.Add(titleText);

        var subtitleText = new TextBlock
        {
            Text = subtitle,
            VerticalAlignment = VerticalAlignment.Center,
            FontSize = 10,
            Foreground = (Brush?)Application.Current?.TryFindResource("Text.Faint") ?? Brushes.Gray,
        };
        Grid.SetColumn(subtitleText, 3);
        grid.Children.Add(subtitleText);

        return new MenuItem
        {
            Header = grid,
            Tag = "header",
        };
    }

    public static Path? IconFor(string geometryKey)
    {
        if (Application.Current?.TryFindResource(geometryKey) is not Geometry geom) { return null; }
        var path = new Path
        {
            Data = geom,
            Width = 14,
            Height = 14,
            Stretch = Stretch.None,
            StrokeThickness = 1.4,
            StrokeStartLineCap = PenLineCap.Round,
            StrokeEndLineCap = PenLineCap.Round,
            StrokeLineJoin = PenLineJoin.Round,
            Fill = Brushes.Transparent,
            SnapsToDevicePixels = true,
        };
        path.SetBinding(Shape.StrokeProperty, new Binding
        {
            RelativeSource = new RelativeSource(RelativeSourceMode.FindAncestor)
            {
                AncestorType = typeof(ContentPresenter),
            },
            Path = new PropertyPath("(0)", TextBlock.ForegroundProperty),
            FallbackValue = Application.Current?.TryFindResource("Text.Secondary") ?? Brushes.Gainsboro,
        });
        return path;
    }

    /// <summary>
    /// Returns true when the project at <paramref name="projectPath"/> has a configured
    /// <c>[remote "origin"]</c>. Worktrees inherit the origin from their owning project's
    /// .git config, which lives at the project root (not inside the worktree).
    /// </summary>
    public static bool HasOriginRemote(string? projectPath)
    {
        try
        {
            if (!TryGetGitConfigPath(projectPath, out var configPath) || configPath is null) { return false; }
            var text = System.IO.File.ReadAllText(configPath);
            return text.Contains("[remote \"origin\"]", StringComparison.Ordinal);
        }
        catch
        {
            return false;
        }
    }

    private static bool TryGetGitConfigPath(string? projectPath, out string? configPath)
    {
        configPath = null;
        if (string.IsNullOrWhiteSpace(projectPath)) { return false; }

        var gitPath = System.IO.Path.Combine(projectPath, ".git");

        if (System.IO.Directory.Exists(gitPath))
        {
            var directConfigPath = System.IO.Path.Combine(gitPath, "config");
            if (System.IO.File.Exists(directConfigPath))
            {
                configPath = directConfigPath;
                return true;
            }
            return false;
        }

        if (!System.IO.File.Exists(gitPath)) { return false; }

        var gitPointer = System.IO.File.ReadAllText(gitPath).Trim();
        const string gitDirPrefix = "gitdir:";
        if (!gitPointer.StartsWith(gitDirPrefix, StringComparison.OrdinalIgnoreCase)) { return false; }

        var gitDir = gitPointer[gitDirPrefix.Length..].Trim();
        if (string.IsNullOrWhiteSpace(gitDir)) { return false; }

        var resolvedGitDir = System.IO.Path.IsPathRooted(gitDir)
            ? gitDir
            : System.IO.Path.GetFullPath(System.IO.Path.Combine(projectPath, gitDir));

        var resolvedConfigPath = System.IO.Path.Combine(resolvedGitDir, "config");
        if (!System.IO.File.Exists(resolvedConfigPath)) { return false; }

        configPath = resolvedConfigPath;
        return true;
    }
}
