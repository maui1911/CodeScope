using System.Windows.Controls;

namespace NoScope.CodeScope.App.Toasts;

/// <summary>
/// Single-toast surface. Hover-to-pause is handled at the <see cref="ToastHostView"/>
/// level so a single MouseEnter pauses the meters of every visible toast (spec §04
/// "hover anywhere = pause every meter") — see <see cref="ToastHostView"/>.
/// </summary>
public partial class ToastView : UserControl
{
    public ToastView()
    {
        InitializeComponent();
    }
}
