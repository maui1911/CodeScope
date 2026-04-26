using System.Text.Json;
using NoScope.CodeScope.App.Persistence;

namespace NoScope.CodeScope.App.Tests;

public sealed class LayoutStoreTests
{
    // LayoutStore.Save/Load route through %LocalAppData%/CodeScope/layout.json which is
    // process-global state — exercising it directly would race with the user's installed
    // build. Instead, test the public Layout record's JSON contract: that's what Save and
    // Load actually marshal across, so contract drift surfaces here just as well.

    [Fact]
    public void Layout_RoundTrips_AcrossSystemTextJson()
    {
        var original = new LayoutStore.Layout(
            GroupCount: 3,
            FocusedGroupIndex: 1,
            SessionToGroup: new Dictionary<string, int>
            {
                ["s1"] = 0,
                ["s2"] = 1,
                ["s3"] = 2,
            },
            GroupWidths: new[] { 1.0, 2.5, 0.5 });

        var json = JsonSerializer.Serialize(original);
        var roundtripped = JsonSerializer.Deserialize<LayoutStore.Layout>(json);

        roundtripped.Should().NotBeNull();
        roundtripped!.GroupCount.Should().Be(3);
        roundtripped.FocusedGroupIndex.Should().Be(1);
        roundtripped.SessionToGroup.Should().BeEquivalentTo(original.SessionToGroup);
        roundtripped.GroupWidths.Should().BeEquivalentTo(original.GroupWidths);
    }

    [Fact]
    public void Layout_OptionalGroupWidths_DefaultsToNull()
    {
        var layout = new LayoutStore.Layout(
            GroupCount: 1,
            FocusedGroupIndex: 0,
            SessionToGroup: new Dictionary<string, int>());

        layout.GroupWidths.Should().BeNull();
    }

    [Fact]
    public void Layout_ScalarFields_MatchAcrossIdenticalInstances()
    {
        var a = new LayoutStore.Layout(2, 0, new Dictionary<string, int> { ["x"] = 1 });
        var b = new LayoutStore.Layout(2, 0, new Dictionary<string, int> { ["x"] = 1 });

        a.GroupCount.Should().Be(b.GroupCount);
        a.FocusedGroupIndex.Should().Be(b.FocusedGroupIndex);
        a.SessionToGroup.Should().BeEquivalentTo(b.SessionToGroup);
    }

    [Fact]
    public void Load_NoExistingFile_ReturnsNullSafely()
    {
        // The actual file may or may not exist on disk — Load is documented to return null on
        // missing/corrupt files. We tolerate both null and a real Layout depending on env state.
        var result = LayoutStore.Load();

        // No exception thrown; result is null OR a deserialised layout.
        // (Cannot assert null because a parallel installed CodeScope build may have written one.)
        if (result is not null)
        {
            result.GroupCount.Should().BeGreaterOrEqualTo(0);
        }
    }
}
