using System.Windows.Controls;
using System.Windows.Input;

namespace NoScope.CodeScope.App.Toasts;

/// <summary>
/// Single-toast surface. The host's MouseEnter / MouseLeave handles pause-on-hover for
/// every toast in the stack at once (spec §04 "hover anywhere = pause every meter").
/// </summary>
public partial class ToastView : UserControl
{
    public ToastView()
    {
        InitializeComponent();
        MouseEnter += (_, _) => (DataContext as ToastItemViewModel)?.Pause();
        MouseLeave += (_, _) => (DataContext as ToastItemViewModel)?.Resume();
    }
}
