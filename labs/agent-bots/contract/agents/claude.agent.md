---
id: claude
display_name: Claude Code
command: claude
headless: -p {prompt}
autonomy: --permission-mode auto
model_flag: --model
instruction_files: CLAUDE.md
verified: 2026-09-11
---

# Claude Code

`-p` is the headless flag; the prompt is its value.

`--permission-mode auto` and not `acceptEdits`: the runner asks the
agent to execute the verifier and to commit, both of which are Bash
calls. `acceptEdits` covers file edits only, so an unattended run under
it cannot finish — see F-4.

Loads `CLAUDE.md` from the repo root and `~/.claude/CLAUDE.md` from the
host. The second one is outside the contract's control (F-7).

Verified 2026-09-11 against the CLI on this machine, and exercised end
to end by the T-0001 and T-0002 runs.
