# Add project from a git URL — design

**Issue:** [#20](https://github.com/maui1911/CodeScope/issues/20) — *uitgebreider nieuw project window waar je evt ook een .git url kan toevoegen.*
**Date:** 2026-04-29
**Status:** approved

## Problem

Today the only way to add a project is to point CodeScope at an existing local folder. If the user wants to start work on a repo they don't have on disk yet, they have to clone it themselves in a separate shell, then come back and add it. The flow should let them paste a git URL and have CodeScope clone + register the project in one step.

## Scope

In:
- A new `NewProjectDialog` with two modes: *Existing folder* (current behaviour) and *Clone from URL*.
- A `git clone` shell-out on `IGitService`.
- An inline busy state inside the dialog (spinner + "Cloning…") so the user sees progress on big repos.
- `SidebarViewModel.AddProjectAsync` rewired through the dialog; clone failures surface inline.

Out (YAGNI):
- Auth prompts — rely on the OS git credential manager (same as every other git op in the app).
- Streaming clone progress (objects/deltas %) — an indeterminate spinner is enough for the "is it still working?" question. Real progress would need parsing `--progress` output on a separate stderr pump.
- Submodule / shallow / branch / sparse options — can be added later if requested.

## UX

The "+" button (and the empty-state CTA) opens `NewProjectDialog`:

- Two radio buttons: **Existing folder** (default) · **Clone from URL**.
- *Existing folder* mode: a single "Browse…" button → standard folder picker. Mirrors today's flow exactly.
- *Clone from URL* mode shows three fields:
  - **Git URL** — text input. Validated against `https?://…`, `ssh://…`, or SCP-style `git@host:owner/repo.git`.
  - **Parent folder** — editable text input + "Browse…" button. Defaults to the parent of the most recently added project (read off `IProjectStore`); falls back to `%USERPROFILE%\source\repos`.
  - **Folder name** — text input. Auto-derived from the URL's repo segment on URL change (strip trailing `.git`); user-editable; must not already exist under the chosen parent.
- "Add" is disabled until the active mode validates.

When the user clicks **Add** in *Clone from URL* mode the dialog stays open and switches to a **busy state**:

- Inputs and the mode toggle are disabled.
- "Add" is replaced by an indeterminate spinner (`ProgressBar IsIndeterminate="True"`) plus the caption *"Cloning &lt;repo&gt;…"*.
- "Cancel" stays enabled and aborts the clone via `CancellationToken` (kills the `git` process); the dialog returns to the editable state.
- On success, the dialog closes with the result. On failure, the dialog returns to the editable state and renders the git error inline beneath the URL field — no toast, since the user is still looking at the dialog.

*Existing folder* mode never shows the busy state; the dialog closes immediately and the store call is fast.

Drag-drop of a folder onto the sidebar keeps using `AddProjectByPathAsync` — that path is unchanged.

## Components

### `NewProjectRequest` / `NewProjectResult` records (`CodeScope.Ui/Dialogs/`)

```csharp
public sealed record NewProjectRequest(string DefaultCloneParent);

public sealed record NewProjectResult(
    string? ExistingFolder,   // set when user picked "Existing folder"
    string? ClonedPath,       // set when the dialog successfully cloned
    bool WasCloned);
```

Exactly one of `ExistingFolder` or `ClonedPath` is non-null. `WasCloned` is just for the caller's success-toast wording.

### `NewProjectDialog` (`Dialogs/NewProjectDialog.xaml[.cs]`)

- Hosted in `App.xaml.cs` via a `Func<NewProjectRequest, Task<NewProjectResult?>> PickNewProject` delegate, registered alongside `PickFolder` and `PickNewWorktree`. Async because the dialog awaits the clone before returning.
- Reuses the existing folder-picker (the dialog accepts a `Func<string?> pickFolder` ctor argument so the WPF folder dialog stays in `App`).
- Accepts an `IGitService gitService` ctor argument so the busy state can drive `CloneAsync` directly. The dialog owns the `CancellationTokenSource` for the clone — Cancel cancels it; closing the window cancels it.
- Dialog styling follows `NewWorktreeDialog` (same shell, same buttons, same window-chrome treatment).

### `IGitService.CloneAsync`

```csharp
Task<Result<string>> CloneAsync(
    string url,
    string parentDir,
    string folderName,
    CancellationToken ct = default);
```

Implementation: `git -C <parentDir> clone -- <url> <folderName>`, then return `Path.Combine(parentDir, folderName)`. Stderr from git is forwarded into `Result.Failure` verbatim (matches the rest of `GitService`'s contract). Pre-flight: refuse when the target directory already exists or is non-empty.

### `SidebarViewModel.AddProjectAsync`

```
result = await _pickNewProject(new NewProjectRequest(DefaultCloneParent()))
if result is null: return  // user cancelled

// Clone (if any) already ran inside the dialog — result carries the resolved local path.
var path = result.ExistingFolder ?? result.ClonedPath!
var add = await _store.AddProjectAsync(path, displayName: null)
if add.IsFailure:
    ErrToast(result.WasCloned ? "Clone added but project add failed" : "Add project failed", add.Error)
else if result.WasCloned:
    Toast("Project cloned", add.Value.Name, ToastSeverity.Ok)
```

`NewProjectResult` therefore carries the resolved local path (`ClonedPath` set after a successful clone) plus a `WasCloned` flag for the success-toast wording. The dialog is the only place that calls `IGitService.CloneAsync`.

`DefaultCloneParent()` reads the most-recent project's parent off `_store.Projects`; falls back to `%USERPROFILE%\source\repos`.

The drag-drop helper `AddProjectByPathAsync` and the palette command are not touched.

## Validation rules

- **URL:** non-empty, trimmed; must match a supported scheme (http(s)://, ssh://, git@host:owner/repo). Invalid URLs disable the Add button; there is no dedicated inline error message — malformed or unreachable URLs that pass the basic scheme check are surfaced by the clone attempt itself.
- **Parent folder:** must exist and be writable.
- **Folder name:** non-empty, no `\\`/`/`, no path-invalid chars, and `Path.Combine(parent, name)` must not exist (or must be an empty directory — git refuses non-empty targets anyway, so we mirror that).

Validation errors render inline under the offending field; "Add" stays disabled.

## Error handling

- Clone failure → dialog returns to editable state with git's stderr rendered inline beneath the URL field. User can edit and retry without re-typing anything. No toast.
- Clone cancellation → dialog returns to editable state silently. The partially-cloned target directory is removed on a best-effort basis.
- If clone succeeds but `_store.AddProjectAsync` fails (duplicate path, etc.) → toast + log; the clone is left on disk for the user to inspect.

## Tests (Core only, per project convention)

`tests/CodeScope.Core.Tests/GitServiceCloneTests.cs`:

1. **Happy path** — `git init --bare` a local source, clone it via `CloneAsync`, assert `Result.IsSuccess` and that the returned path is a valid worktree (`HEAD` exists).
2. **Target already exists** — pre-create the target directory, expect `Result.Failure`.
3. **Invalid URL** — pass garbage, expect `Result.Failure` with non-empty error text.
4. **Cancellation** — start a clone with an already-cancelled token, expect `Result.Failure` and no orphan target directory.

No UI/dialog tests — same posture as `NewWorktreeDialog`.

## Migration / rollout

Pure addition. `projects.json` schema unchanged. Existing "Add project" entry-points (sidebar +, empty-state CTA, command palette) all funnel through `AddProjectAsync`; drag-drop stays on the unchanged path.

## Open questions

None.
