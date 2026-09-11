# Stub agents

Fake agents that stand in for `claude` so the runner's verdict logic
can be exercised without spending tokens, and — more to the point —
without waiting for a real agent to misbehave.

Each one is a failure mode the runner used to get wrong. They are the
regression tests for F-4 and F-8.

| Stub | Simulates | Expected verdict |
|---|---|---|
| `blocked.sh` | an agent that refuses through the contract channel: writes `.bot-blocked`, exits 0 | `blocked`, reason quoted in the handoff, worktree kept |
| `crash.sh` | an agent that dies silently: exits 3, writes nothing | `blocked`, "agent exited 3 without reporting", worktree kept |
| `liar.sh` | an agent that **claims success and is wrong**: commits a deliberately failing test, prints a confident summary, exits 0 | `blocked`, verifier exit 101, worktree kept |
| `meddler.sh` | not an agent — a **verifier** that passes while committing into the worktree it was judging | `blocked`, "verifier changed the tree it was measuring" |

Before F-4 was fixed, the first two both landed on `done` as a no-op,
and the cleanup path then deleted the branch — the evidence went with
it. `liar.sh` is the one that matters most: it is the standing proof
that the verdict comes from the tree and not from the claim.

## Running one

```bash
BOT_AGENT_CMD=labs/agent-bots/run/stubs/liar.sh BOT_AGENT_ARGS="" \
  labs/agent-bots/run/bot-run.sh --task <task> --reset
```

`BOT_AGENT_ARGS=""` matters, and not because of what it contains: the
variable being *set at all* is the signal that a stub is standing in
for the profile's argv. Leave it out and the stub is invoked with the
real agent's flags.

`meddler.sh` is the odd one out — it is a verifier rather than an
agent, so it is named in a task's `verify:` instead of being injected
through `BOT_AGENT_CMD`. `examples/T-0003-meddling-verifier.md` wires
it up; run that one with `--skip-agent`.

`liar.sh` appends to `core/src/telemetry.rs`, so point it at a task
whose `touches:` covers that file, or it will trip the scope check
first and prove the wrong thing. A blocked run keeps its worktree by
design; remove it and the branch before re-running.
