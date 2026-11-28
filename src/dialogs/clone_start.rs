//! Needs-you dialog for an item no project has a remote for (ADR-0027): clone its repository as
//! a new project. The next board read then finds one project for the item and starts its round.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use codescope_core::Theme;
use gpui::{
    AnyElement, App, AppContext as _, Context, FocusHandle, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, PathPromptOptions, SharedString,
    Styled, Window, anchored, deferred, div, point, px,
};

use crate::app::AppShell;
use crate::dialogs::new_project::default_clone_parent;
use crate::dialogs::pick_project::work_on;
use crate::text_field::{TextField, focused_caret_style, render_input_content};
use crate::theme;

/// A field of the dialog that takes typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloneField {
    Parent,
    Name,
}

pub struct CloneStartDialogState {
    pub focus_handle: FocusHandle,
    pub number: u32,
    pub repository: String,
    /// The column the item waits in.
    pub column: String,
    /// What starts after the clone: `research`, `implementation` or `a review`.
    pub work: &'static str,
    pub url: String,
    pub parent: TextField,
    pub name: TextField,
    pub focused: CloneField,
    /// Set while `git clone` runs.
    pub cloning: bool,
    pub error: Option<String>,
}

impl CloneStartDialogState {
    fn field_mut(&mut self, field: CloneField) -> &mut TextField {
        match field {
            CloneField::Parent => &mut self.parent,
            CloneField::Name => &mut self.name,
        }
    }
}

/// The URL to clone `owner/name` from, and the folder name it gets by default.
fn clone_source(repository: &str) -> (String, String) {
    let name = repository.rsplit('/').next().unwrap_or(repository);
    (format!("https://github.com/{repository}.git"), name.to_string())
}

/// `name`, or `name-2`, `name-3`, … when `parent` already has a folder of that name.
fn free_name(parent: &Path, name: &str, taken: impl Fn(&Path) -> bool) -> String {
    (1..100)
        .map(|n| if n == 1 { name.to_string() } else { format!("{name}-{n}") })
        .find(|candidate| !taken(&parent.join(candidate)))
        .unwrap_or_else(|| name.to_string())
}

/// The folder the clone goes into, or why it can't: `existing` are the projects' paths.
fn clone_target(parent: &str, name: &str, existing: &[&str]) -> Result<PathBuf, String> {
    let (parent, name) = (parent.trim(), name.trim());
    if parent.is_empty() {
        return Err("Pick the folder to clone into.".into());
    }
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\\']) {
        return Err("The name is a single folder name, such as notes-sync.".into());
    }
    let target = Path::new(parent).join(name);
    let spelled = target.to_string_lossy();
    let same = |path: &&str| {
        let trim = |p: &str| p.trim_end_matches(['/', '\\']).replace('\\', "/").to_lowercase();
        trim(path) == trim(&spelled)
    };
    if existing.iter().any(same) {
        return Err(format!("{spelled} is already a project."));
    }
    Ok(target)
}

impl AppShell {
    /// Open the dialog for an item whose repository no project has a remote for.
    pub fn open_clone_start_dialog(
        &mut self,
        item_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.clone_start_dialog.is_some() {
            return;
        }
        let Some(dialog) = self.clone_start_state(item_id, cx.focus_handle(), cx) else {
            return;
        };
        dialog.focus_handle.focus(window);
        // gpui hands the focus to the root's `track_focus` on the click that opened this.
        window.prevent_default();
        self.clone_start_dialog = Some(dialog);
        cx.notify();
    }

    fn clone_start_state(
        &self,
        item_id: &str,
        focus_handle: FocusHandle,
        cx: &App,
    ) -> Option<CloneStartDialogState> {
        let (settings, board) = self.settings.boards.iter().find_map(|settings| {
            let board = self.board.reads_by_url.get(&settings.url)?.last_good.as_ref()?;
            board.items.iter().any(|item| item.id == item_id).then_some((settings, board))
        })?;
        let item = board.items.iter().find(|item| item.id == item_id)?;
        let repository = item.repository.clone()?;
        let column = item.column_id.as_deref().and_then(|id| board.column(id))?;
        let (url, name) = clone_source(&repository);
        let projects = self.current_projects(cx);
        let paths: Vec<&str> = projects.projects.iter().map(|p| p.path.as_str()).collect();
        let home = codescope_core::paths::home_dir();
        let parent = default_clone_parent(&paths, home.as_deref());
        let name = free_name(Path::new(&parent), &name, Path::exists);
        Some(CloneStartDialogState {
            focus_handle,
            number: item.number?,
            repository,
            column: column.name.clone(),
            work: work_on(board, settings, item),
            url,
            parent: TextField::with_text(parent),
            name: TextField::with_text(name),
            focused: CloneField::Name,
            cloning: false,
            error: None,
        })
    }

    pub fn cancel_clone_start_dialog(&mut self, cx: &mut Context<Self>) {
        if self.clone_start_dialog.as_ref().is_some_and(|d| !d.cloning) {
            self.clone_start_dialog = None;
            cx.notify();
        }
    }

    pub fn submit_clone_start_dialog(&mut self, cx: &mut Context<Self>) {
        let projects = self.current_projects(cx);
        let paths: Vec<&str> = projects.projects.iter().map(|p| p.path.as_str()).collect();
        let Some(dialog) = self.clone_start_dialog.as_ref().filter(|d| !d.cloning) else {
            return;
        };
        let target = clone_target(dialog.parent.text(), dialog.name.text(), &paths);
        let Some(dialog) = self.clone_start_dialog.as_mut() else {
            return;
        };
        let target = match target {
            Ok(target) => target,
            Err(err) => {
                dialog.error = Some(err);
                cx.notify();
                return;
            }
        };
        dialog.cloning = true;
        dialog.error = None;
        cx.notify();
        let url = dialog.url.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_spawn(async move {
                    let parent = target.parent().unwrap_or(Path::new(""));
                    let name = target.file_name().unwrap_or_default().to_string_lossy();
                    codescope_core::git::clone_repo(&url, parent, &name)
                })
                .await;
            let _ = this.update(cx, |this, cx| this.finish_clone_start(result, cx));
        })
        .detach();
    }

    fn finish_clone_start(&mut self, result: anyhow::Result<PathBuf>, cx: &mut Context<Self>) {
        let Some(dialog) = self.clone_start_dialog.as_mut() else {
            return;
        };
        dialog.cloning = false;
        let target = match result {
            Ok(target) => target,
            Err(err) => {
                dialog.error = Some(format!("{err:#}"));
                cx.notify();
                return;
            }
        };
        let Some(dialog) = self.clone_start_dialog.take() else {
            return;
        };
        let path = target.to_string_lossy().into_owned();
        self.add_local_project(path, cx);
        let next = format!("#{} starts its {} at the next board read.", dialog.number, dialog.work);
        let title = format!("Cloned {}", dialog.repository);
        self.push_toast(crate::app::ToastKind::Ok, title, Some(next.into()), cx);
        self.refresh_boards(cx);
        cx.notify();
    }

    fn edit_clone_field(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut TextField) -> bool,
    ) {
        let Some(dialog) = self.clone_start_dialog.as_mut().filter(|d| !d.cloning) else {
            return;
        };
        let field = dialog.focused;
        if !edit(dialog.field_mut(field)) {
            return;
        }
        dialog.error = None;
        self.wake_text_blink(cx);
        cx.notify();
    }

    fn focus_clone_field(&mut self, field: CloneField, cx: &mut Context<Self>) {
        if let Some(dialog) = self.clone_start_dialog.as_mut() {
            dialog.focused = field;
            self.wake_text_blink(cx);
            cx.notify();
        }
    }

    fn pick_clone_start_parent(&mut self, cx: &mut Context<Self>) {
        if self.clone_start_dialog.as_ref().is_none_or(|d| d.cloning) {
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Pick the folder to clone into".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if let Some(dialog) = this.clone_start_dialog.as_mut() {
                    dialog.parent.set_text(path.to_string_lossy().into_owned());
                    dialog.focused = CloneField::Parent;
                    dialog.error = None;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    pub fn render_clone_start_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dialog = self.clone_start_dialog.as_ref()?;
        let viewport = window.viewport_size();
        let ink = theme::ink(theme);
        let muted = theme::ink_muted(theme);
        let faint = theme::text_faint();
        let divider = theme::divider(theme);
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);
        let frost = theme::frost_10(theme);
        let short_name = dialog.repository.rsplit('/').next().unwrap_or(&dialog.repository);

        let header = div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .px_6()
            .pt_5()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .text_size(px(11.0))
                    .font(theme::font_mono())
                    .child(div().text_color(theme::signal_review()).child("NEEDS YOU"))
                    .child(div().text_color(faint).child(format!(
                        "#{} · {} · in {}",
                        dialog.number, dialog.repository, dialog.column
                    ))),
            )
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(ink)
                    .child(format!("Clone {short_name} first")),
            )
            .child(div().text_size(px(13.0)).text_color(muted).child(format!(
                "No project points at {} yet. After the clone, {} of #{} starts on its own.",
                dialog.repository, dialog.work, dialog.number
            )));

        let label = |text: &'static str| {
            div()
                .text_size(px(13.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_color(ink)
                .child(text)
        };
        let input = |id: &'static str, field: CloneField, text: &TextField| {
            let focused = dialog.focused == field;
            div()
                .id(id)
                .flex_1()
                .min_w(px(0.0))
                .px(px(10.0))
                .h(px(34.0))
                .flex()
                .items_center()
                .bg(canvas)
                .border_1()
                .border_color(if focused { accent } else { divider })
                .rounded(px(6.0))
                .text_size(px(12.0))
                .font(theme::font_mono())
                .text_color(ink)
                .cursor_text()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        cx.stop_propagation();
                        this.focus_clone_field(field, cx);
                        if let Some(dialog) = this.clone_start_dialog.as_mut() {
                            let text = dialog.field_mut(field);
                            if let Some(index) = text.index_for_window_point(event.position) {
                                text.set_caret(index);
                            }
                        }
                    }),
                )
                .child(if focused {
                    render_input_content(
                        text,
                        SharedString::default(),
                        focused_caret_style(theme, self.text_blink_phase),
                    )
                    .into_any_element()
                } else {
                    div().truncate().child(text.text().to_string()).into_any_element()
                })
        };
        let repository = div()
            .px(px(10.0))
            .h(px(34.0))
            .flex()
            .items_center()
            .bg(theme::elevated(theme))
            .border_1()
            .border_color(divider)
            .rounded(px(6.0))
            .text_size(px(12.0))
            .font(theme::font_mono())
            .text_color(muted)
            .child(dialog.url.clone());
        let browse = div()
            .id("clone-start-browse")
            .px(px(10.0))
            .h(px(34.0))
            .flex()
            .items_center()
            .text_size(px(12.0))
            .text_color(ink)
            .border_1()
            .border_color(divider)
            .rounded(px(6.0))
            .flex_shrink_0()
            .cursor_pointer()
            .hover(move |s| s.bg(frost))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.pick_clone_start_parent(cx);
                }),
            )
            .child("Browse");
        let target = Path::new(dialog.parent.text().trim()).join(dialog.name.text().trim());
        let fields = div()
            .flex()
            .flex_col()
            .gap(px(14.0))
            .px_6()
            .child(
                div().flex().flex_col().gap(px(6.0)).child(label("Repository")).child(repository),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap(px(12.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(label("Parent folder"))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap(px(6.0))
                                    .child(input(
                                        "clone-start-parent",
                                        CloneField::Parent,
                                        &dialog.parent,
                                    ))
                                    .child(browse),
                            ),
                    )
                    .child(
                        div()
                            .w(px(160.0))
                            .flex_shrink_0()
                            .flex()
                            .flex_col()
                            .gap(px(6.0))
                            .child(label("Name"))
                            .child(input("clone-start-name", CloneField::Name, &dialog.name)),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .font(theme::font_mono())
                    .text_color(faint)
                    .child(format!("→ {}", target.display())),
            );

        let message: Option<(String, gpui::Hsla)> = match (&dialog.error, dialog.cloning) {
            (_, true) => Some((format!("Cloning {short_name}…"), muted)),
            (Some(err), false) => Some((err.clone(), theme::danger())),
            (None, false) => None,
        };

        let button = |id: &'static str, text: &'static str, primary: bool| {
            let enabled = !dialog.cloning;
            let base = div()
                .id(id)
                .px(px(14.0))
                .py_2()
                .text_size(px(13.0))
                .rounded(px(6.0))
                .flex_shrink_0()
                .child(text);
            let base = match (primary, enabled) {
                (true, true) => {
                    base.font_weight(gpui::FontWeight::SEMIBOLD).text_color(canvas).bg(accent)
                }
                (true, false) => base.text_color(theme::ink_ghost(theme)).bg(divider),
                (false, true) => base
                    .text_color(ink)
                    .border_1()
                    .border_color(divider)
                    .hover(move |s| s.bg(frost)),
                (false, false) => {
                    base.text_color(theme::ink_ghost(theme)).border_1().border_color(divider)
                }
            };
            if enabled { base.cursor_pointer() } else { base }
        };
        let later = button("clone-start-later", "Not now", false)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.cancel_clone_start_dialog(cx);
                }),
            );
        let start = button("clone-start-ok", "Clone and start", true).on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                this.submit_clone_start_dialog(cx);
            }),
        );
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_6()
            .py(px(14.0))
            .border_t_1()
            .border_color(divider)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(12.0))
                    .text_color(faint)
                    .child(format!("Until then the item waits in {}.", dialog.column)),
            )
            .child(later)
            .child(start);

        let card = div()
            .flex()
            .flex_col()
            .gap_3()
            .w(px(560.0))
            .bg(theme::elevated(theme))
            .border_1()
            .border_color(divider)
            .rounded_lg()
            .shadow_lg()
            .track_focus(&dialog.focus_handle)
            .key_context("CloneStartDialog")
            .on_key_down(cx.listener(handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(fields)
            .children(message.map(|(text, color)| {
                div().px_6().text_size(px(12.0)).text_color(color).child(text)
            }))
            .child(footer);

        let backdrop = div()
            .w(viewport.width)
            .h(viewport.height)
            .flex()
            .items_center()
            .justify_center()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.55))
            .occlude()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.cancel_clone_start_dialog(cx)),
            )
            .child(card);

        Some(
            deferred(anchored().position(point(px(0.0), px(0.0))).child(backdrop))
                .with_priority(10)
                .into_any_element(),
        )
    }
}

fn handle_key_down(
    shell: &mut AppShell,
    event: &KeyDownEvent,
    _window: &mut Window,
    cx: &mut Context<AppShell>,
) {
    cx.stop_propagation();
    let keystroke = &event.keystroke;
    if crate::text_field::is_paste_chord(keystroke) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            shell.edit_clone_field(cx, |field| field.insert_str(text.trim()));
        }
        return;
    }
    match keystroke.key.as_str() {
        "escape" => shell.cancel_clone_start_dialog(cx),
        "enter" => shell.submit_clone_start_dialog(cx),
        "tab" => {
            let next = match shell.clone_start_dialog.as_ref().map(|d| d.focused) {
                Some(CloneField::Parent) => CloneField::Name,
                _ => CloneField::Parent,
            };
            shell.focus_clone_field(next, cx);
        }
        "backspace" => shell.edit_clone_field(cx, TextField::backspace),
        "delete" => shell.edit_clone_field(cx, TextField::delete_forward),
        "left" => shell.edit_clone_field(cx, TextField::move_left),
        "right" => shell.edit_clone_field(cx, TextField::move_right),
        "home" => shell.edit_clone_field(cx, TextField::move_home),
        "end" => shell.edit_clone_field(cx, TextField::move_end),
        _ => {
            let typed: String = keystroke
                .key_char
                .as_deref()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            if !typed.is_empty() {
                shell.edit_clone_field(cx, |field| field.insert_str(&typed));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_clones_from_github_into_a_folder_of_its_name() {
        let (url, name) = clone_source("maui1911/notes-sync");
        assert_eq!(url, "https://github.com/maui1911/notes-sync.git");
        assert_eq!(name, "notes-sync");
    }

    #[test]
    fn the_name_offered_is_one_no_folder_in_the_parent_has_yet() {
        let parent = Path::new("D:\\Dev");
        let taken = |path: &Path| path.ends_with("CodeScope") || path.ends_with("CodeScope-2");
        assert_eq!(free_name(parent, "CodeScope", taken), "CodeScope-3");
        assert_eq!(free_name(parent, "notes-sync", taken), "notes-sync");
    }

    #[test]
    fn the_clone_goes_into_a_new_folder_under_the_parent() {
        let target = clone_target(" D:\\Dev ", " notes-sync ", &["D:\\Dev\\CodeScope"]).unwrap();
        assert_eq!(target, Path::new("D:\\Dev").join("notes-sync"));
    }

    #[test]
    fn the_clone_needs_a_parent_a_plain_name_and_a_folder_no_project_has() {
        let no_parent = clone_target("", "notes-sync", &[]).unwrap_err();
        assert_eq!(no_parent, "Pick the folder to clone into.");
        for name in ["", "..", "a/b", "a\\b"] {
            let err = clone_target("D:\\Dev", name, &[]).unwrap_err();
            assert!(err.starts_with("The name is a single folder name"), "{name}: {err}");
        }
        let existing = Path::new("D:\\Dev").join("Notes-Sync");
        let existing = existing.to_string_lossy();
        let err = clone_target("D:\\Dev", "notes-sync", &[existing.as_ref()]).unwrap_err();
        assert!(err.ends_with("is already a project."), "{err}");
    }
}
