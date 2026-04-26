using NoScope.CodeScope.Ui;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class AutomationIdsTests
{
    [Fact]
    public void SafeToken_NullEmptyOrWhitespace_ReturnsUnknown()
    {
        AutomationIds.SafeToken(null).Should().Be("unknown");
        AutomationIds.SafeToken("").Should().Be("unknown");
        AutomationIds.SafeToken("   ").Should().Be("unknown");
    }

    [Fact]
    public void SafeToken_AllAlphaNumeric_PreservesInput()
    {
        AutomationIds.SafeToken("Project1").Should().Be("Project1");
        AutomationIds.SafeToken("CodeScope").Should().Be("CodeScope");
    }

    [Fact]
    public void SafeToken_ReplacesNonAlphaNumeric_WithUnderscores()
    {
        AutomationIds.SafeToken("hello world").Should().Be("hello_world");
        AutomationIds.SafeToken("feature/branch-name").Should().Be("feature_branch_name");
        AutomationIds.SafeToken("a.b.c").Should().Be("a_b_c");
    }

    [Fact]
    public void SafeToken_TrimsLeadingAndTrailingUnderscores()
    {
        AutomationIds.SafeToken(" prefix").Should().Be("prefix");
        AutomationIds.SafeToken("suffix ").Should().Be("suffix");
        AutomationIds.SafeToken("  /a/  ").Should().Be("a");
    }

    [Fact]
    public void SafeToken_OnlyPunctuation_FallsBackToUnknown()
    {
        AutomationIds.SafeToken("///").Should().Be("unknown");
        AutomationIds.SafeToken("--- ").Should().Be("unknown");
    }

    [Fact]
    public void SafeToken_KeepsUnicodeLettersAndDigits()
    {
        // char.IsLetterOrDigit is Unicode-aware.
        AutomationIds.SafeToken("café_42").Should().Be("café_42");
    }
}
