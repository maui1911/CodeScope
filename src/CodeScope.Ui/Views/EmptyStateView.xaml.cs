using System.Windows.Controls;

namespace NoScope.CodeScope.Ui.Views;

/// <summary>
/// First-run hero shown when no projects are registered. Inherits <c>DataContext</c>
/// from <c>MainWindow</c> so the "Add your first project" CTA binds to
/// <c>Sidebar.AddProjectCommand</c>.
/// </summary>
public partial class EmptyStateView : UserControl
{
    public EmptyStateView() => InitializeComponent();
}
