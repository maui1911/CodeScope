# labs/

Experiments that are **not part of the product**.

Rules for everything under this directory:

1. **Nothing here ships.** `labs/` is not referenced by `Cargo.toml`,
   not compiled, not packaged by cargo-dist, and not installed.
2. **No product code may depend on it.** The dependency arrow only
   ever points `labs/ -> product`, never back. If an experiment
   graduates, it gets ported into `core/` or `src/` through a normal
   PR with tests — the labs copy is then deleted, not kept in sync.
3. **It may be deleted without ceremony.** An experiment that has not
   been touched in a release cycle is dead weight; drop it.
4. **It is allowed to be rough.** No clippy gate, no test coverage
   requirement, no API stability. That is the whole point of a
   sandbox — the cost of an idea being wrong here is a deleted folder.

Everything else in `CLAUDE.md` still applies: English only, never
commit to `main`, never run repo-wide `cargo fmt`.

## Experiments

| Folder | What | Status |
|---|---|---|
| [`agent-bots/`](agent-bots/) | Persistent named agents coordinating through a file contract and git worktrees, in the shape of xAI's Grok Bot | Prototype |
