---
id: copilot
display_name: GitHub Copilot CLI
command: copilot
headless: -p {prompt}
autonomy: --allow-all-tools
model_flag:
instruction_files: AGENTS.md
verified: 2026-09-11
---

# GitHub Copilot CLI

`-p/--prompt` for non-interactive scripting.

`--allow-all-tools` lets tools run without prompting. There is a wider
`--allow-all` (tools plus paths plus URLs); the narrower flag is the
right default, since a bot working in a worktree has no business
reaching arbitrary URLs.

`model_flag` is deliberately empty — no model selector was found on
this CLI. An empty value means the runner will not pass one, which is
correct behaviour rather than a gap to paper over. Revisit if the CLI
grows one.

Flags verified 2026-09-11 against `copilot --help` on this machine. Not
yet exercised by a real run.
