using NoScope.CodeScope.App.Updates;

namespace NoScope.CodeScope.App.Tests;

public sealed class VersionInfoTests
{
    [Fact]
    public void Display_StartsWithCapitalV()
    {
        VersionInfo.Display.Should().StartWith("V");
    }

    [Fact]
    public void Display_DoesNotContainBuildMetadata()
    {
        // SourceLink "+commitSha" suffix should be stripped before display.
        VersionInfo.Display.Should().NotContain("+");
    }

    [Fact]
    public void Display_HasNoNestedVPrefix()
    {
        // Strip-then-re-prefix should not leave "Vv" or "VV".
        VersionInfo.Display.Should().NotStartWith("Vv");
        VersionInfo.Display.Should().NotStartWith("VV");
    }
}
