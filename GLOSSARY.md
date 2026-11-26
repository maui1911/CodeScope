# CodeScope

A desktop app for running AI coding agent sessions side by side, each in its own git worktree.

## Language

### Workspace

**Project**:
An entry in the CodeScope sidebar: a local folder, normally a git repository, with its worktrees and sessions, or a saved remote-shell command.
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
A column on a board that makes a dispatch, or its next round, start for an item a person moves into it.
_Avoid_: watched column, automation column

**Progress column**:
A column on a board that only the tool working on an item moves it into, to show how far its dispatch is.
_Avoid_: status column, state column

**Tool**:
Whatever starts agent sessions on a board for one person: CodeScope, a script, or the person working by hand.
_Avoid_: client, integration, bot

**Board protocol**:
The rules that let several people and tools share one board: what each column is for, who works on an item, and how results are marked in comments.
_Avoid_: API, contract, integration

### Dispatch

**Dispatch**:
The work on one board item for the person it is assigned to: one worktree on that person's computer and the agent session working in it, going through phases, in a tab they can open and steer at any time.
_Avoid_: assign, pick up, run, bot task

**Phase**:
One stage of a dispatch: research, which ends with the findings, or implementation, which ends when its pull request is merged or closed.
_Avoid_: step, stage

**Round**:
One stretch of work in a dispatch that a single event starts and a single result ends: the research round, the implementation round, a review round or a repair.
_Avoid_: run, job, iteration

**Research**:
The phase in which the agent works out the cause and an approach for an item, without changing code.
_Avoid_: triage, investigation, analysis

**Findings**:
The comment on the item's issue or pull request in which research records the cause, the approach and any decision points; the handover to the round that Ready starts.
_Avoid_: report, research notes

**Decision point**:
A choice in the findings that the user makes before the item goes to Ready, with lettered options and the agent's advice.
_Avoid_: question, open point

**Implementation**:
The phase in which the agent changes the code for an item: for an issue, its first round opens a pull request, and review rounds and repairs keep working on it; for a pull request item, review rounds work on that pull request.
_Avoid_: build, fix

**Review round**:
A round in implementation that a move to Ready starts on an existing pull request: the agent handles its review comments, and the findings and answers when research came first.
_Avoid_: rework, address review

**Repair**:
A round in implementation in which the agent makes its own pull request mergeable again after a red check, a merge conflict or a base branch that moved on, without handling review comments.
_Avoid_: fix-up, auto-fix, retry

**Needs you**:
The state of a dispatch that cannot go on without the person it is assigned to: its session waits for input, findings wait for a decision, a draft pull request waits to be tested by hand, or a choice is needed before it can start. Part of it only that person's own tool knows, so only they see it.
_Avoid_: blocked, stuck, waiting

**Turn**:
Who a dispatch is waiting on, as GitHub shows it to everyone on the board: an agent, CI, a person, the reviewers, or nobody. When it is the turn of the person the dispatch is assigned to, it needs them.
_Avoid_: owner, assignee, status
