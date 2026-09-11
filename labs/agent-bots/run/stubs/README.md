# Stub agents

Fake agents that stand in for `claude` so the runner's verdict logic
can be exercised without spending tokens, and — more to the point —
without waiting for a real agent to misbehave.

Each one is a failure mode the runner used to get wrong. They are the
regression tests for F-4, F-8 and F-25.

| Stub | Simulates | Expected verdict |
|---|---|---|
| `blocked.sh` | an agent that refuses through the contract channel: writes `.bot-blocked`, exits 0 | `blocked`, reason quoted in the handoff, worktree kept |
| `crash.sh` | an agent that dies silently: exits 3, writes nothing | `blocked`, "agent exited 3 without reporting", worktree kept |
| `liar.sh` | an agent that **claims success and is wrong**: commits a deliberately failing test, prints a confident summary, exits 0 | `blocked`, verifier exit 101, worktree kept |
| `sloppy.sh` | an agent that does the job correctly and leaves an unlinked `TODO` behind | `blocked`, the offending added line quoted |
| `meddler.sh` | not an agent — a **verifier** that passes while committing into the worktree it was judging | `blocked`, "verifier changed the tree it was measuring" |
| `fabulist.sh` | a reviewer that writes a confident, well-formed review citing a file this repo has never had | `blocked`, the invented path quoted |
| `stumped.sh` | a reviewer that completes the paperwork and reports `verdict: blocked` — its own escalation | `blocked`, pointing at the review's blind-spots section |
| `critic.sh` | a reviewer whose findings are well-formed and real: the positive case for the bot-to-bot handoff | `done`, a task derived for the next bot and the handoff addressed to it |
| `refusenik.sh` | an agent that answers with an **empty commit** carrying its reasoning | `needs-review`, the commit subject quoted |
| `mover.sh` | an agent that does its work and **moves the base ref out from under itself** while doing it | one of four, by configuration: `done` after a clean rebase, or `needs-review` for a conflict, a verifier that is now red, or a branch the new base has emptied |

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

`critic.sh` and `fabulist.sh` are a pair, both for review tasks:
fabulist is the negative case (a confident review citing a file that
does not exist), critic the positive one (everything checks out, so the
handoff is derived). Point critic at
`examples/T-0006-review-telemetry.md`, and add `--chain` to watch the
derived task dispatch itself.

One thing to know when chaining with a stub: the child inherits
`BOT_AGENT_CMD`, so both halves of the chain run the same stub. That is
how the harvest bug in F-21 was found — a fixer behaving like a
reviewer — but it does mean a stubbed chain is not two different
bots.

`mover.sh` is the only one driven entirely by the environment, because
its four outcomes differ in exactly one thing — what lands on the base
while the bot is working. Same path with different content conflicts;
same path with the same content empties the branch; different paths
rebase cleanly, and whether that is a `done` is then up to the verifier.
`sweep.sh` runs all four against a throwaway `bot-sweep/base` ref, and
they are the only cases that check the board event as well as the exit
code: three of the four are `needs-review` for three different reasons,
and a runner that reported the wrong one would still score 2.

`liar.sh` appends to `core/src/telemetry.rs`, so point it at a task
whose `touches:` covers that file, or it will trip the scope check
first and prove the wrong thing. A blocked run keeps its worktree by
design; remove it and the branch before re-running.
