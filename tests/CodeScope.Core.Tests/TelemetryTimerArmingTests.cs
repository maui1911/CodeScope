using NoScope.CodeScope.Core.Services;
using Microsoft.Extensions.Logging.Abstractions;

namespace NoScope.CodeScope.Core.Tests;

/// <summary>
/// Verifies the four telemetry services pause their poll timer when no watches are
/// registered and re-arm it on the first <c>Register</c>. Without this, an idle
/// CodeScope (no agent sessions) burns ~8 timer callbacks/sec across the four
/// services for nothing. Issue #36.
/// </summary>
public sealed class TelemetryTimerArmingTests : IDisposable
{
    private readonly string _root = Path.Combine(Path.GetTempPath(), "cs-tel-arm-" + Guid.NewGuid().ToString("n"));

    public void Dispose()
    {
        try { Directory.Delete(_root, recursive: true); } catch { /* best effort */ }
    }

    [Fact]
    public void Claude_Timer_Disarmed_Until_First_Register()
    {
        using var svc = new ClaudeTelemetryService(NullLogger<ClaudeTelemetryService>.Instance, _root, enablePolling: true);
        svc.IsPollTimerArmedForTest.Should().BeFalse();

        svc.Register("a", @"C:\dev\a");
        svc.IsPollTimerArmedForTest.Should().BeTrue();

        svc.Register("b", @"C:\dev\b");
        svc.IsPollTimerArmedForTest.Should().BeTrue();

        svc.Unregister("a");
        svc.IsPollTimerArmedForTest.Should().BeTrue();

        svc.Unregister("b");
        svc.IsPollTimerArmedForTest.Should().BeFalse();
    }

    [Fact]
    public void Pi_Timer_Disarmed_Until_First_Register()
    {
        using var svc = new PiTelemetryService(NullLogger<PiTelemetryService>.Instance, _root, enablePolling: true);
        svc.IsPollTimerArmedForTest.Should().BeFalse();

        svc.Register("a", @"C:\dev\a");
        svc.IsPollTimerArmedForTest.Should().BeTrue();

        svc.Unregister("a");
        svc.IsPollTimerArmedForTest.Should().BeFalse();
    }

    [Fact]
    public void Copilot_Timer_Disarmed_Until_First_Register()
    {
        using var svc = new CopilotTelemetryService(NullLogger<CopilotTelemetryService>.Instance, _root, enablePolling: true);
        svc.IsPollTimerArmedForTest.Should().BeFalse();

        svc.Register("a", @"C:\dev\a");
        svc.IsPollTimerArmedForTest.Should().BeTrue();

        svc.Unregister("a");
        svc.IsPollTimerArmedForTest.Should().BeFalse();
    }

    [Fact]
    public void OpenCode_Timer_Disarmed_Until_First_Register()
    {
        using var svc = new OpenCodeTelemetryService(NullLogger<OpenCodeTelemetryService>.Instance, _root, enablePolling: true);
        svc.IsPollTimerArmedForTest.Should().BeFalse();

        svc.Register("a", @"C:\dev\a");
        svc.IsPollTimerArmedForTest.Should().BeTrue();

        svc.Unregister("a");
        svc.IsPollTimerArmedForTest.Should().BeFalse();
    }

    [Fact]
    public void Disabled_Polling_Stays_Disarmed_Forever()
    {
        // Test-seam constructor (enablePolling=false) — no timer at all, so the arming
        // flag stays false through Register/Unregister. Existing telemetry tests rely on this.
        using var svc = new ClaudeTelemetryService(NullLogger<ClaudeTelemetryService>.Instance, _root);
        svc.IsPollTimerArmedForTest.Should().BeFalse();
        svc.Register("a", @"C:\dev\a");
        svc.IsPollTimerArmedForTest.Should().BeFalse();
    }
}
