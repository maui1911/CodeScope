namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>Semantic session state — drives the tab status dot (§Top-bar spec §3).</summary>
public enum TabStatus
{
    Idle,
    Active,
    Wait,
}
