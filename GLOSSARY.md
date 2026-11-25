# CodeScope

A desktop app for running AI coding agent sessions side by side, each in its own git worktree.

## Language

### Workspace

**Project**:
An entry in the CodeScope sidebar: a local git repository with its worktrees and sessions, or a saved remote-shell command.
_Avoid_: GitHub project, repo (for the sidebar entry)

### Work intake

**Host**:
The service that holds repositories, issues, pull requests and boards, such as GitHub.
_Avoid_: forge, provider, platform

**Repository**:
A repository on a host. A project belongs to each repository its remotes point to; a board item belongs to at most one.
_Avoid_: repo URL, remote (for the host side)

**Board**:
A board on a host whose columns hold items, possibly from several repositories, that the user picks work from. On GitHub, a Projects board.
_Avoid_: GitHub project, project board

**Board item**:
One entry on a board: an issue, a pull request, or on GitHub also a draft issue.
_Avoid_: card, ticket, task, work item

**Trigger column**:
A column on a board that makes CodeScope start a phase for any item the user moves into it.
_Avoid_: watched column, automation column

**Progress column**:
A column on a board that only CodeScope moves items into, to show how far their dispatch is.
_Avoid_: status column, state column

### Dispatch

**Dispatch**:
CodeScope's work on one board item: one worktree and one agent session, going through phases, in a tab the user can open and steer at any time.
_Avoid_: assign, pick up, run, bot task

**Phase**:
One stage of a dispatch, either research or implementation, each ending in something posted on the host.
_Avoid_: step, stage

**Research**:
The phase in which the agent works out the cause and an approach for an item, without changing code.
_Avoid_: triage, investigation, analysis

**Findings**:
The comment on the item's issue or pull request in which research records the cause, the approach and open questions; the handover to implementation.
_Avoid_: report, research notes

**Decision point**:
A choice in the findings that the user makes before implementation, with lettered options and the agent's advice.
_Avoid_: question, open point

**Implementation**:
The phase in which the agent changes the code for an item and opens a pull request.
_Avoid_: build, fix

**Repair**:
A round in implementation in which the agent makes its own pull request mergeable again after a red check, a merge conflict or a base branch that moved on, without handling review comments.
_Avoid_: fix-up, auto-fix, retry

**Needs you**:
The state of a dispatch that cannot go on without the user: its session waits for input, a decision point is open, a draft pull request waits to be tested by hand, or CodeScope needs a choice before it can start.
_Avoid_: blocked, stuck, waiting

**Turn**:
Who a dispatch is waiting on: the agent, CI, the user, the reviewers, or nobody. The user's turn is needs you.
_Avoid_: owner, assignee, status
