using System.Windows;
using System.Windows.Controls;
using NoScope.CodeScope.Ui.ViewModels;
using EasyWindowsTerminalControl;
using Microsoft.Terminal.Wpf;

namespace NoScope.CodeScope.Ui.Views;

/// <summary>
/// Hosts an <see cref="EasyTerminalControl"/>.
///
/// <para>EasyTerminalControl auto-starts its inner <c>TerminalControl.Loaded</c> with whatever
/// <c>StartupCommandLine</c> is set at that moment. In a TabControl ItemTemplate scenario the
/// UserControl's DataContext is propagated <em>after</em> the inner TerminalControl Loaded fires,
/// so the default <c>"powershell.exe"</c> would be launched with the wrong (CodeScope's own) cwd.
/// We bypass that auto-start by constructing a <see cref="TermPTY"/> manually and calling
/// <c>Start</c> with the view-model's <c>CommandLine</c> — the advanced pattern from the
/// EasyWindowsTerminalControl README.</para>
///
/// <para>Gate on both DataContext + Loaded because WPF can raise them in either order. The app
/// itself must have a console allocated (see <c>App.EnsureHiddenConsole</c>) — without one the
/// ConPTY session dies milliseconds after the child starts.</para>
/// </summary>
public partial class SessionTabView : UserControl
{
    private bool _started;
    private bool _isLoaded;

    public SessionTabView()
    {
        InitializeComponent();
        Terminal.Theme = BuildTheme();
        DataContextChanged += (_, _) => TryStartShell();
        Loaded += (_, _) => { _isLoaded = true; TryStartShell(); };
    }

    private static TerminalTheme BuildTheme() => new()
    {
        DefaultBackground = EasyTerminalControl.ColorToVal(System.Windows.Media.Color.FromArgb(255, 10, 10, 10)),
        DefaultForeground = EasyTerminalControl.ColorToVal(System.Windows.Media.Color.FromArgb(255, 245, 245, 245)),
        DefaultSelectionBackground = 0x444444,
        CursorStyle = CursorStyle.BlinkingBlock,
        ColorTable =
        [
            0x0C0C0C, 0x1F0FC5, 0x0EA113, 0x009CC1, 0xDA3700, 0x981788, 0xDD963A, 0xCCCCCC,
            0x767676, 0x5648E7, 0x0CC616, 0xA5F1F9, 0xFF783B, 0x9E00B4, 0xD6D661, 0xF2F2F2,
        ],
    };

    private void TryStartShell()
    {
        if (_started) { return; }
        if (!_isLoaded) { return; }
        if (DataContext is not SessionTabViewModel vm || string.IsNullOrWhiteSpace(vm.CommandLine))
        {
            return;
        }
        _started = true;

        // Discard whatever the control auto-started (the default shell that dies without a console).
        var disconnected = Terminal.DisconnectConPTYTerm();
        // The auto-started shell may already be gone — ignore teardown errors, but trace them.
        try { disconnected?.CloseStdinToApp(); }
        catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"[SessionTabView] CloseStdinToApp: {ex.Message}"); }
        try { disconnected?.StopExternalTermOnly(); }
        catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"[SessionTabView] StopExternalTermOnly: {ex.Message}"); }

        var term = new TermPTY();
        term.TermReady += (_, _) => Dispatcher.Invoke(() =>
        {
            Terminal.ConPTYTerm = term;
            Terminal.Focus();
        });

        var cmd = vm.CommandLine;
        _ = Task.Run(() =>
        {
            try { term.Start(cmd, consoleWidth: 120, consoleHeight: 32, logOutput: false); }
            catch (Exception ex)
            {
                // Process exit races — not fatal, the connection was already wired; surface in traces.
                System.Diagnostics.Debug.WriteLine($"[SessionTabView] term.Start: {ex.Message}");
            }
        });
    }
}
