---
id: pi
display_name: Pi
command: pi
headless: --print {prompt}
autonomy:
model_flag: --model
instruction_files:
verified: 2026-09-11
---

# Pi

`--print` (or `-p`) processes the prompt and exits.

`autonomy` is empty on purpose: pi enables its read, bash, edit and
write tools by default, so there is nothing to unlock. The flags in
this area (`--no-tools`, `--tools <allowlist>`) *restrict* rather than
permit. That is the inverse of every other agent here, and it is a good
argument for the profile carrying an opaque argv fragment rather than a
boolean like `autonomous: true` — the same intent maps to adding a flag
for one CLI and to adding nothing for another.

`--tools` would be the way to hand a specific bot a narrower toolset
than its charter describes in prose. Worth revisiting once bots have
genuinely different roles.

`instruction_files` is empty: no repo-level instruction file was
identified for this CLI. If that is wrong, a bot run under pi silently
misses the project conventions, so it is worth confirming before pi
runs anything real.

Flags verified 2026-09-11 against `pi --help` on this machine. Not yet
exercised by a real run.
