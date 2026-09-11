---
id: codex
display_name: Codex CLI
command: codex
headless: exec
prompt: stdin
autonomy: -s workspace-write --add-dir {git_dir}
shell: native
model_flag: -m
instruction_files: AGENTS.md
verified: 2026-09-11
---

# Codex CLI

Non-interactive mode is a **subcommand**, not a flag: `codex exec
<prompt>`. That alone is why the runner cannot assume a `-p`-shaped
invocation for every agent — the argv template has to come from the
profile.

The prompt goes on **stdin**, not in argv, and that is not a style
choice. `shell: native` below means the runner launches `codex.cmd`
rather than npm's `sh` shim, and cmd.exe truncates a command line at
8191 characters. A prompt carrying a whole task file goes past that
silently, taking the flags that follow it with it — the first attempt
lost `--add-dir` and half the task, and Codex answered "send me the
issue to fix". `codex exec` reads its instructions from stdin when no
prompt argument is given, which keeps argv short enough that no
launcher can lose it. See F-27.

`-s workspace-write` is the middle sandbox policy; the other two are
`read-only` (cannot commit) and `danger-full-access`. Codex sandboxes
model-generated shell commands itself, which makes it the one agent
here that bounds its own blast radius rather than relying on the
worktree being throwaway.

And that is exactly why its first real run came back blocked. Under
`workspace-write` the writable roots are the working directory, `/tmp`
and `$TMPDIR` — and a linked worktree's git directory is in none of
them. It lives under the *main* repository, so Codex could edit every
file it had been given and could not write `index.lock`. It said so
through `.bot-blocked` and stopped, which is the correct behaviour and
a completely useless outcome.

`--add-dir` is the documented way to widen that, and it is declared
here — the banner confirms the sandbox accepts it. It does **not** make
`.git/worktrees/<leaf>/index.lock` writable in practice, and the run
still ends at "cannot commit". What the flag would grant if it worked
is worth stating anyway, because it is the reason the real answer lies
elsewhere: the whole object store and every ref in the repository. There is no narrower grant: objects and `refs/heads/*` live
in the common directory for a linked worktree, so "let this agent
commit on its own branch" and "let this agent rewrite `main`" are the
same permission. That is not a Codex flaw — it is what a linked
worktree is. The narrower answer is a per-task `git clone --shared`,
where the git directory is inside the sandbox and only a push at the
end crosses the boundary. See F-26.

`{git_dir}` is substituted by the runner, because the path is
per-machine and a contract file is not. It is kept despite not being
sufficient: removing it would make the next person rediscover that the
git directory is the problem rather than reading it here.

It is not enough on Windows. With the git directory granted, the next
thing the run hit was `CreateFileMapping Win32 error 5` — Git Bash's
fork emulation needs shared memory the restricted token denies, so
Codex could not run the verifier it had been told to run before
editing.

Hence `shell: native`. Codex picks the shell it runs commands in by
looking at PATH; it finds Git Bash because Git Bash is there, and Git
Bash is the one thing on this machine that its sandbox kills. Take it
off the PATH and it picks PowerShell, which the sandbox is perfectly
happy with — as is `git.exe`, which is a native Win32 binary and was
never the problem. The runner does the PATH surgery, because the
runner is the only thing that knows what is on this machine's PATH.

That has a consequence for `verify:`. A verify string is a POSIX
command by convention here, and an agent without a POSIX shell cannot
be told to run one. `cargo test …` is shell-neutral and fine;
`test -f …` is not. So the prompt stops ordering it and says what has
always been true instead: the runner runs the verifier afterwards, in
a clean checkout, and that is the result that counts.

Reads `AGENTS.md`, not `CLAUDE.md`. A bot contract that names its
conventions in the wrong file is invisible to this agent — which makes
`instruction_files` part of the contract rather than a detail.

`codex exec` also takes `-C/--cd`, so the runner could point it at the
worktree instead of relying on the subshell. Left alone for now: one
mechanism for every agent is easier to reason about than a per-agent
special case.

Flags verified 2026-09-11 against `codex exec --help` on this machine,
and exercised by a real run the same day — which is how the sandbox
problem above was found.
