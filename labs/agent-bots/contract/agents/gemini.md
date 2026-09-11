---
id: gemini
display_name: Gemini CLI
command: gemini
headless: -p {prompt}
autonomy: --approval-mode auto_edit
model_flag: -m
instruction_files: GEMINI.md
verified: 2026-09-11
---

# Gemini CLI

`-p/--prompt` runs headless, same shape as Claude Code.

`--approval-mode auto_edit` rather than `yolo`. The four modes are
`default`, `auto_edit`, `yolo` and `plan`; `yolo` auto-approves every
tool, which is more than an unattended lint fix needs. If a task turns
out to need shell execution that `auto_edit` withholds, that is a
per-task escalation, not a reason to loosen the default.

Reads `GEMINI.md`. Third agent, third filename — see `codex.md`.

Flags verified 2026-09-11 against `gemini --help` on this machine. Not
yet exercised by a real run.
