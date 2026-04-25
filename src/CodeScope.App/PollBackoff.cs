namespace NoScope.CodeScope.App;

/// <summary>
/// Per-item poll state with exponential back-off. Used by <see cref="WorktreeStatusPoller"/>
/// and <see cref="PullRequestStatusPoller"/> to avoid re-querying items whose results
/// haven't changed; any observed change resets the skip counter.
/// <para>
/// Back-off sequence: 0 → 1 → 3 → 7 → … → <c>maxSkipTicks</c> (clamped). Mutation happens
/// on a single poller thread, so no locking. Equality uses <see cref="object.Equals(object?, object?)"/>.
/// </para>
/// </summary>
public sealed class PollBackoff<T> where T : class
{
    public T? Last;
    public int TicksUntilNextPoll;

    /// <summary>
    /// Updates the back-off state for the latest observation. If the value equals the
    /// previously-observed one, grows the skip count (cap <paramref name="maxSkipTicks"/>);
    /// otherwise records the new value and resets the skip count to zero.
    /// </summary>
    public void Observe(T? value, int maxSkipTicks)
    {
        if (Equals(value, Last))
        {
            TicksUntilNextPoll = TicksUntilNextPoll switch
            {
                0 => 1,
                var s when s * 2 + 1 > maxSkipTicks => maxSkipTicks,
                var s => s * 2 + 1,
            };
        }
        else
        {
            Last = value;
            TicksUntilNextPoll = 0;
        }
    }
}
