namespace NoScope.CodeScope.Ui.Services;

/// <summary>
/// Updates the application's taskbar overlay icon to reflect aggregate agent activity.
/// </summary>
public interface ITaskbarBadgeService
{
    /// <summary>
    /// Apply a new badge state. The service decides the visual:
    /// <list type="bullet">
    ///   <item><c>agentTabCount == 0</c> → no overlay (cleared).</item>
    ///   <item><c>busyCount == 0 &amp;&amp; agentTabCount &gt; 0</c> → green dot.</item>
    ///   <item>
    ///     <c>busyCount &gt;= 1</c> → red disc with the busy digit;
    ///     <c>busyCount &gt; 9</c> renders <c>9⁺</c>.
    ///   </item>
    /// </list>
    /// </summary>
    void Apply(int busyCount, int agentTabCount);
}
