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
- `SidebarViewModel.AddProjectAsync` rewired through the dialog; clone failures abort cleanly.

Out (YAGNI):
- Auth prompts — rely on the OS git credential manager (same as every other git op in the app).
- Streaming clone progress / cancellation — single shell-out + toast lifecycle, like `FetchAllAsync`.
- Submodule / shallow / branch / sparse options — can be added later if requested.

## UX

The "+" button (and the empty-state CTA) opens `NewProjectDialog`:

- Two radio buttons: **Existing folder** (default) · **Clone from URL**.
- *Existing folder* mode: a single "Browse…" button → standard folder picker. Mirrors today's flow exactly.
- *Clone from URL* mode shows three fields:
  - **Git URL** — text input. Validated against `https?://…`, `ssh://…`, or SCP-style `git@host:owner/repo.git`.
  - **Parent folder** — read-only text + "Browse…" button. Defaults to the parent of the most recently added project (read off `IProjectStore`); falls back to `%USERPROFILE%\source\repos`.
  - **Folder name** — text input. Auto-derived from the URL's repo segment on URL change (strip trailing `.git`); user-editable; must not already exist under the chosen parent.
- "Add" is disabled until the active mode validates.

Drag-drop of a folder onto the sidebar keeps using `AddProjectByPathAsync` — that path is unchanged.

## Components

### `NewProjectRequest` / `NewProjectResult` records (`CodeScope.Ui/Dialogs/`)

```csharp
public sealed record NewProjectRequest(string DefaultCloneParent);

public sealed record NewProjectResult(
    string? ExistingFolder,        // set when mode == Existing
    string? CloneUrl,              // set when mode == Clone
    string? CloneParent,
    string? CloneFolderName);
```

Exactly one of `ExistingFolder` or `CloneUrl` is non-null — the dialog's caller switches on which.

### `NewProjectDialog` (`Dialogs/NewProjectDialog.xaml[.cs]`)

- Hosted in `App.xaml.cs` via a `Func<NewProjectRequest, NewProjectResult?> PickNewProject` delegate, registered alongside `PickFolder` and `PickNewWorktree`.
- Reuses the existing folder-picker (the dialog accepts a `Func<string?> pickFolder` ctor argument so the WPF folder dialog stays in `App`).
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
result = _pickNewProject(new NewProjectRequest(DefaultCloneParent()))
if result is null: return

if result.ExistingFolder is set:
    _store.AddProjectAsync(result.ExistingFolder, displayName: null)
else:
    Toast("Cloning…", "<url>", ToastSeverity.Info)
    cloned = await _git.CloneAsync(result.CloneUrl!, result.CloneParent!, result.CloneFolderName!)
    if cloned.IsFailure:
        ErrToast("Clone failed", cloned.Error, retry: () => AddProjectAsync())
        return
    _store.AddProjectAsync(cloned.Value, displayName: null)
    Toast("Project cloned", folderName, ToastSeverity.Ok)
```

`DefaultCloneParent()` reads the most-recent project's parent off `_store.Projects`; falls back to `%USERPROFILE%\source\repos`.

The drag-drop helper `AddProjectByPathAsync` and the palette command are not touched.

## Validation rules

- **URL:** non-empty, trimmed; matches one of:
  - `^https?://\S+`
  - `^ssh://\S+`
  - `^git@\S+:\S+`
  Anything else → "Enter a valid git URL".
- **Parent folder:** must exist and be writable.
- **Folder name:** non-empty, no `\\`/`/`, no path-invalid chars, and `Path.Combine(parent, name)` must not exist (or must be an empty directory — git refuses non-empty targets anyway, so we mirror that).

Validation errors render inline under the offending field; "Add" stays disabled.

## Error handling

- Clone failure → `ErrToast` with git's stderr text. The dialog has already closed; offering a retry that re-opens the dialog with the same inputs is overkill — the toast's "retry" simply re-runs `AddProjectAsync()` (re-opens an empty dialog), matching existing patterns.
- If clone succeeds but `AddProjectAsync` fails (duplicate path, etc.) → log + toast; the clone is left on disk for the user to inspect.

## Tests (Core only, per project convention)

`tests/CodeScope.Core.Tests/GitServiceCloneTests.cs`:

1. **Happy path** — `git init --bare` a local source, clone it via `CloneAsync`, assert `Result.IsSuccess` and that the returned path is a valid worktree (`HEAD` exists).
2. **Target already exists** — pre-create the target directory, expect `Result.Failure`.
3. **Invalid URL** — pass garbage, expect `Result.Failure` with non-empty error text.

No UI/dialog tests — same posture as `NewWorktreeDialog`.

## Migration / rollout

Pure addition. `projects.json` schema unchanged. Existing "Add project" entry-points (sidebar +, empty-state CTA, command palette) all funnel through `AddProjectAsync`; drag-drop stays on the unchanged path.

## Open questions

None.
