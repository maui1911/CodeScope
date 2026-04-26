using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class EditorGroupViewModelTests
{
    private static SessionTabViewModel MakeTab(string id) =>
        new(new SessionDescriptor { Id = id, WorkingDirectory = @"C:\r", Shell = "pwsh.exe", Title = id }, "p", null, id);

    [Fact]
    public void Id_IsHexGuid_AndUniquePerInstance()
    {
        var a = new EditorGroupViewModel();
        var b = new EditorGroupViewModel();

        a.Id.Should().HaveLength(32);
        a.Id.Should().NotBe(b.Id);
        a.Id.Should().MatchRegex("^[0-9a-f]{32}$");
    }

    [Fact]
    public void Tabs_DefaultsToEmptyCollection()
    {
        new EditorGroupViewModel().Tabs.Should().BeEmpty();
    }

    [Fact]
    public void Tabs_HonoursInjectedCollection()
    {
        var shared = new System.Collections.ObjectModel.ObservableCollection<SessionTabViewModel> { MakeTab("a") };
        var g = new EditorGroupViewModel(shared);

        g.Tabs.Should().BeSameAs(shared);
        g.Tabs.Should().HaveCount(1);
    }

    [Fact]
    public void IsFocused_DefaultsToFalse()
    {
        new EditorGroupViewModel().IsFocused.Should().BeFalse();
    }

    [Fact]
    public void SelectedTab_FlipsIsActiveOnNewTab()
    {
        var g = new EditorGroupViewModel();
        var tab = MakeTab("a");

        g.SelectedTab = tab;

        tab.IsActive.Should().BeTrue();
    }

    [Fact]
    public void SelectedTab_ClearsIsActiveOnPreviousTab()
    {
        var g = new EditorGroupViewModel();
        var first = MakeTab("a");
        var second = MakeTab("b");

        g.SelectedTab = first;
        first.IsActive.Should().BeTrue();

        g.SelectedTab = second;

        first.IsActive.Should().BeFalse();
        second.IsActive.Should().BeTrue();
    }

    [Fact]
    public void SelectedTab_NullClearsActiveFlagOnLastTab()
    {
        var g = new EditorGroupViewModel();
        var tab = MakeTab("a");

        g.SelectedTab = tab;
        g.SelectedTab = null;

        tab.IsActive.Should().BeFalse();
    }
}
