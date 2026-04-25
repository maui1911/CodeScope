namespace NoScope.CodeScope.Ui;

internal static class AutomationIds
{
    /// <summary>
    /// Reduces an arbitrary display string to a token safe for UIA AutomationId values.
    /// Non-alphanumeric runs collapse to underscores; leading/trailing underscores are
    /// trimmed; null/empty/whitespace-only inputs return <c>"unknown"</c>.
    /// </summary>
    public static string SafeToken(string? s)
    {
        if (string.IsNullOrWhiteSpace(s)) { return "unknown"; }
        var token = new string([.. s.Select(c => char.IsLetterOrDigit(c) ? c : '_')]).Trim('_');
        return string.IsNullOrEmpty(token) ? "unknown" : token;
    }
}
