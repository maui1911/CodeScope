using NoScope.CodeScope.Core;
using NoScope.CodeScope.Core.Models;
using NoScope.CodeScope.Core.Services;
using FluentAssertions;
using Microsoft.Extensions.Logging.Abstractions;
using Xunit;

namespace NoScope.CodeScope.Core.Tests;

public sealed class ProjectStoreTests : IDisposable
{
    private readonly string _tempDir;
    private readonly string _configPath;
    private readonly ProjectStore _store;

    public ProjectStoreTests()
    {
        _tempDir = Path.Combine(Path.GetTempPath(), "codescope-tests-" + Guid.NewGuid().ToString("n"));
        Directory.CreateDirectory(_tempDir);
        _configPath = Path.Combine(_tempDir, "projects.json");
        _store = new ProjectStore(NullLogger<ProjectStore>.Instance, _configPath);
    }

    public void Dispose()
    {
        try
        {
            Directory.Delete(_tempDir, recursive: true);
        }
        catch
        {
            // Best effort.
        }
    }

    [Fact]
    public async Task Load_Returns_Empty_Config_When_File_Missing()
    {
        var result = await _store.LoadAsync();

        result.IsSuccess.Should().BeTrue();
        result.Value.Version.Should().Be(ProjectsConfig.CurrentVersion);
        result.Value.Agents.Should().BeEmpty();
        result.Value.Projects.Should().BeEmpty();
    }

    [Fact]
    public async Task Save_Then_Load_Roundtrips_Config()
    {
        var original = new ProjectsConfig
        {
            Version = ProjectsConfig.CurrentVersion,
            Agents =
            [
                new AgentProfile
                {
                    Id = "claude",
                    DisplayName = "Claude Code",
                    Command = "claude",
                    ResumeArgs = ["--continue"],
                    NewSessionArgs = [],
                    IsDefault = true,
                },
            ],
            Projects =
            [
                new Project
                {
                    Id = "demo",
                    Name = "Demo",
                    Path = @"C:\demo",
                    DefaultBranch = "main",
                    WorktreeRoot = @"C:\demo.worktrees",
                    Worktrees =
                    [
                        new Worktree { Id = "primary", Path = @"C:\demo", IsPrimary = true },
                    ],
                    Sessions =
                    [
                        new Session
                        {
                            Id = "feat-x",
                            WorktreePath = @"C:\demo.worktrees\feat-x",
                            Branch = "feat/x",
                            AgentId = "claude",
                            LastOpened = DateTimeOffset.UtcNow,
                        },
                    ],
                },
            ],
        };

        var saved = await _store.SaveAsync(original);
        saved.IsSuccess.Should().BeTrue();

        var loaded = await _store.LoadAsync();
        loaded.IsSuccess.Should().BeTrue();
        loaded.Value.Should().BeEquivalentTo(original, o => o.ComparingByMembers<ProjectsConfig>());
    }

    [Fact]
    public async Task Load_Returns_Failure_On_Invalid_Json()
    {
        await File.WriteAllTextAsync(_configPath, "{ not valid json");

        var result = await _store.LoadAsync();

        result.IsFailure.Should().BeTrue();
        result.Error.Should().Contain("Invalid JSON");
    }

    [Fact]
    public async Task Save_Is_Atomic_Via_Tmp_File()
    {
        await _store.SaveAsync(new ProjectsConfig());
        File.Exists(_configPath).Should().BeTrue();
        File.Exists(_configPath + ".tmp").Should().BeFalse("temp file should have been renamed");
    }

    [Fact]
    public async Task Save_Always_Writes_CurrentVersion()
    {
        var stale = new ProjectsConfig { Version = 0 };

        await _store.SaveAsync(stale);

        var loaded = await _store.LoadAsync();
        loaded.Value.Version.Should().Be(ProjectsConfig.CurrentVersion);
    }

    [Fact]
    public async Task Load_Preserves_Future_Version_Without_Crashing()
    {
        // Forward compat: if a newer CodeScope wrote a version we don't know, we should load and
        // leave the data intact rather than corrupting it.
        var json = """{ "version": 999, "agents": [], "projects": [] }""";
        await File.WriteAllTextAsync(_configPath, json);

        var result = await _store.LoadAsync();

        result.IsSuccess.Should().BeTrue();
        result.Value.Version.Should().Be(999);
    }

    [Fact]
    public async Task Session_DisplayName_Roundtrips_Through_Config()
    {
        var original = new ProjectsConfig
        {
            Projects =
            [
                new Project
                {
                    Id = "p",
                    Name = "P",
                    Path = @"C:\p",
                    Sessions =
                    [
                        new Session
                        {
                            Id = "s",
                            WorktreePath = @"C:\p",
                            DisplayName = "claude · feat-x",
                        },
                    ],
                },
            ],
        };

        await _store.SaveAsync(original);
        var loaded = await _store.LoadAsync();

        loaded.Value.Projects[0].Sessions[0].DisplayName.Should().Be("claude · feat-x");
    }

    [Fact]
    public async Task Migration_Synthesizes_Primary_Worktree_When_None_Present()
    {
        var original = new ProjectsConfig
        {
            Projects = [new Project { Id = "p", Name = "P", Path = @"C:\p" }],
        };
        await _store.SaveAsync(original);

        var loaded = await _store.LoadAsync();

        var wts = loaded.Value.Projects.Single().Worktrees;
        wts.Should().ContainSingle();
        wts[0].IsPrimary.Should().BeTrue();
        wts[0].Path.Should().Be(@"C:\p");
        wts[0].Id.Should().Be("primary");
    }

    [Fact]
    public async Task Migration_Leaves_Existing_Worktrees_Untouched()
    {
        var original = new ProjectsConfig
        {
            Projects =
            [
                new Project
                {
                    Id = "p",
                    Name = "P",
                    Path = @"C:\p",
                    Worktrees =
                    [
                        new Worktree { Id = "primary", Path = @"C:\p", IsPrimary = true, Branch = "main" },
                        new Worktree { Id = "feat-x",  Path = @"C:\p.wt\feat-x", Branch = "feat/x" },
                    ],
                },
            ],
        };
        await _store.SaveAsync(original);

        var loaded = await _store.LoadAsync();

        loaded.Value.Projects.Single().Worktrees.Should().HaveCount(2);
    }

    [Fact]
    public async Task Load_Migrates_Legacy_OpenTabs_To_Unsorted()
    {
        const string legacyJson = """
        {
          "version": 1,
          "agents": [],
          "projects": [
            {
              "id": "open-tabs",
              "name": "Open tabs",
              "path": "",
              "defaultBranch": "main",
              "sessions": [
                { "id": "t1", "worktreePath": "C:\\tmp" }
              ]
            }
          ]
        }
        """;
        await File.WriteAllTextAsync(_configPath, legacyJson);

        var result = await _store.LoadAsync();

        result.IsSuccess.Should().BeTrue();
        var project = result.Value.Projects.Single();
        project.Id.Should().Be("unsorted");
        project.Name.Should().Be("Unsorted");
        project.Sessions.Should().ContainSingle(s => s.Id == "t1");
    }
}
