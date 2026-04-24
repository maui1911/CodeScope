namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Parameters to hand to the UI layer when asked to spawn a session.
/// Pure data — no terminal control dependency so <see cref="NoScope.CodeScope.Core"/> stays UI-free.
/// </summary>
public sealed record SessionDescriptor
{
    public required string Id { get; init; }

    /// <summary>Absolute working directory.</summary>
    public required string WorkingDirectory { get; init; }

    /// <summary>Executable — typically "pwsh.exe".</summary>
    public required string Shell { get; init; }

    /// <summary>Args passed to the shell, already split.</summary>
    public IReadOnlyList<string> ShellArgs { get; init; } = [];

    /// <summary>Display title shown on the tab.</summary>
    public required string Title { get; init; }
}
