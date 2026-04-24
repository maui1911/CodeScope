using System.Windows;
using System.Windows.Input;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Dialogs;

public partial class CommandPaletteDialog : Window
{
    public PaletteAction? Picked { get; private set; }

    private CommandPaletteDialog(IEnumerable<PaletteAction> actions)
    {
        InitializeComponent();
        DataContext = new CommandPaletteViewModel(actions);
        Loaded += (_, _) => QueryBox.Focus();
    }

    /// <summary>Opens the palette as a modal owned by the app's main window. Returns null on Esc/close.</summary>
    public static PaletteAction? Prompt(IEnumerable<PaletteAction> actions)
    {
        var dlg = new CommandPaletteDialog(actions) { Owner = Application.Current?.MainWindow };
        return dlg.ShowDialog() == true ? dlg.Picked : null;
    }

    private void OnQueryKeyDown(object sender, KeyEventArgs e)
    {
        switch (e.Key)
        {
            case Key.Escape:
                DialogResult = false;
                Close();
                break;
            case Key.Enter:
                Commit();
                e.Handled = true;
                break;
            case Key.Down when Results.Items.Count > 0:
                Results.SelectedIndex = Math.Min(Results.SelectedIndex + 1, Results.Items.Count - 1);
                e.Handled = true;
                break;
            case Key.Up when Results.Items.Count > 0:
                Results.SelectedIndex = Math.Max(Results.SelectedIndex - 1, 0);
                e.Handled = true;
                break;
        }
    }

    private void OnResultsKeyDown(object sender, KeyEventArgs e)
    {
        if (e.Key == Key.Enter) { Commit(); e.Handled = true; }
        else if (e.Key == Key.Escape) { DialogResult = false; Close(); }
    }

    private void OnResultsDoubleClick(object sender, MouseButtonEventArgs e) => Commit();

    private void Commit()
    {
        if (DataContext is CommandPaletteViewModel vm && vm.Selected is { } action)
        {
            Picked = action;
            DialogResult = true;
            Close();
        }
    }
}
