using NoScope.CodeScope.Core.Services;
using FluentAssertions;
using Xunit;

namespace NoScope.CodeScope.Core.Tests;

public sealed class NotificationServiceTests
{
    private static NotificationEntry Entry(string id, string? sessionId = "s1", bool read = false) =>
        new(id, sessionId, "session-" + (sessionId ?? ""), NotificationKind.SessionReady,
            "Ready", "agent turn complete", DateTimeOffset.UtcNow, read);

    [Fact]
    public void Push_Adds_Newest_First()
    {
        var sut = new NotificationService();
        sut.Push(Entry("a"));
        sut.Push(Entry("b"));

        sut.Entries.Select(e => e.Id).Should().Equal("b", "a");
    }

    [Fact]
    public void Push_Trims_Beyond_MaxEntries()
    {
        var sut = new NotificationService(maxEntries: 3);
        for (var i = 0; i < 5; i++) { sut.Push(Entry($"n{i}")); }

        sut.Entries.Should().HaveCount(3);
        sut.Entries.Select(e => e.Id).Should().Equal("n4", "n3", "n2");
    }

    [Fact]
    public void UnreadCount_Reflects_Pushed_Entries()
    {
        var sut = new NotificationService();
        sut.Push(Entry("a"));
        sut.Push(Entry("b", read: true));
        sut.Push(Entry("c"));

        sut.UnreadCount.Should().Be(2);
    }

    [Fact]
    public void MarkAllRead_Clears_Unread_Flag()
    {
        var sut = new NotificationService();
        sut.Push(Entry("a"));
        sut.Push(Entry("b"));

        sut.MarkAllRead();

        sut.UnreadCount.Should().Be(0);
        sut.Entries.Should().OnlyContain(e => e.IsRead);
    }

    [Fact]
    public void MarkSessionRead_Only_Affects_Matching_Session()
    {
        var sut = new NotificationService();
        sut.Push(Entry("a", sessionId: "s1"));
        sut.Push(Entry("b", sessionId: "s2"));
        sut.Push(Entry("c", sessionId: "s1"));

        sut.MarkSessionRead("s1");

        sut.Entries.Single(e => e.Id == "a").IsRead.Should().BeTrue();
        sut.Entries.Single(e => e.Id == "b").IsRead.Should().BeFalse();
        sut.Entries.Single(e => e.Id == "c").IsRead.Should().BeTrue();
    }

    [Fact]
    public void MarkRead_Only_Affects_Single_Entry()
    {
        var sut = new NotificationService();
        sut.Push(Entry("a"));
        sut.Push(Entry("b"));

        sut.MarkRead("a");

        sut.Entries.Single(e => e.Id == "a").IsRead.Should().BeTrue();
        sut.Entries.Single(e => e.Id == "b").IsRead.Should().BeFalse();
    }

    [Fact]
    public void Clear_Empties_The_Buffer()
    {
        var sut = new NotificationService();
        sut.Push(Entry("a"));
        sut.Push(Entry("b"));

        sut.Clear();

        sut.Entries.Should().BeEmpty();
        sut.UnreadCount.Should().Be(0);
    }

    [Fact]
    public void Changed_Fires_On_Push_And_Mutations()
    {
        var sut = new NotificationService();
        var count = 0;
        sut.Changed += (_, _) => count++;

        sut.Push(Entry("a"));
        sut.MarkRead("a");
        sut.Push(Entry("b"));
        sut.MarkAllRead();
        sut.Clear();

        count.Should().Be(5);
    }

    [Fact]
    public void MarkRead_Noop_When_Already_Read()
    {
        var sut = new NotificationService();
        sut.Push(Entry("a", read: true));
        var count = 0;
        sut.Changed += (_, _) => count++;

        sut.MarkRead("a");
        sut.MarkAllRead();

        count.Should().Be(0);
    }
}
