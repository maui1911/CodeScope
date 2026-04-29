using NoScope.CodeScope.Core.Models;
using Microsoft.Extensions.Logging;

namespace NoScope.CodeScope.Core.Services;

/// <inheritdoc />
public sealed class SessionManager : ISessionManager
{
    private readonly ILogger<SessionManager> _logger;

    public SessionManager(ILogger<SessionManager> logger)
    {
        _logger = logger;
    }

    public SessionDescriptor CreateShellSession(string workingDirectory, string? id = null)
    {
        if (string.IsNullOrWhiteSpace(workingDirectory))
        {
            throw new ArgumentException("workingDirectory is required", nameof(workingDirectory));
        }

        if (!Directory.Exists(workingDirectory))
        {
            _logger.LogWarning("Session working directory does not exist: {Path}", workingDirectory);
        }

        // Absolute pwsh path, quoted if it contains spaces — ConPTY's CreateProcess takes a single
        // lpCommandLine string and won't correctly identify the exe otherwise.
        // -WorkingDirectory lands pwsh in the session folder while still loading the user profile,
        // so oh-my-posh / PSReadLine / aliases all come up as in Windows Terminal.
        var shell = ResolveShell();
        if (shell.Contains(' ') && shell[0] != '"') { shell = $"\"{shell}\""; }
        // `-NoExit -NoLogo` keep pwsh interactive after profile load — without it the pty
        // sometimes sees stdin close and pwsh exits immediately, producing the dreaded
        // "Session Terminated" pane. Mirrors the agent-session launch line.
        var args = new[] { "-NoExit", "-NoLogo", "-WorkingDirectory", Quote(workingDirectory) };

        return new SessionDescriptor
        {
            Id = id ?? Guid.NewGuid().ToString("n"),
            WorkingDirectory = workingDirectory,
            Shell = shell,
            ShellArgs = args,
            Title = Path.GetFileName(workingDirectory.TrimEnd(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar))
                is { Length: > 0 } name
                ? name
                : workingDirectory,
        };
    }

    public SessionDescriptor CreateAgentSession(string workingDirectory, AgentProfile agent, string? id = null, bool resume = false, string? agentSessionId = null)
    {
        if (string.IsNullOrWhiteSpace(workingDirectory))
        {
            throw new ArgumentException("workingDirectory is required", nameof(workingDirectory));
        }

        ArgumentNullException.ThrowIfNull(agent);

        // Session-id-aware launch path:
        //   fresh + agentSessionId + SessionIdFlag  →  "<flag> <uuid>" + NewSessionArgs
        //   resume + agentSessionId + ResumeByIdArgs →  ResumeByIdArgs + "<uuid>"
        //   else                                    →  ResumeArgs / NewSessionArgs verbatim
        //
        // When the last entry in ResumeByIdArgs ends with '=' (e.g. "--resume=") the id is
        // concatenated directly instead of appended as a separate arg. This covers CLIs
        // like Copilot whose optional-value flags require the `=` syntax.
        IReadOnlyList<string> argList;
        if (resume)
        {
            argList = agentSessionId is { Length: > 0 } rid && agent.ResumeByIdArgs.Count > 0
                ? JoinResumeByIdArgs(agent.ResumeByIdArgs, rid)
                : agent.ResumeArgs;
        }
        else
        {
            if (agentSessionId is { Length: > 0 } nid && !string.IsNullOrEmpty(agent.SessionIdFlag))
            {
                argList = [agent.SessionIdFlag, nid, .. agent.NewSessionArgs];
            }
            else
            {
                argList = agent.NewSessionArgs;
            }
        }
        var agentArgs = argList.Count == 0
            ? string.Empty
            : " " + string.Join(' ', argList);
        // Wrapped in pwsh scriptblock so -NoExit keeps the shell alive after the agent exits.
        // Extra-quoted for ConPTY command-line parsing (pwsh's own argv splitter would otherwise
        // treat '&' / '{' as separate tokens and never feed the block to -Command).
        var agentCall = Quote($"& {{ {agent.Command}{agentArgs} }}");

        var folderName = Path.GetFileName(workingDirectory.TrimEnd(
            Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar));
        if (string.IsNullOrEmpty(folderName))
        {
            folderName = workingDirectory;
        }

        return new SessionDescriptor
        {
            Id = id ?? Guid.NewGuid().ToString("n"),
            WorkingDirectory = workingDirectory,
            Shell = "pwsh.exe",
            ShellArgs =
            [
                "-NoExit",
                "-NoLogo",
                "-WorkingDirectory",
                Quote(workingDirectory),
                "-Command",
                agentCall,
            ],
            Title = $"{agent.DisplayName} · {folderName}",
        };
    }

    /// <summary>
    /// Wraps <paramref name="value"/> in double quotes if it contains whitespace — which is almost
    /// always the case for Windows paths. Safe to always quote: pwsh strips the outer quotes.
    /// </summary>
    private static string Quote(string value) =>
        value.Length > 0 && value[0] == '"' ? value : $"\"{value}\"";

    /// <summary>
    /// Combines <paramref name="resumeByIdArgs"/> with <paramref name="sessionId"/>.
    /// When the last element ends with <c>=</c> the id is concatenated directly
    /// (e.g. <c>["--resume="]</c> + <c>"abc"</c> → <c>["--resume=abc"]</c>).
    /// Otherwise the id is appended as a separate arg.
    /// </summary>
    private static IReadOnlyList<string> JoinResumeByIdArgs(IReadOnlyList<string> resumeByIdArgs, string sessionId)
    {
        var last = resumeByIdArgs[^1];
        if (last.EndsWith('='))
        {
            var result = new List<string>(resumeByIdArgs.Count);
            for (var i = 0; i < resumeByIdArgs.Count - 1; i++)
                result.Add(resumeByIdArgs[i]);
            result.Add($"{last}{sessionId}");
            return result;
        }
        return [.. resumeByIdArgs, sessionId];
    }

    private static string ResolveShell()
    {
        // Prefer pwsh 7 (cross-platform PowerShell) so oh-my-posh / PSReadLine / user profile
        // all light up. Fall back to Windows PowerShell, then cmd as a last resort.
        string[] candidates =
        [
            @"C:\Program Files\PowerShell\7\pwsh.exe",
            @"C:\Program Files\PowerShell\7-preview\pwsh.exe",
            @"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe",
            @"C:\Windows\System32\cmd.exe",
        ];
        foreach (var c in candidates)
        {
            if (File.Exists(c)) { return c; }
        }
        return "pwsh.exe";
    }
}
