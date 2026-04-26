namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>
/// Semantic session state — drives the status dot in the sidebar, tab strip, and status bar.
/// Two-state model: <see cref="Ready"/> (green, awaiting your input) vs <see cref="Busy"/>
/// (red, agent is working — covers both Composing and PendingToolUse). Selection visuals are
/// independent.
/// </summary>
public enum TabStatus
{
    Ready,
    Busy,
}
