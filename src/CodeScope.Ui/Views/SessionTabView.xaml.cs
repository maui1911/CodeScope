using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
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
        Unloaded += (_, _) => TeardownShell();
        // Tunneling preview so we win over the inner TerminalControl's Win32 input path.
        PreviewKeyDown += OnTerminalPreviewKeyDown;
    }

    /// <summary>
    /// Terminal-friendly clipboard bindings. Windows-Terminal-style semantics:
    /// <list type="bullet">
    ///   <item><c>Ctrl+C</c> with a non-empty selection → copy &amp; clear selection; empty selection
    ///     falls through as <c>SIGINT</c> to the shell.</item>
    ///   <item><c>Ctrl+Shift+C</c> → always copy (never SIGINT).</item>
    ///   <item><c>Ctrl+V</c> / <c>Ctrl+Shift+V</c> → paste clipboard text into the ConPTY. Line
    ///     endings are normalised to <c>\r</c> because the shell treats each CR as Enter —
    ///     forwarding <c>\r\n</c> would insert blank commands between pasted lines.</item>
    /// </list>
    /// </summary>
    private void OnTerminalPreviewKeyDown(object sender, KeyEventArgs e)
    {
        var ctrl = (Keyboard.Modifiers & ModifierKeys.Control) == ModifierKeys.Control;
        if (!ctrl) { return; }
        var shift = (Keyboard.Modifiers & ModifierKeys.Shift) == ModifierKeys.Shift;

        if (e.Key == Key.C)
        {
            var sel = Terminal.Terminal?.GetSelectedText();
            if (!string.IsNullOrEmpty(sel))
            {
                TrySetClipboard(sel);
                e.Handled = true;
                return;
            }
            // No selection: only swallow on Shift (explicit copy shortcut). Plain Ctrl+C
            // drops through so pwsh / claude / etc. get their SIGINT.
            if (shift) { e.Handled = true; }
        }
        else if (e.Key == Key.V)
        {
            var text = TryGetClipboardText();
            if (string.IsNullOrEmpty(text)) { return; }
            var normalised = text.Replace("\r\n", "\r").Replace("\n", "\r");
            Terminal.ConPTYTerm?.WriteToTerm(normalised.AsSpan());
            e.Handled = true;
        }
        else if (e.Key == Key.O && shift)
        {
            // Ctrl+Shift+O → treat the current selection as a URL and open it in the default
            // browser. Microsoft.Terminal.Wpf does not implement OSC-8 hyperlinks or Ctrl+click
            // URL detection, so selection-then-shortcut is the pragmatic substitute.
            if (TryOpenSelectedUrl()) { e.Handled = true; }
        }
    }

    private bool TryOpenSelectedUrl()
    {
        var sel = Terminal.Terminal?.GetSelectedText();
        if (string.IsNullOrWhiteSpace(sel)) { return false; }
        var trimmed = sel.Trim();
        // The terminal wraps long URLs with newlines when they hit the right edge — stitch the
        // line fragments back together before validation or a perfectly good URL looks malformed.
        trimmed = trimmed.Replace("\r\n", string.Empty).Replace("\n", string.Empty).Replace("\r", string.Empty);
        if (!Uri.TryCreate(trimmed, UriKind.Absolute, out var uri)) { return false; }
        if (uri.Scheme != Uri.UriSchemeHttp && uri.Scheme != Uri.UriSchemeHttps) { return false; }
        try
        {
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo(uri.AbsoluteUri) { UseShellExecute = true });
            return true;
        }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[SessionTabView] Open URL failed: {ex.Message}");
            return false;
        }
    }

    private void OnContextMenuOpened(object sender, RoutedEventArgs e)
    {
        var sel = Terminal.Terminal?.GetSelectedText();
        var hasSelection = !string.IsNullOrEmpty(sel);
        MenuCopy.IsEnabled = hasSelection;
        MenuOpenUrl.IsEnabled = hasSelection && LooksLikeUrl(sel);
    }

    private static bool LooksLikeUrl(string? text)
    {
        if (string.IsNullOrWhiteSpace(text)) { return false; }
        var t = text.Trim().Replace("\r\n", string.Empty).Replace("\n", string.Empty).Replace("\r", string.Empty);
        return Uri.TryCreate(t, UriKind.Absolute, out var uri)
            && (uri.Scheme == Uri.UriSchemeHttp || uri.Scheme == Uri.UriSchemeHttps);
    }

    private void OnOpenUrlClicked(object sender, RoutedEventArgs e) => TryOpenSelectedUrl();

    private void OnCopyClicked(object sender, RoutedEventArgs e)
    {
        var sel = Terminal.Terminal?.GetSelectedText();
        if (!string.IsNullOrEmpty(sel)) { TrySetClipboard(sel); }
    }

    private void OnPasteClicked(object sender, RoutedEventArgs e)
    {
        var text = TryGetClipboardText();
        if (string.IsNullOrEmpty(text)) { return; }
        var normalised = text.Replace("\r\n", "\r").Replace("\n", "\r");
        Terminal.ConPTYTerm?.WriteToTerm(normalised.AsSpan());
    }

    private static void TrySetClipboard(string text)
    {
        // Clipboard is a shared resource — another app can hold OpenClipboard momentarily and
        // WPF raises COMException. One retry is enough in practice; silently dropping the copy
        // is friendlier than crashing the terminal.
        try { Clipboard.SetText(text); }
        catch (System.Runtime.InteropServices.COMException)
        {
            try { Clipboard.SetText(text); }
            catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"[SessionTabView] Clipboard set: {ex.Message}"); }
        }
        catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"[SessionTabView] Clipboard set: {ex.Message}"); }
    }

    private static string? TryGetClipboardText()
    {
        try { return Clipboard.ContainsText() ? Clipboard.GetText() : null; }
        catch (Exception ex)
        {
            System.Diagnostics.Debug.WriteLine($"[SessionTabView] Clipboard get: {ex.Message}");
            return null;
        }
    }

    /// <summary>
    /// Kills the ConPTY child + pipes when the hosting tab is removed. Without this the pwsh
    /// process lingers (ref'd only via the local <c>term</c> captured in <see cref="TryStartShell"/>)
    /// and keeps a Windows file lock on its cwd — which breaks <c>git worktree remove</c>
    /// downstream when the user deletes the worktree the session was pinned to.
    /// </summary>
    private void TeardownShell()
    {
        if (!_started) { return; }
        var term = Terminal.DisconnectConPTYTerm();
        try { term?.CloseStdinToApp(); }
        catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"[SessionTabView] Teardown CloseStdin: {ex.Message}"); }
        try { term?.StopExternalTermOnly(); }
        catch (Exception ex) { System.Diagnostics.Debug.WriteLine($"[SessionTabView] Teardown StopExternal: {ex.Message}"); }
        _started = false;
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
