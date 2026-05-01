using NoScope.CodeScope.App.Toasts;
using NoScope.CodeScope.Ui.Services;

namespace NoScope.CodeScope.App.Tests;

/// <summary>
/// Drives <see cref="ToastService.ShowCore"/> directly so tests don't need a WPF
/// dispatcher. Covers the two caps (non-error visible cap + persistent-error hard cap)
/// and the id-dedupe contract that prevents poller-driven retries from stacking.
/// Issue #34.
/// </summary>
public sealed class ToastServiceTests
{
    [Fact]
    public void NonErrorToasts_AreCappedAtThree()
    {
        var svc = new ToastService();

        for (var i = 0; i < 5; i++)
        {
            svc.ShowCore(new ToastRequest(ToastSeverity.Info, $"Info {i}", null));
        }

        svc.Items.Count(i => i.Severity == ToastSeverity.Info).Should().Be(3);
        // Oldest dropped, newest retained — last three indices survive.
        svc.Items.Select(i => i.Title).Should().BeEquivalentTo(["Info 2", "Info 3", "Info 4"]);
    }

    [Fact]
    public void ErrorToasts_AreCappedHardAtTwenty()
    {
        var svc = new ToastService();

        for (var i = 0; i < 25; i++)
        {
            svc.ShowCore(new ToastRequest(ToastSeverity.Err, $"Err {i}", null));
        }

        svc.Items.Count.Should().Be(20);
        // Oldest 5 dropped; 5..24 retained.
        svc.Items.Select(i => i.Title).Should().BeEquivalentTo(
            Enumerable.Range(5, 20).Select(i => $"Err {i}"));
    }

    [Fact]
    public void StableIdReplacesExistingErrorInPlace_SoCapNeverHits()
    {
        var svc = new ToastService();

        for (var i = 0; i < 100; i++)
        {
            svc.ShowCore(new ToastRequest(
                ToastSeverity.Err,
                $"Poll #{i}",
                "gh not found",
                Id: "gh-missing"));
        }

        svc.Items.Should().HaveCount(1);
        svc.Items[0].Title.Should().Be("Poll #99");
    }

    [Fact]
    public void NonErrorAndErrorCaps_AreIndependent()
    {
        var svc = new ToastService();

        // 30 errors → trimmed to 20.
        for (var i = 0; i < 30; i++)
        {
            svc.ShowCore(new ToastRequest(ToastSeverity.Err, $"Err {i}", null));
        }
        // Then 5 infos → trimmed to 3.
        for (var i = 0; i < 5; i++)
        {
            svc.ShowCore(new ToastRequest(ToastSeverity.Info, $"Info {i}", null));
        }

        svc.Items.Count(i => i.Severity == ToastSeverity.Err).Should().Be(20);
        svc.Items.Count(i => i.Severity == ToastSeverity.Info).Should().Be(3);
    }
}
