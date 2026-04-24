namespace NoScope.CodeScope.Ui.ViewModels;

/// <summary>One row in the Ctrl+K command palette.</summary>
public sealed record PaletteAction(string Title, string? Subtitle, Func<Task> Execute, string? Icon = null)
{
    /// <summary>Display text for list rows: "<Title>" or "<Title>  —  <Subtitle>". Used by the ranker.</summary>
    public string Display => string.IsNullOrWhiteSpace(Subtitle) ? Title : $"{Title}   —   {Subtitle}";
}
