using NoScope.CodeScope.App.Polling;

namespace NoScope.CodeScope.App.Tests;

public sealed class PollBackoffTests
{
    [Fact]
    public void FirstObservation_RecordsValueAndKeepsSkipZero()
    {
        var bo = new PollBackoff<string>();

        bo.Observe("a", maxSkipTicks: 9);

        bo.Last.Should().Be("a");
        bo.TicksUntilNextPoll.Should().Be(0);
    }

    [Fact]
    public void RepeatedSameValue_GrowsBackoffViaDoublePlusOne()
    {
        var bo = new PollBackoff<string>();

        bo.Observe("x", 100);
        bo.TicksUntilNextPoll.Should().Be(0);

        bo.Observe("x", 100);
        bo.TicksUntilNextPoll.Should().Be(1);   // 0 → 1

        bo.Observe("x", 100);
        bo.TicksUntilNextPoll.Should().Be(3);   // 1 → 3

        bo.Observe("x", 100);
        bo.TicksUntilNextPoll.Should().Be(7);   // 3 → 7

        bo.Observe("x", 100);
        bo.TicksUntilNextPoll.Should().Be(15);  // 7 → 15
    }

    [Fact]
    public void Backoff_ClampsToMaxSkipTicks()
    {
        var bo = new PollBackoff<string>();
        bo.Observe("x", 9);  // initial
        for (var i = 0; i < 10; i++) { bo.Observe("x", 9); }

        bo.TicksUntilNextPoll.Should().Be(9);
    }

    [Fact]
    public void ChangedValue_RecordsAndResetsSkip()
    {
        var bo = new PollBackoff<string>();
        bo.Observe("a", 100);
        bo.Observe("a", 100);
        bo.Observe("a", 100);
        bo.TicksUntilNextPoll.Should().BeGreaterThan(0);

        bo.Observe("b", 100);

        bo.Last.Should().Be("b");
        bo.TicksUntilNextPoll.Should().Be(0);
    }

    [Fact]
    public void NullObservation_EqualsInitialNullState_AndImmediatelyBacksOff()
    {
        // Initial Last is null. Observe(null, _) sees Equals(null, null) ⇒ back-off branch
        // fires on the first call, bumping TicksUntilNextPoll from 0 to 1.
        var bo = new PollBackoff<string>();

        bo.Observe(null, 100);

        bo.Last.Should().BeNull();
        bo.TicksUntilNextPoll.Should().Be(1);
    }

    [Fact]
    public void NullAfterValue_ResetsBackoff()
    {
        var bo = new PollBackoff<string>();
        bo.Observe("a", 100);
        bo.Observe("a", 100);
        bo.TicksUntilNextPoll.Should().Be(1);

        bo.Observe(null, 100);

        bo.Last.Should().BeNull();
        bo.TicksUntilNextPoll.Should().Be(0);
    }

    [Fact]
    public void MaxSkipTicksZero_KeepsBackoffZero()
    {
        var bo = new PollBackoff<string>();
        bo.Observe("x", 0);
        bo.Observe("x", 0);
        bo.Observe("x", 0);

        // 0 → switch picks first arm (0 => 1), but next iter:
        //   1 * 2 + 1 = 3 > maxSkipTicks(0) → clamps to 0.
        bo.TicksUntilNextPoll.Should().Be(0);
    }
}
