using NoScope.CodeScope.Core.Services;
using NoScope.CodeScope.Ui.ViewModels;

namespace NoScope.CodeScope.Ui.Tests;

public sealed class NotificationsViewModelTests
{
    private static NotificationEntry MakeEntry(string id, bool isRead = false) =>
        new(id, "sess-" + id, "Tab " + id, NotificationKind.SessionReady, "Title " + id, "Detail", DateTimeOffset.UtcNow, isRead);

    [Fact]
    public void Construction_HydratesFromService()
    {
        var svc = Substitute.For<INotificationService>();
        svc.Entries.Returns(new[] { MakeEntry("1"), MakeEntry("2", isRead: true) });
        svc.UnreadCount.Returns(1);

        var vm = new NotificationsViewModel(svc);

        vm.Entries.Should().HaveCount(2);
        vm.UnreadCount.Should().Be(1);
        vm.HasAny.Should().BeTrue();
        vm.HasUnread.Should().BeTrue();
        vm.IsOpen.Should().BeFalse();
    }

    [Fact]
    public void HasAny_FalseWhenServiceEmpty()
    {
        var svc = Substitute.For<INotificationService>();
        svc.Entries.Returns(Array.Empty<NotificationEntry>());

        new NotificationsViewModel(svc).HasAny.Should().BeFalse();
    }

    [Fact]
    public void Toggle_FlipsIsOpen()
    {
        var svc = Substitute.For<INotificationService>();
        svc.Entries.Returns(Array.Empty<NotificationEntry>());
        var vm = new NotificationsViewModel(svc);

        vm.ToggleCommand.Execute(null);
        vm.IsOpen.Should().BeTrue();
        vm.ToggleCommand.Execute(null);
        vm.IsOpen.Should().BeFalse();
    }

    [Fact]
    public void Open_SetsIsOpenAndMarksAllRead()
    {
        var svc = Substitute.For<INotificationService>();
        svc.Entries.Returns(Array.Empty<NotificationEntry>());
        var vm = new NotificationsViewModel(svc);

        vm.OpenCommand.Execute(null);

        vm.IsOpen.Should().BeTrue();
        svc.Received(1).MarkAllRead();
    }

    [Fact]
    public void ClearAll_DelegatesToService()
    {
        var svc = Substitute.For<INotificationService>();
        svc.Entries.Returns(Array.Empty<NotificationEntry>());
        var vm = new NotificationsViewModel(svc);

        vm.ClearAllCommand.Execute(null);

        svc.Received(1).Clear();
    }

    [Fact]
    public void Activate_MarksReadAndRaisesActivateRequested_AndClosesPopover()
    {
        var svc = Substitute.For<INotificationService>();
        var entry = MakeEntry("42");
        svc.Entries.Returns(new[] { entry });
        var vm = new NotificationsViewModel(svc) { IsOpen = true };
        NotificationEntry? captured = null;
        vm.ActivateRequested += (_, e) => captured = e;

        vm.ActivateCommand.Execute(entry);

        svc.Received(1).MarkRead("42");
        captured.Should().BeSameAs(entry);
        vm.IsOpen.Should().BeFalse();
    }

    [Fact]
    public void Activate_NullEntry_IsNoop()
    {
        var svc = Substitute.For<INotificationService>();
        svc.Entries.Returns(Array.Empty<NotificationEntry>());
        var vm = new NotificationsViewModel(svc) { IsOpen = true };

        vm.ActivateCommand.Execute(null);

        svc.DidNotReceive().MarkRead(Arg.Any<string>());
        vm.IsOpen.Should().BeTrue();  // unchanged
    }

    [Fact]
    public void HasUnread_TracksUnreadCountChange()
    {
        var svc = Substitute.For<INotificationService>();
        svc.Entries.Returns(Array.Empty<NotificationEntry>());
        svc.UnreadCount.Returns(0);
        var vm = new NotificationsViewModel(svc);
        vm.HasUnread.Should().BeFalse();

        vm.UnreadCount = 3;

        vm.HasUnread.Should().BeTrue();
    }
}
