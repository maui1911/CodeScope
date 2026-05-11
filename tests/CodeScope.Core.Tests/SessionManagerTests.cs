using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

public sealed class SessionManagerTests
{
    [Fact]
    public void CreateShellSession_Rejects_Empty_Path()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);

        var act = () => manager.CreateShellSession("");

        act.Should().Throw<ArgumentException>();
    }

    [Fact]
    public void CreateShellSession_Uses_Pwsh_With_WorkingDirectory()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);

        var workingDir = Path.GetTempPath();
        var descriptor = manager.CreateShellSession(workingDir);

        // ResolveShell returns the absolute path to the shell (quoted when it contains spaces).
        // We just care that the pwsh/powershell family was chosen and that -WorkingDirectory
        // lands pwsh in the session folder with the user profile loaded.
        var shellLower = descriptor.Shell.Trim('"').ToLowerInvariant();
        shellLower.Should().Match(s => s.EndsWith("pwsh.exe") || s.EndsWith("powershell.exe"));
        descriptor.ShellArgs.Should().Contain("-WorkingDirectory");
        descriptor.ShellArgs.Should().Contain(a => a.Contains(workingDir.TrimEnd('\\')));
    }

    [Fact]
    public void CreateShellSession_Derives_Title_From_Folder_Name()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);
        var folder = Path.Combine(Path.GetTempPath(), "my-repo");
        Directory.CreateDirectory(folder);
        try
        {
            var descriptor = manager.CreateShellSession(folder);

            descriptor.Title.Should().Be("my-repo");
        }
        finally
        {
            Directory.Delete(folder);
        }
    }

    [Fact]
    public void CreateShellSession_Assigns_Id_When_Not_Provided()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);

        var a = manager.CreateShellSession(Path.GetTempPath());
        var b = manager.CreateShellSession(Path.GetTempPath());

        a.Id.Should().NotBeNullOrWhiteSpace();
        a.Id.Should().NotBe(b.Id);
    }

    [Fact]
    public void CreateAgentSession_Wraps_Agent_Command_Inside_Pwsh_Command()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);
        var agent = new NoScope.CodeScope.Core.Models.AgentProfile
        {
            Id = "claude", DisplayName = "Claude Code", Command = "claude",
            NewSessionArgs = ["--dangerously-skip-permissions"],
        };

        var d = manager.CreateAgentSession(Path.GetTempPath(), agent);

        // Agent sessions go through ResolveShell() — same fallback chain as shell sessions —
        // so the result is the absolute pwsh/powershell path (quoted when it contains spaces).
        var shellLower = d.Shell.Trim('"').ToLowerInvariant();
        shellLower.Should().Match(s => s.EndsWith("pwsh.exe") || s.EndsWith("powershell.exe"));
        d.ShellArgs.Should().Contain("-NoExit");
        d.ShellArgs.Should().Contain("-Command");
        var payload = string.Join(' ', d.ShellArgs);
        payload.Should().Contain("claude");
        payload.Should().Contain("--dangerously-skip-permissions");
    }

    [Fact]
    public void CreateAgentSession_Title_Uses_Agent_Display_Name_And_Folder()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);
        var folder = Path.Combine(Path.GetTempPath(), "my-repo-x");
        Directory.CreateDirectory(folder);
        try
        {
            var agent = new NoScope.CodeScope.Core.Models.AgentProfile
            { Id = "claude", DisplayName = "Claude Code", Command = "claude" };

            var d = manager.CreateAgentSession(folder, agent);

            d.Title.Should().Be("Claude Code · my-repo-x");
        }
        finally { Directory.Delete(folder); }
    }

    [Fact]
    public void CreateAgentSession_Fresh_With_Agent_SessionId_Injects_Flag()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);
        var agent = new NoScope.CodeScope.Core.Models.AgentProfile
        {
            Id = "claude", DisplayName = "Claude Code", Command = "claude",
            SessionIdFlag = "--session-id",
            ResumeByIdArgs = ["--resume"],
        };
        var uuid = "11111111-2222-3333-4444-555555555555";

        var d = manager.CreateAgentSession(Path.GetTempPath(), agent, agentSessionId: uuid);

        var payload = string.Join(' ', d.ShellArgs);
        payload.Should().Contain("--session-id " + uuid);
        payload.Should().NotContain("--continue");
        payload.Should().NotContain("--resume");
    }

    [Fact]
    public void CreateAgentSession_Resume_With_AgentSessionId_Uses_ResumeByIdArgs()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);
        var agent = new NoScope.CodeScope.Core.Models.AgentProfile
        {
            Id = "claude", DisplayName = "Claude Code", Command = "claude",
            ResumeArgs = ["--continue"],
            SessionIdFlag = "--session-id",
            ResumeByIdArgs = ["--resume"],
        };
        var uuid = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

        var d = manager.CreateAgentSession(Path.GetTempPath(), agent, resume: true, agentSessionId: uuid);

        var payload = string.Join(' ', d.ShellArgs);
        payload.Should().Contain("--resume " + uuid);
        payload.Should().NotContain("--continue");
    }

    [Fact]
    public void CreateAgentSession_Resume_Without_AgentSessionId_Falls_Back_To_ResumeArgs()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);
        var agent = new NoScope.CodeScope.Core.Models.AgentProfile
        {
            Id = "claude", DisplayName = "Claude Code", Command = "claude",
            ResumeArgs = ["--continue"],
            SessionIdFlag = "--session-id",
            ResumeByIdArgs = ["--resume"],
        };

        var d = manager.CreateAgentSession(Path.GetTempPath(), agent, resume: true);

        var payload = string.Join(' ', d.ShellArgs);
        payload.Should().Contain("--continue");
        payload.Should().NotContain("--resume");
    }

    [Fact]
    public void CreateAgentSession_Fresh_Without_SessionIdFlag_Behaves_Like_Before()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);
        var agent = new NoScope.CodeScope.Core.Models.AgentProfile
        {
            Id = "codex", DisplayName = "Codex", Command = "codex",
            NewSessionArgs = [],
            // SessionIdFlag deliberately not set.
        };

        var d = manager.CreateAgentSession(Path.GetTempPath(), agent, agentSessionId: "ignored-because-no-flag");

        var payload = string.Join(' ', d.ShellArgs);
        payload.Should().NotContain("--session-id");
        payload.Should().NotContain("ignored-because-no-flag");
    }

    [Fact]
    public void CreateAgentSession_Resume_With_EqualsSuffix_Concatenates_Id()
    {
        var manager = new SessionManager(NullLogger<SessionManager>.Instance);
        var agent = new NoScope.CodeScope.Core.Models.AgentProfile
        {
            Id = "copilot", DisplayName = "Copilot CLI", Command = "copilot",
            ResumeArgs = ["--continue"],
            ResumeByIdArgs = ["--resume="],
        };
        var uuid = "0cb916db-26aa-40f2-86b5-1ba81b225fd2";

        var d = manager.CreateAgentSession(Path.GetTempPath(), agent, resume: true, agentSessionId: uuid);

        var payload = string.Join(' ', d.ShellArgs);
        // Must produce "--resume=<id>" (concatenated), NOT "--resume= <id>" (space-separated).
        payload.Should().Contain($"--resume={uuid}");
        payload.Should().NotContain("--continue");
    }
}
