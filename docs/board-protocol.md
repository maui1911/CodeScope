# Board protocol

What a tool does to share a GitHub Projects board with CodeScope and
with the people on it. Version 1, proposed in
[ADR-0027](DECISIONS.md#adr-0027--a-board-on-the-host-drives-dispatch).
The terms are defined in [GLOSSARY.md](../GLOSSARY.md).

A tool here is whatever starts agent sessions for one person:
CodeScope, a scheduled script, or a person working by hand. Each tool
works for one person only, and a person runs one tool per board:
GitHub shows two tools of the same person as that one person.
Everything another tool or person needs to know is on GitHub;
sessions, worktrees and which computer runs what are not.

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
it. A tool may let its person map other column names to these roles,
but other tools on the board then don't see them; a shared board
uses these names.

## Who works on an item

1. An item belongs to the people it is assigned to. A tool never
   starts work on an item assigned only to others.
2. A tool starts automatically only on a status change into a trigger
   column (`ProjectV2ItemStatusChangedEvent`, not `wasAutomated`) made
   after its person added the board, and made either by its own
   person, or by someone with write access to the repository while the
   item is assigned to its person alone. With several assignees, only
   the tool of the assignee who made the move starts.
3. Before it moves the item into a progress column, the tool assigns
   the issue to its person if nobody is assigned.
4. A tool moves an item in two ways only: from a trigger column into
   the progress column of the round a person's move starts, and on
   from the progress column the tool itself put it in, once per round.
   A person's move into a trigger column hands the item to the tool
   again; a move anywhere else ends the tool's moves for that round. A
   move a person makes through a tool is that person's own move.
5. Leave an item alone once GitHub has moved it to Done.
6. A draft issue waits for a person to convert it to an issue: it has
   no repository and no timeline that says who moved it.

## Rounds

A move into Research starts research. A move into Ready starts
implementation on an issue without an open pull request, and a review
round on the tool's own pull request or on a pull request item; an
issue with an open pull request from elsewhere waits for a person.
Implementation and review rounds move the item to In progress when
they start, and to In review when they end with a ready pull request.

## Comments

A comment that carries a result ends with a marker on its own line:

```text
<!-- dispatch v=1 phase=<phase> outcome=<outcome> by=<tool> -->
```

| `phase` | `outcome` | Meaning |
| --- | --- | --- |
| `research` | `findings` | Cause and approach, no open choices; the item is in Needs decision and can go to Ready |
| `research` | `decision-points` | Findings with open choices; the item is in Needs decision |
| `implementation` | `pull-request` | The pull request is open, after implementation or again after a review round or a repair |

`phase` is `research` or `implementation`; a review round and a repair
are rounds in implementation. The tool working on the item posts these
comments, on its issue, or on the pull request when the item is one.
An agent posts no comments of its own there: they would look like its
person's.
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
third-level heading `### B1 <choice>`, the context in a sentence or
two, the options as a list starting with **A**, **B**, …, and a line
`Advice: A, <reason>`.

An answer is a comment such as `B1 A, B2 B`, optionally followed by
free text. Free text alone is an answer too.

## Branches and pull requests

- For an issue, the branch is `<number>-<slug>`, linked to the issue
  (`createLinkedBranch`), and the pull request says
  `Closes #<number>`. Only a merge into the default branch closes the
  issue: GitHub ignores both for a pull request into another branch,
  and a person closes the issue when that work lands.
- A ready pull request was tested automatically: the item moves to In
  review.
- A draft pull request has a section `## Test by hand` in its body: a
  person tests it first, and marking it ready moves the item to In
  review.
- A tool pushes a repair (red check, merge conflict, base moved on)
  only while the pull request has no submitted review, at most twice
  in a row; a comment by someone other than the tool's person, with
  write access, starts the count again.

## Skills

How the team researches and implements lives in the repository as Agent
Skills (`SKILL.md`), by default named `research`, `implement`,
`address-review` and `repair`. Any tool can start a session with them;
the format of the result is this protocol's, not the skill's. Agent
definitions in the repository come along the same way, so a skill can
hand work to them.
