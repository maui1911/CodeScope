//! "Add project" dialog — Rust port of
//! `src/CodeScope.Ui/Dialogs/NewProjectDialog.xaml(.cs)`.
//!
//! Mirrors the C# UX 1:1: a segmented mode toggle between
//! "Existing folder" (the default) and "Clone from URL", an inline
//! error row, and a footer caption that reflects the current mode.
//! On confirm, we either accept the picked folder verbatim or run
//! `git clone` and accept the resulting destination — in both cases
//! the path is handed to [`Sidebar::add_project`] which writes
//! `projects.json` before mutating the in-memory list.
//!
//! Why a custom modal instead of the bare native folder picker the
//! Rust port had before: the C# build wraps the picker in a richer
//! flow (clone-from-URL is the second mode, default-parent heuristic,
//! inline duplicate / "destination not empty" errors) and parity is
//! the goal. Live state lives next to the [`crate::sidebar::Sidebar`]
//! entity for the same reason `NewWorktreeDialogState` does — opening
//! and persisting the result both run against the same
//! `ProjectsConfig`, so threading the dialog state through a separate
//! entity would only add ceremony.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use codescope_core::Theme;
use gpui::{
    AppContext, Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, PathPromptOptions, SharedString, Styled, Window, anchored,
    deferred, div, point, px,
};

use crate::sidebar::Sidebar;
use crate::text_field::{TextField, focused_caret_style, render_input_content};
use crate::theme;

/// Two ways to add a project. Mirrors the segmented control at the
/// top of the C# dialog body. [`Existing`] is the default — the user
/// already has a checkout on disk and just wants the sidebar to track
/// it. [`Clone`] runs `git clone <url> <parent>/<name>` first and then
/// adds the resulting path. Toggling fields are preserved across mode
/// switches so a user who started filling one and then realised they
/// wanted the other doesn't lose their typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    Existing,
    Clone,
}

/// Which input field the dialog routes typed characters into. The
/// "Existing folder" path field is read-only (Browse-only) and is
/// therefore not represented here. Mirrors the focus semantics of the
/// C# `Url`/`Parent`/`Name` text boxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogField {
    Url,
    Parent,
    Name,
}

/// Live state of an open dialog. Created by
/// [`Sidebar::open_new_project_dialog`] and dropped on confirm /
/// cancel. Field naming and semantics match the C# build's
/// `NewProjectDialog` so the diffs read cleanly side-by-side.
pub struct NewProjectDialogState {
    pub mode: DialogMode,
    pub focused_field: DialogField,
    pub focus_handle: FocusHandle,
    /// Absolute path picked via the platform folder dialog when in
    /// [`DialogMode::Existing`]. Empty until the user clicks Browse.
    pub existing_path: String,
    /// Clone-mode fields — all three are user-editable. Each is a
    /// [`TextField`] so the caret can be moved with the arrow keys /
    /// Home / End and characters inserted mid-string.
    pub url: TextField,
    pub parent: TextField,
    pub name: TextField,
    /// `true` while the user has not customised the auto-derived
    /// folder name. Once they edit it, BRANCH→NAME re-derive stops.
    /// Mirrors the C# `NameBox.Tag == NameBox.Text` heuristic.
    pub name_auto: bool,
    /// `true` while a `git clone` task is in flight.
    pub busy: bool,
    /// "Cloning <name>…" caption while busy.
    pub busy_text: Option<String>,
    /// Inline error text rendered under the body. Set by the submit
    /// path and cleared on the next typing change.
    pub error: Option<String>,
}

impl NewProjectDialogState {
    pub fn new(default_clone_parent: String, focus_handle: FocusHandle) -> Self {
        Self {
            mode: DialogMode::Existing,
            focused_field: DialogField::Url,
            focus_handle,
            existing_path: String::new(),
            url: TextField::new(),
            parent: TextField::with_text(default_clone_parent),
            name: TextField::new(),
            name_auto: true,
            busy: false,
            busy_text: None,
            error: None,
        }
    }

    /// Add-button enablement. Mirrors C#'s `RefreshAddEnabled`:
    /// existing-mode requires a non-empty, existing directory;
    /// clone-mode requires a syntactically-valid url, an existing
    /// parent, and a name with no path-invalid characters. Returning
    /// `bool` (not `Result`) keeps the render path branch-free —
    /// callers only need to know whether to grey out the Add button.
    pub fn is_valid(&self) -> bool {
        if self.busy {
            return false;
        }
        match self.mode {
            DialogMode::Existing => {
                let p = self.existing_path.trim();
                !p.is_empty() && Path::new(p).is_dir()
            }
            DialogMode::Clone => {
                if !is_valid_git_url(self.url.text()) {
                    return false;
                }
                let parent = self.parent.text().trim();
                let name = self.name.text().trim();
                if parent.is_empty() || !Path::new(parent).is_dir() {
                    return false;
                }
                !name.is_empty() && !contains_invalid_filename_chars(name)
            }
        }
    }

    /// Mutable accessor for a specific clone-mode field by name.
    /// Used by the mouse-down hit-test path so a click on a non-
    /// focused field can both shift focus AND drop the caret at the
    /// click position in one step. The existing-mode path field is
    /// read-only — typed characters skip it via `focused_field_mut`,
    /// but it's still reachable here for completeness.
    pub fn field_mut_by(&mut self, field: DialogField) -> &mut TextField {
        match field {
            DialogField::Url => &mut self.url,
            DialogField::Parent => &mut self.parent,
            DialogField::Name => &mut self.name,
        }
    }

    /// Mutable accessor for the currently-focused clone-mode field.
    /// Returns `None` when the dialog is busy, in existing-mode (no
    /// typeable field), or the focus enum doesn't map to a real input.
    /// Inserting / deleting through this returns the same field so the
    /// caller can apply post-edit hooks (`maybe_redrive_name`).
    fn focused_field_mut(&mut self) -> Option<&mut TextField> {
        if self.busy {
            return None;
        }
        if self.mode == DialogMode::Existing {
            // The existing-path field is read-only — typed characters
            // outside Browse are dropped. Mirrors the WPF
            // `IsReadOnly="True"` on the path TextBox.
            return None;
        }
        Some(match self.focused_field {
            DialogField::Url => &mut self.url,
            DialogField::Parent => &mut self.parent,
            DialogField::Name => &mut self.name,
        })
    }

    /// Insert a typed character at the focused field's caret. Honours
    /// the same auto-derive + read-only rules the legacy `append_char`
    /// used to. Returns `true` when a buffer was actually touched (so
    /// the caller can `wake_text_blink` + notify).
    pub fn insert_char(&mut self, ch: char) -> bool {
        if self.focused_field_mut().is_none() {
            return false;
        }
        let field = self.focused_field;
        match field {
            DialogField::Url => {
                self.url.insert_char(ch);
                self.maybe_redrive_name();
            }
            DialogField::Parent => {
                self.parent.insert_char(ch);
            }
            DialogField::Name => {
                self.name_auto = false;
                self.name.insert_char(ch);
            }
        }
        self.error = None;
        true
    }

    pub fn backspace(&mut self) -> bool {
        if self.focused_field_mut().is_none() {
            return false;
        }
        let field = self.focused_field;
        let changed = match field {
            DialogField::Url => {
                let c = self.url.backspace();
                if c {
                    self.maybe_redrive_name();
                }
                c
            }
            DialogField::Parent => self.parent.backspace(),
            DialogField::Name => {
                let c = self.name.backspace();
                if c {
                    self.name_auto = false;
                }
                c
            }
        };
        if changed {
            self.error = None;
        }
        changed
    }

    pub fn delete_forward(&mut self) -> bool {
        if self.focused_field_mut().is_none() {
            return false;
        }
        let field = self.focused_field;
        let changed = match field {
            DialogField::Url => {
                let c = self.url.delete_forward();
                if c {
                    self.maybe_redrive_name();
                }
                c
            }
            DialogField::Parent => self.parent.delete_forward(),
            DialogField::Name => {
                let c = self.name.delete_forward();
                if c {
                    self.name_auto = false;
                }
                c
            }
        };
        if changed {
            self.error = None;
        }
        changed
    }

    pub fn move_caret_left(&mut self) -> bool {
        let Some(field) = self.focused_field_mut() else { return false };
        field.move_left()
    }

    pub fn move_caret_right(&mut self) -> bool {
        let Some(field) = self.focused_field_mut() else { return false };
        field.move_right()
    }

    pub fn move_caret_home(&mut self) -> bool {
        let Some(field) = self.focused_field_mut() else { return false };
        field.move_home()
    }

    pub fn move_caret_end(&mut self) -> bool {
        let Some(field) = self.focused_field_mut() else { return false };
        field.move_end()
    }

    fn maybe_redrive_name(&mut self) {
        if self.name_auto {
            self.name.set_text(derive_repo_name(self.url.text()));
        }
    }
}

/// Mirror of C#'s `NewProjectDialog.IsValidGitUrl`. Accepts http(s)://,
/// ssh://, and SCP-style `git@host:owner/repo`. Anything shorter or
/// missing the post-scheme body fails. Pulled out as a pure helper so
/// the validity check can be unit-tested without instantiating the
/// dialog state (which holds a `FocusHandle`).
pub fn is_valid_git_url(url: &str) -> bool {
    let u = url.trim();
    if u.is_empty() {
        return false;
    }
    let lower = u.to_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("ssh://") {
        if let Some(idx) = u.find("://") {
            return idx + 3 < u.len();
        }
        return false;
    }
    if lower.starts_with("git@") {
        // Require host + ':' + path.
        let after = &u["git@".len()..];
        if let Some(colon) = after.find(':') {
            return colon > 0 && colon < after.len() - 1;
        }
    }
    false
}

/// Mirror of C#'s `NewProjectDialog.DeriveRepoName`. Strips trailing
/// `/`, the `.git` suffix, then takes the segment after the last `/`
/// or `:`. Replaces filename-invalid characters with `-` defensively
/// so a hostile URL can't smuggle a `*` into the destination folder.
pub fn derive_repo_name(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    let base = if trimmed.to_lowercase().ends_with(".git") {
        &trimmed[..trimmed.len() - 4]
    } else {
        trimmed
    };
    let leaf = match base.rfind(['/', ':']) {
        Some(idx) => &base[idx + 1..],
        None => base,
    };
    leaf.chars()
        .map(|c| if INVALID_FILENAME_CHARS.contains(&c) || c.is_control() { '-' } else { c })
        .collect()
}

const INVALID_FILENAME_CHARS: &[char] =
    &['<', '>', ':', '"', '|', '?', '*', '/', '\\', '\0'];

fn contains_invalid_filename_chars(s: &str) -> bool {
    s.chars().any(|c| INVALID_FILENAME_CHARS.contains(&c) || c.is_control())
}

/// Heuristic for the dialog's default "Parent folder" value, mirroring
/// the C# `DefaultCloneParent`. Pure helper so it can be tested
/// without `Sidebar`. Resolution order:
/// 1. the parent of the most-recently-added project (when its parent
///    exists on disk),
/// 2. `<home>/source/repos` when that exists,
/// 3. `<home>` otherwise.
///
/// On hosts where we can't resolve a home directory at all we hand
/// back the OS temp dir, since `cx.prompt_for_paths` needs *some*
/// existing path as a starting point. Matches the C# fallback exactly.
pub fn default_clone_parent(existing_paths: &[&str], home: Option<&Path>) -> String {
    if let Some(last) = existing_paths.iter().rev().find(|p| !p.trim().is_empty()) {
        let trimmed = last.trim_end_matches(['\\', '/']);
        if let Some(parent) = Path::new(trimmed).parent()
            && parent.is_dir()
        {
            return parent.to_string_lossy().into_owned();
        }
    }
    if let Some(h) = home
        && h.is_dir()
    {
        let candidate = h.join("source").join("repos");
        if candidate.is_dir() {
            return candidate.to_string_lossy().into_owned();
        }
        return h.to_string_lossy().into_owned();
    }
    std::env::temp_dir().to_string_lossy().into_owned()
}

impl Sidebar {
    /// Open the modal. Mirrors the entry point the C# `+` button hits.
    /// Replaces the previous bare-picker flow — clicking "+" now opens
    /// the in-app modal, and the dialog itself drives the platform
    /// folder picker as part of its Browse button.
    ///
    /// Gated: returns silently if a "New project" or "New worktree"
    /// dialog is already open. All call sites — the heading `+`
    /// glyph and the sidebar footer's "New Project" button — share
    /// this single gate so neither path can stack a second modal on
    /// top of an existing one.
    pub fn open_new_project_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.new_project_dialog().is_some() || self.dialog().is_some() {
            return;
        }
        let existing_paths: Vec<&str> = self
            .projects()
            .projects
            .iter()
            .map(|p| p.path.as_str())
            .collect();
        let home = home_dir();
        let parent = default_clone_parent(&existing_paths, home.as_deref());
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let state = NewProjectDialogState::new(parent, focus_handle);
        self.set_new_project_dialog(Some(state));
        self.close_menu_no_notify();
        cx.notify();
    }

    /// Close without adding anything.
    pub fn cancel_new_project_dialog(&mut self, cx: &mut Context<Self>) {
        if self.take_new_project_dialog().is_some() {
            cx.notify();
        }
    }

    /// Switch the segmented mode toggle. Idempotent on the active
    /// mode.
    pub fn set_new_project_mode(&mut self, mode: DialogMode, cx: &mut Context<Self>) {
        if let Some(state) = self.new_project_dialog_mut() {
            if state.busy || state.mode == mode {
                return;
            }
            state.mode = mode;
            // Reset focus so the freshly-revealed panel's first
            // typeable field receives keystrokes. Existing-mode has
            // no typeable field; we still set Url so a follow-up
            // mode flip back lands on URL.
            state.focused_field = match mode {
                DialogMode::Existing => DialogField::Url,
                DialogMode::Clone => DialogField::Url,
            };
            state.error = None;
            cx.notify();
        }
    }

    /// Switch which clone-mode field receives typed characters.
    pub fn focus_new_project_field(&mut self, field: DialogField, cx: &mut Context<Self>) {
        if let Some(state) = self.new_project_dialog_mut() {
            if state.busy {
                return;
            }
            state.focused_field = field;
            cx.notify();
        }
    }

    /// Run the platform folder picker for the existing-mode path.
    pub fn pick_existing_project_folder(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.new_project_dialog().is_none() {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match rx.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(err)) => {
                    eprintln!("warning: file picker failed: {err:#}");
                    return;
                }
            };
            if let Some(path) = paths.into_iter().next() {
                let path_str = path.to_string_lossy().into_owned();
                let _ = this.update(cx, |this, cx| {
                    if let Some(state) = this.new_project_dialog_mut() {
                        state.existing_path = path_str;
                        state.error = None;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Same picker, used by clone-mode's "Parent folder" Browse.
    pub fn pick_clone_parent_folder(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.new_project_dialog().is_none() {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Pick parent folder".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match rx.await {
                Ok(Ok(Some(paths))) => paths,
                Ok(Ok(None)) | Err(_) => return,
                Ok(Err(err)) => {
                    eprintln!("warning: file picker failed: {err:#}");
                    return;
                }
            };
            if let Some(path) = paths.into_iter().next() {
                let path_str = path.to_string_lossy().into_owned();
                let _ = this.update(cx, |this, cx| {
                    if let Some(state) = this.new_project_dialog_mut() {
                        state.parent.set_text(path_str);
                        state.error = None;
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Confirm the dialog. Existing-mode validates the duplicate
    /// rule and hands the path to [`Sidebar::add_project`]. Clone-
    /// mode runs `git clone` on a background task and, on success,
    /// hands the resolved destination over the same way.
    pub fn submit_new_project_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.new_project_dialog() else { return };
        if !state.is_valid() {
            return;
        }
        let mode = state.mode;
        match mode {
            DialogMode::Existing => {
                let path = state.existing_path.trim().to_string();
                // Duplicate detection — mirrors C# `SessionStore.AddProjectAsync`'s
                // path-fold check. We surface the error inline so the
                // user can hit Browse again instead of having the
                // dialog silently close on "no-op".
                if self
                    .projects()
                    .find_project_index_by_path(&path)
                    .is_some()
                {
                    if let Some(state) = self.new_project_dialog_mut() {
                        state.error =
                            Some(format!("Project already added: {path}"));
                    }
                    cx.notify();
                    return;
                }
                self.cancel_new_project_dialog(cx);
                self.add_project(path, cx);
            }
            DialogMode::Clone => {
                let url = state.url.text().trim().to_string();
                let parent = state.parent.text().trim().to_string();
                let name = state.name.text().trim().to_string();
                let target = Path::new(&parent).join(&name);
                let target_str = target.to_string_lossy().into_owned();
                if self
                    .projects()
                    .find_project_index_by_path(&target_str)
                    .is_some()
                {
                    if let Some(state) = self.new_project_dialog_mut() {
                        state.error =
                            Some(format!("Project already added: {target_str}"));
                    }
                    cx.notify();
                    return;
                }
                if let Some(state) = self.new_project_dialog_mut() {
                    state.busy = true;
                    state.busy_text = Some(format!("Cloning {name}…"));
                    state.error = None;
                }
                cx.notify();

                let parent_path = PathBuf::from(parent);
                let url_for_task = url.clone();
                let name_for_task = name.clone();
                cx.spawn(async move |this, cx| {
                    let result = cx
                        .background_spawn(async move {
                            codescope_core::git::clone_repo(
                                &url_for_task,
                                &parent_path,
                                &name_for_task,
                            )
                        })
                        .await;
                    let _ = this.update(cx, |this, cx| match result {
                        Ok(target) => {
                            this.cancel_new_project_dialog(cx);
                            this.add_project(target.to_string_lossy().into_owned(), cx);
                        }
                        Err(err) => {
                            if let Some(state) = this.new_project_dialog_mut() {
                                state.busy = false;
                                state.busy_text = None;
                                state.error = Some(format!("{err:#}"));
                            }
                            cx.notify();
                        }
                    });
                })
                .detach();
            }
        }
    }

    /// Render the modal. Returns `None` when no dialog is open.
    /// Layered above everything else via `deferred(anchored(...))`,
    /// matching the new-worktree dialog's overlay strategy.
    pub fn render_new_project_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.new_project_dialog()?;
        let viewport = window.viewport_size();

        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let ink_muted = theme::ink_muted(theme);
        let frost = theme::frost_10(theme);
        let danger = theme::danger();
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);

        let mode = state.mode;
        let busy = state.busy;
        let valid = state.is_valid();
        let error_msg: Option<SharedString> =
            state.error.as_ref().map(|e| e.clone().into());
        let busy_text: Option<SharedString> =
            state.busy_text.as_ref().map(|s| s.clone().into());
        let focus_handle = state.focus_handle.clone();

        // Header — eyebrow + title + subtitle, mirroring the C# XAML.
        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_5()
            .pt_5()
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(theme::accent(theme))
                    .child("ADD PROJECT"),
            )
            .child(
                div()
                    .text_size(px(20.0))
                    .text_color(ink)
                    .child("Add a project"),
            )
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(ink_muted)
                    .child("Bring a folder in, or clone a repo straight in."),
            );

        // Segmented mode toggle.
        let seg = |id: &'static str, label: &'static str, target: DialogMode| {
            let active = mode == target;
            let bg = if active { accent } else { gpui::transparent_black() };
            let fg = if active { canvas } else { ink_muted };
            div()
                .id(id)
                .flex_grow()
                .h(px(28.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .bg(bg)
                .text_size(px(12.5))
                .text_color(fg)
                .cursor_pointer()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.set_new_project_mode(target, cx);
                    }),
                )
                .child(label)
        };
        let mode_toggle = div()
            .px_5()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_1()
                    .p(px(2.0))
                    .bg(canvas)
                    .rounded(px(6.0))
                    .child(seg("np-mode-existing", "Existing folder", DialogMode::Existing))
                    .child(seg("np-mode-clone", "Clone from URL", DialogMode::Clone)),
            );

        // Reusable label + chrome.
        let field_label = |text: &'static str| {
            div()
                .text_size(px(11.0))
                .text_color(ink_ghost)
                .child(text)
        };
        // Inline read-only chrome with Browse on the right (existing mode + clone parent).
        // `on_browse` is the closure produced by `cx.listener(...)` —
        // we accept it generic-ly so the caller's closure type stays
        // anonymous (boxing it would force a different signature than
        // `on_mouse_down` accepts).
        let path_chrome = |id: &'static str,
                           value: &str,
                           on_browse: Box<
            dyn Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static,
        >| {
            let display: SharedString = if value.is_empty() {
                SharedString::from("")
            } else {
                value.to_string().into()
            };
            div()
                .id(id)
                .h(px(36.0))
                .bg(canvas)
                .border_1()
                .border_color(divider)
                .rounded(px(6.0))
                .flex()
                .flex_row()
                .items_center()
                .child(
                    div()
                        .px_3()
                        .flex_grow()
                        .text_size(px(13.0))
                        .text_color(if value.is_empty() { ink_ghost } else { ink })
                        .truncate()
                        .child(display),
                )
                .child(
                    div()
                        // Stable per-row id so gpui can persist hover
                        // state. Length-bounded to keep it reasonable —
                        // `id` is one of the dialog's static field ids.
                        .id(("np-browse", id.len() as u64))
                        .px_3()
                        .h_full()
                        .border_l_1()
                        .border_color(divider)
                        .flex()
                        .items_center()
                        .text_size(px(11.0))
                        .text_color(ink_dim)
                        .cursor_pointer()
                        .hover(move |s| s.bg(frost))
                        .on_mouse_down(MouseButton::Left, on_browse)
                        .child("Browse"),
                )
        };

        // Plain text input (no Browse button) used for URL / Name.
        // Renders the buffer split at the caret with the caret bar
        // painted inline (no gap) — only when the field is focused
        // *and* the global blink phase is on.
        let blink_phase = self.text_blink_phase;
        let textbox = |id: &'static str,
                       field: &TextField,
                       placeholder: &'static str,
                       this_field: DialogField|
         -> gpui::Stateful<gpui::Div> {
            let is_focused = state.focused_field == this_field && mode == DialogMode::Clone;
            let mut style = focused_caret_style(theme, blink_phase);
            // Suppress the caret entirely on unfocused fields; only
            // the field receiving keystrokes should advertise itself
            // with a caret bar.
            style.show_caret = is_focused && blink_phase;
            div()
                .id(id)
                .px_3()
                .h(px(36.0))
                .bg(canvas)
                .border_1()
                .border_color(if is_focused { accent } else { divider })
                .rounded(px(6.0))
                .text_size(px(13.0))
                .cursor_pointer()
                .flex()
                .items_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.focus_new_project_field(this_field, cx);
                        if let Some(state) = this.new_project_dialog_mut() {
                            let idx = state
                                .field_mut_by(this_field)
                                .index_for_window_point(event.position);
                            if let Some(idx) = idx {
                                state.field_mut_by(this_field).set_caret(idx);
                                cx.notify();
                            }
                        }
                    }),
                )
                .child(render_input_content(
                    field,
                    SharedString::from(placeholder),
                    style,
                ))
        };

        // Body — mode-dependent.
        let body: gpui::AnyElement = match mode {
            DialogMode::Existing => {
                let chrome = path_chrome(
                    "np-existing",
                    &state.existing_path,
                    Box::new(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.pick_existing_project_folder(window, cx);
                    })),
                );
                div()
                    .px_5()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(field_label("FOLDER"))
                    .child(chrome)
                    .into_any_element()
            }
            DialogMode::Clone => {
                let url_field = textbox("np-url", &state.url, "https://…", DialogField::Url);
                let parent_chrome = path_chrome(
                    "np-parent",
                    state.parent.text(),
                    Box::new(cx.listener(|this, _, window, cx| {
                        cx.stop_propagation();
                        this.pick_clone_parent_folder(window, cx);
                    })),
                );
                let name_field =
                    textbox("np-name", &state.name, "<derived from URL>", DialogField::Name);
                div()
                    .px_5()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(field_label("GIT URL"))
                    .child(url_field)
                    .child(field_label("PARENT FOLDER"))
                    .child(parent_chrome)
                    .child(field_label("FOLDER NAME"))
                    .child(name_field)
                    .into_any_element()
            }
        };

        let error_block = error_msg.map(|msg| {
            div()
                .px_5()
                .text_size(px(12.0))
                .text_color(danger)
                .child(msg)
        });

        // Footer caption — mirrors C# `FootMeta`. Differs by mode and
        // by busy state (the C# build replaces FootMeta with a busy
        // panel during clone — we render the same wording inline).
        let footer_caption: SharedString = if let Some(text) = busy_text.clone() {
            text
        } else {
            match mode {
                DialogMode::Existing => {
                    let path = if state.existing_path.is_empty() {
                        "…".to_string()
                    } else {
                        state.existing_path.clone()
                    };
                    format!("add project · {path}").into()
                }
                DialogMode::Clone => {
                    let url = if state.url.text().trim().is_empty() {
                        "…".to_string()
                    } else {
                        state.url.text().trim().to_string()
                    };
                    let name = if state.name.text().trim().is_empty() {
                        "…".to_string()
                    } else {
                        state.name.text().trim().to_string()
                    };
                    format!("git clone · {url} → {name}").into()
                }
            }
        };

        let footer_meta = div()
            .px_5()
            .text_size(px(11.0))
            .text_color(ink_muted)
            .truncate()
            .child(footer_caption);

        // Buttons.
        let cancel_btn = div()
            .id("np-cancel")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(ink_dim)
            .border_1()
            .border_color(divider)
            .rounded(px(6.0))
            .cursor_pointer()
            .hover(move |s| s.bg(frost).text_color(ink))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.cancel_new_project_dialog(cx);
                }),
            )
            .child("Cancel");

        let add_color = if valid { canvas } else { ink_ghost };
        let add_bg = if valid { accent } else { divider };
        let add_btn = {
            let mut btn = div()
                .id("np-add")
                .px_4()
                .py_2()
                .text_size(px(13.0))
                .text_color(add_color)
                .bg(add_bg)
                .rounded(px(6.0));
            if valid {
                btn = btn
                    .cursor_pointer()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            cx.stop_propagation();
                            this.submit_new_project_dialog(cx);
                        }),
                    );
            }
            btn.child(if busy { "Adding…" } else { "Add" })
        };

        let footer_buttons = div()
            .flex()
            .flex_row()
            .justify_end()
            .gap_2()
            .px_5()
            .pb_5()
            .child(cancel_btn)
            .child(add_btn);

        let mut card = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(520.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .pb_2()
            .track_focus(&focus_handle)
            .key_context("NewProjectDialog")
            .on_key_down(cx.listener(handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(mode_toggle)
            .child(body);
        if let Some(eb) = error_block {
            card = card.child(eb);
        }
        card = card.child(footer_meta).child(footer_buttons);

        let backdrop = div()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if !this.new_project_dialog().is_some_and(|s| s.busy) {
                        this.cancel_new_project_dialog(cx);
                    }
                }),
            )
            .child(card);

        Some(
            deferred(
                anchored()
                    .position(point(px(0.0), px(0.0)))
                    .child(backdrop),
            )
            .with_priority(10)
            .into_any_element(),
        )
    }
}

fn handle_key_down(
    sidebar: &mut Sidebar,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<Sidebar>,
) {
    let key = event.keystroke.key.as_str();
    cx.stop_propagation();

    let busy = sidebar
        .new_project_dialog()
        .map(|s| s.busy)
        .unwrap_or(false);
    let mode = sidebar
        .new_project_dialog()
        .map(|s| s.mode)
        .unwrap_or(DialogMode::Existing);

    match key {
        "escape" => {
            if !busy {
                sidebar.cancel_new_project_dialog(cx);
            }
            return;
        }
        "enter" => {
            sidebar.submit_new_project_dialog(cx);
            return;
        }
        "tab" => {
            if mode == DialogMode::Clone
                && let Some(state) = sidebar.new_project_dialog_mut()
            {
                state.focused_field = match state.focused_field {
                    DialogField::Url => DialogField::Parent,
                    DialogField::Parent => DialogField::Name,
                    DialogField::Name => DialogField::Url,
                };
                cx.notify();
            }
            return;
        }
        "backspace" => {
            let touched = sidebar
                .new_project_dialog_mut()
                .map(|s| s.backspace())
                .unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "delete" => {
            let touched = sidebar
                .new_project_dialog_mut()
                .map(|s| s.delete_forward())
                .unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "left" => {
            let touched = sidebar
                .new_project_dialog_mut()
                .map(|s| s.move_caret_left())
                .unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "right" => {
            let touched = sidebar
                .new_project_dialog_mut()
                .map(|s| s.move_caret_right())
                .unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "home" => {
            let touched = sidebar
                .new_project_dialog_mut()
                .map(|s| s.move_caret_home())
                .unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "end" => {
            let touched = sidebar
                .new_project_dialog_mut()
                .map(|s| s.move_caret_end())
                .unwrap_or(false);
            if touched {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        "space" => {
            let changed = sidebar
                .new_project_dialog_mut()
                .map(|s| s.insert_char(' '))
                .unwrap_or(false);
            if changed {
                sidebar.wake_text_blink(cx);
                cx.notify();
            }
            return;
        }
        _ => {}
    }

    let Some(key_char) = event.keystroke.key_char.as_deref() else {
        return;
    };
    if key_char.is_empty() {
        return;
    }
    let mut changed = false;
    if let Some(state) = sidebar.new_project_dialog_mut() {
        for ch in key_char.chars() {
            if !ch.is_control() && state.insert_char(ch) {
                changed = true;
            }
        }
    }
    if changed {
        sidebar.wake_text_blink(cx);
        cx.notify();
    }
}

/// Best-effort home-directory probe. Avoids pulling the `home` crate
/// just for this — `USERPROFILE` on Windows, `HOME` elsewhere covers
/// the realistic cases.
fn home_dir() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_valid_git_url_accepts_common_shapes() {
        assert!(is_valid_git_url("https://github.com/owner/repo"));
        assert!(is_valid_git_url("https://github.com/owner/repo.git"));
        assert!(is_valid_git_url("http://gitea.local/repo.git"));
        assert!(is_valid_git_url("ssh://git@host:22/owner/repo.git"));
        assert!(is_valid_git_url("git@github.com:owner/repo.git"));
    }

    #[test]
    fn is_valid_git_url_rejects_garbage() {
        assert!(!is_valid_git_url(""));
        assert!(!is_valid_git_url("   "));
        assert!(!is_valid_git_url("https://"));
        assert!(!is_valid_git_url("git@github.com"));
        assert!(!is_valid_git_url("github.com/owner/repo"));
        assert!(!is_valid_git_url("git@:owner/repo"));
    }

    #[test]
    fn derive_repo_name_strips_dot_git_and_takes_leaf() {
        assert_eq!(derive_repo_name("https://github.com/owner/repo.git"), "repo");
        assert_eq!(derive_repo_name("https://github.com/owner/repo"), "repo");
        assert_eq!(derive_repo_name("https://github.com/owner/repo/"), "repo");
        assert_eq!(derive_repo_name("git@github.com:owner/repo.git"), "repo");
        assert_eq!(derive_repo_name("ssh://git@host/owner/repo.GIT"), "repo");
    }

    #[test]
    fn derive_repo_name_replaces_invalid_chars_with_dashes() {
        // A hostile or just unusual URL must not let `*` or `?` into
        // the destination folder name.
        assert_eq!(derive_repo_name("https://h/owner/repo*?"), "repo--");
        assert_eq!(derive_repo_name(""), "");
    }

    #[test]
    fn default_clone_parent_uses_recent_project_parent_when_present() {
        // The recent-project parent obviously varies by host, so we
        // just exercise the priority order against the temp dir,
        // which is guaranteed to exist on every CI runner.
        let tmp = std::env::temp_dir();
        let inner = tmp.join("codescope-test-default-parent");
        let _ = std::fs::create_dir_all(&inner);
        let recent = inner.join("repo-x");
        let recent_str = recent.to_string_lossy().into_owned();
        let inner_str = inner.to_string_lossy().into_owned();
        let got = default_clone_parent(&[recent_str.as_str()], None);
        assert_eq!(got, inner_str);
        let _ = std::fs::remove_dir(&inner);
    }

    #[test]
    fn default_clone_parent_falls_back_to_home_when_no_projects() {
        let tmp = std::env::temp_dir();
        let got = default_clone_parent(&[], Some(&tmp));
        // Either `<tmp>/source/repos` exists (unlikely under temp) or
        // we fall back to the home dir itself. Both are acceptable.
        assert!(got == tmp.to_string_lossy() || got.contains("source"));
    }

    #[test]
    fn default_clone_parent_returns_existing_directory_when_home_missing() {
        let got = default_clone_parent(&[], None);
        // Must always be an existing directory so `cx.prompt_for_paths`
        // has a starting point.
        assert!(Path::new(&got).is_dir(), "{got} is not a directory");
    }

    #[test]
    fn name_auto_redrives_until_user_edits() {
        // We can't construct a real `FocusHandle` outside of gpui, so
        // we exercise the helper directly. The `_` field is just to
        // satisfy the signature — this isn't a public API.
        let mut url = String::new();
        let mut name = String::new();
        let mut name_auto = true;
        // Append URL chars; while name_auto, name follows.
        for ch in "https://h/owner/repo.git".chars() {
            url.push(ch);
            if name_auto {
                name = derive_repo_name(&url);
            }
        }
        assert_eq!(name, "repo");

        // User edits name → flag flips → further URL changes don't
        // overwrite.
        name.push('-');
        name.push('2');
        name_auto = false;
        for ch in "x".chars() {
            url.push(ch);
            if name_auto {
                name = derive_repo_name(&url);
            }
        }
        assert_eq!(name, "repo-2");
    }
}
