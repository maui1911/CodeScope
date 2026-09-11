---
id: opencode
display_name: OpenCode
command: opencode-cli
headless:
autonomy:
model_flag:
instruction_files: AGENTS.md
verified: never
---

# OpenCode

**Unverified stub. The runner will refuse to dispatch with this
profile** — `headless` is empty, and an empty invocation template is
treated as "this profile has not been filled in" rather than as a
default.

OpenCode is not installed on the machine where the other five profiles
were checked, so every field here would have been a guess. Writing
plausible-looking flags into a contract file is worse than leaving it
blank: the blank fails at dispatch with a clear message, while a wrong
guess fails somewhere inside the agent run, after a worktree has been
spent on it.

Windows ships the binary as `opencode-cli` because `opencode` collides
with a reserved name — `AgentId::from_str` in the product carries that
alias for the same reason. That part is taken from the product registry
rather than guessed.

To fill this in: install the CLI, find its non-interactive invocation
and whatever flag lets tools run unattended, confirm which repo-level
instruction file it reads, and set `verified` to the date you checked.
