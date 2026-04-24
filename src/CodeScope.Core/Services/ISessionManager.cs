namespace NoScope.CodeScope.Core.Services;

/// <summary>
/// Translates high-level UI actions (new tab, close tab) into <see cref="SessionDescriptor"/>s
/// that the view layer consumes to bind the terminal control. Keeps launch policy (shell flags,
/// UTF-8 setup, pwsh vs cmd) in Core.
/// </summary>
public interface ISessionManager
{
    /// <summary>
    /// Build a shell-only session pinned to <paramref name="workingDirectory"/>.
    /// </summary>
    /// <exception cref="ArgumentException">
    /// <paramref name="workingDirectory"/> is null, empty, or whitespace. Unlike the
    /// rest of Core (which reports failure via <see cref="Result{T}"/>), session construction
    /// throws on invariant violations because callers (VM commands) already pre-filter
    /// the path — an empty <c>workingDirectory</c> here is a programming bug, not a user error.
    /// </exception>
    SessionDescriptor CreateShellSession(string workingDirectory, string? id = null);

    /// <summary>
    /// Build a session that launches <paramref name="agent"/> inside pwsh.
    /// When <paramref name="resume"/> is true, <see cref="Models.AgentProfile.ResumeArgs"/>
    /// is used instead of <c>NewSessionArgs</c> so Claude Code / codex / OpenCode reopen
    /// their last conversation in this working directory.
    /// </summary>
    /// <exception cref="ArgumentException"><paramref name="workingDirectory"/> is null/empty/whitespace.</exception>
    /// <exception cref="ArgumentNullException"><paramref name="agent"/> is null.</exception>
    SessionDescriptor CreateAgentSession(string workingDirectory, Models.AgentProfile agent, string? id = null, bool resume = false, string? agentSessionId = null);
}
