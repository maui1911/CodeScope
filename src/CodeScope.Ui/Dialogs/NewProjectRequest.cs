namespace NoScope.CodeScope.Ui.Dialogs;

/// <summary>
/// Input envelope for <see cref="NewProjectDialog.PromptAsync(NewProjectRequest, System.Func{string?}, NoScope.CodeScope.Core.Services.IGitService)"/>.
/// </summary>
/// <param name="DefaultCloneParent">Folder pre-filled in the "Parent folder" field of the
/// Clone-from-URL mode. Caller picks: typically the parent of the most-recently-added
/// project, falling back to <c>%USERPROFILE%\source\repos</c>.</param>
public sealed record NewProjectRequest(string DefaultCloneParent);

/// <summary>
/// Result of <see cref="NewProjectDialog"/>. Exactly one of <see cref="ExistingFolder"/>
/// or <see cref="ClonedPath"/> is non-null. <see cref="WasCloned"/> is derived from
/// <see cref="ClonedPath"/> so callers can vary the success-toast wording.
/// </summary>
/// <param name="ExistingFolder">Set when the user picked "Existing folder".</param>
/// <param name="ClonedPath">Set when the dialog successfully cloned the URL.</param>
public sealed record NewProjectResult(string? ExistingFolder, string? ClonedPath)
{
    public bool WasCloned => ClonedPath is not null;
}
