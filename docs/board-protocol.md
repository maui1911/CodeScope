# Board protocol

What a tool does to share a GitHub Projects board with CodeScope and
with the people on it. Version 1, proposed in
[ADR-0027](DECISIONS.md#adr-0027--a-board-on-the-host-drives-dispatch).
The terms are defined in [GLOSSARY.md](../GLOSSARY.md).

A tool here is whatever starts agent sessions for one person:
CodeScope, a scheduled script, or a person working by hand. Each tool
works for one person only. Everything another tool or person needs to
know is on GitHub; sessions, worktrees and which computer runs what
are not.

## Columns

A board has one single-select field *Status*. Its options take these
roles, matched by name, ignoring case, punctuation and emoji:

| Role | Kind | Who moves items in |
| --- | --- | --- |
| Todo (also *To do*, *Backlog*) | rest | people |
| Research | trigger | people |
| Researching | progress | the tool working on the item |
| Needs decision | progress | the tool working on the item |
| Ready | trigger | people |
| In progress | progress | the tool working on the item |
| In review | progress | the tool working on the item |
| Done | host | GitHub's workflows: item closed, pull request merged |

Research is optional: an item moved from Todo straight to Ready skips
it.

## Who works on an item

1. An item belongs to the person it is assigned to. A tool never
   starts work on an item assigned to someone else.
2. A tool starts automatically only on a status change into a trigger
   column (`ProjectV2ItemStatusChangedEvent`, not `wasAutomated`) that
   was made either by its own person, or by someone with write access
   to the repository while the item is assigned to its person.
3. Before it moves the item into a progress column, the tool assigns
   the issue to its person if nobody is assigned.
4. A tool changes an item's status only while the item still has the
   status that tool set last. Whoever moves it by hand takes it over.
5. Leave an item alone once GitHub has moved it to Done.

## Comments

A comment that carries a result ends with a marker on its own line:

```text
<!-- dispatch v=1 phase=<phase> outcome=<outcome> by=<tool> -->
```

| `phase` | `outcome` | Meaning |
| --- | --- | --- |
| `research` | `findings` | Cause and approach, no open choices; the item can go to Ready |
| `research` | `decision-points` | Findings with open choices; the item is in Needs decision |
| `implementation` | `pull-request` | The pull request is open |
| `implementation` | `question` | The agent stopped and asks something |
| `repair` | `pull-request` | The pull request is mergeable again |
| `repair` | `question` | The repair stopped; a person decides |

`by` names the tool, for example `codescope`; nothing depends on it.
The line before the marker starts with `**Next step:**` and says what
a person does next.

A comment without a marker is a person's, unless a bot posted it.
Comments count as answers only from people with write access to the
repository.

### Findings

Findings use these headings, in this order:

```markdown
## Cause
## Approach
## Decision points
```

*Decision points* is left out when nothing is open. Each point is a
third-level heading `### B1 <question>`, the context in a sentence or
two, the options as a list starting with **A**, **B**, …, and a line
`Advice: A, <reason>`.

An answer is a comment such as `B1 A, B2 B`, optionally followed by
free text. Free text alone is an answer too.

## Branches and pull requests

- The branch is `<number>-<slug>`, linked to the issue
  (`createLinkedBranch`); a pull request to another branch than the
  default one says `Closes #<number>`.
- A ready pull request was tested automatically: the item moves to In
  review.
- A draft pull request has a section `## Test by hand` in its body: a
  person tests it first, and marking it ready moves the item to In
  review.
- A tool pushes a repair (red check, merge conflict, base moved on)
  only while the pull request has no submitted review, at most twice
  in a row; a comment by someone with write access starts the count
  again.

## Skills

How the team researches and builds lives in the repository as Agent
Skills (`SKILL.md`), by default named `research`, `implement`,
`address-review` and `repair`. Any tool can start a session with them;
the format of the result is this protocol's, not the skill's.
