using System.Windows;
using System.Windows.Input;

namespace NoScope.CodeScope.Ui.Dialogs;

public partial class RenameDialog : Window
{
    private void OnChromeDrag(object sender, MouseButtonEventArgs e)
    {
        if (e.ChangedButton == MouseButton.Left) { DragMove(); }
    }

    public string? ResultName { get; private set; }

    private RenameDialog(string current, string title)
    {
        InitializeComponent();
        Title = title;
        HeaderText.Text = title;
        NameBox.Text = current;
        NameBox.Focus();
        NameBox.SelectAll();
    }

    public static string? Prompt(string current, string title = "Rename session")
    {
        var dlg = new RenameDialog(current, title) { Owner = Application.Current?.MainWindow };
        return dlg.ShowDialog() == true ? dlg.ResultName : null;
    }

    private void OnOk(object sender, RoutedEventArgs e)
    {
        ResultName = NameBox.Text?.Trim();
        DialogResult = !string.IsNullOrWhiteSpace(ResultName);
        Close();
    }

    private void OnCancel(object sender, RoutedEventArgs e)
    {
        DialogResult = false;
        Close();
    }
}
