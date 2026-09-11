# Skill: clippy-sweep

## Purpose

Make `cargo clippy` clean for a named set of files, without changing
behaviour and without reformatting anything.

## Inputs

- `touches:` — the files in scope. Nothing outside them may change.
- `verify:` — the clippy invocation that must exit 0.

## Workflow

1. Run the `verify:` command first, before editing anything. Capture
   the output.
   - Exits 0 already? Stop, change nothing, commit nothing. A no-op run
     is a success, and the runner will record it as one.
   - Fails for a reason unrelated to the files in `touches:`? Stop and
     escalate through `.bot-blocked` — it was broken before you
     arrived. See the escalation section of your charter.
2. Group the remaining warnings by lint name. Fix them one lint at a
   time, smallest change first.
3. Re-run the verifier after each group, not once at the end. A lint
   fix that breaks a test is easier to find when it is the only change.
4. Re-read each hunk you touched and hand-format it to match its
   surroundings. Do not run `cargo fmt`.
5. Commit once, with a message naming the lints fixed.

## Validation

- `verify:` exits 0.
- `git diff --name-only <base>..HEAD` is a subset of `touches:`.
- `cargo test -p <crate>` still passes.
- The diff contains no whitespace-only hunks. If it does, you
  reformatted something — revert those hunks.

## Traps

- `#[allow(...)]` is not a fix. Use it only when the lint is genuinely
  wrong about this code, and add a one-line comment saying why.
- Clippy will suggest collapsing `match` into `?`. That is usually
  right here and matches the house style.
- Some lints only fire under `--all-targets`, i.e. inside `#[cfg(test)]`
  blocks. Test code is in scope and follows the same rules.
