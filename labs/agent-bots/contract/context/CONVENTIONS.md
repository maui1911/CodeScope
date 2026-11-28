# Conventions

Hard rules. A run that breaks one of these is a failed run even if the
code works and the tests pass.

## Absolute

1. **Never commit to `main`.** Work happens on a branch. The runner
   creates it for you; if you find yourself on `main`, stop and report
   it as a blocker.
2. **Never run `cargo fmt`.** The local rustfmt disagrees with whatever
   formatted this tree and rewrites all of it — even when given a
   single file. There is no CI fmt gate. Hand-format the hunks you
   touch to match their surroundings.
3. **English only.** Code, comments, commit messages, handoffs, PR
   bodies. Regardless of the language of the task description.
4. **No `TODO` / `FIXME` without a linked issue number.**
5. **Never touch the contract plane.** `context/`, `skills/` and
   `bots/*/BOT.md` are changed by humans through a PR, never by a bot
   mid-task.

## Rust style

- Idiomatic Rust 2024 (`edition = "2024"`, needs 1.85+).
- Prefer `?` over `match`/`unwrap` for error propagation. An `unwrap`
  is allowed only for a proven invariant, and it carries a one-line
  `// SAFETY:`-style comment saying why.
- `cargo clippy --workspace --all-targets` clean on files you changed.
- Module names `snake_case`, types `CamelCase`.
- 4-space indent, 100-character line guideline.

## Tests

- For `core/` logic: **write the test first**. Tests live next to the
  code they cover, behind `#[cfg(test)]`.
- UI code in `src/` is not unit-tested; verify it by running the app.

## Scope

- One concern per commit.
- Stay inside the `touches:` globs declared on the task. Touching a
  file outside them fails verification, even if the change is correct
  — propose it in the handoff `next action` instead.
