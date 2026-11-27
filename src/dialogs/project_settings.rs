//! Project settings dialog: what a project sets for the board dispatches on it (ADR-0027), kept
//! on the project in `projects.json`. Every field left empty keeps the default.

use std::sync::Arc;

use codescope_core::board::dispatch::Round;
use codescope_core::board::dispatch_settings::{
    DispatchSettings, REPAIR_SKILL, SkillNames, is_skill_name,
};
use codescope_core::board::setup::SETUP_SCRIPT;
use codescope_core::{ProjectsConfig, Theme};
use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, SharedString, Styled, Window, anchored, deferred, div, point,
    px,
};

use crate::app::AppShell;
use crate::text_field::{TextField, focused_caret_style, render_input_content};
use crate::theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Input {
    SetupCommand,
    Research,
    Implement,
    AddressReview,
    Repair,
}

const INPUTS: [Input; 5] = [
    Input::SetupCommand,
    Input::Research,
    Input::Implement,
    Input::AddressReview,
    Input::Repair,
];

impl Input {
    fn index(self) -> usize {
        INPUTS.iter().position(|input| *input == self).unwrap_or_default()
    }

    fn after(self, step: isize) -> Input {
        let count = INPUTS.len() as isize;
        INPUTS[(self.index() as isize + step).rem_euclid(count) as usize]
    }

    fn label(self) -> &'static str {
        match self {
            Input::SetupCommand => "Setup command",
            Input::Research => "Research",
            Input::Implement => "Implementation",
            Input::AddressReview => "Review",
            Input::Repair => "Repair",
        }
    }

    fn default_skill(self) -> &'static str {
        match self {
            Input::SetupCommand => "",
            Input::Research => Round::Research.skill(),
            Input::Implement => Round::Implementation.skill(),
            Input::AddressReview => Round::Review.skill(),
            Input::Repair => REPAIR_SKILL,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Input::SetupCommand => "project-settings-setup",
            Input::Research => "project-settings-research",
            Input::Implement => "project-settings-implement",
            Input::AddressReview => "project-settings-review",
            Input::Repair => "project-settings-repair",
        }
    }
}

pub(crate) struct ProjectSettingsDialogState {
    focus_handle: FocusHandle,
    project_id: String,
    project_name: String,
    focused: Input,
    fields: [TextField; 5],
    error: Option<String>,
}

impl ProjectSettingsDialogState {
    fn field(&self, input: Input) -> &TextField {
        &self.fields[input.index()]
    }

    fn field_mut(&mut self, input: Input) -> &mut TextField {
        &mut self.fields[input.index()]
    }
}

fn fields_of(settings: &DispatchSettings) -> [TextField; 5] {
    let text = |value: &Option<String>| TextField::with_text(value.clone().unwrap_or_default());
    let skills = &settings.skills;
    [
        text(&settings.setup_command),
        text(&skills.research),
        text(&skills.implement),
        text(&skills.address_review),
        text(&skills.repair),
    ]
}

fn skill_name(input: Input, text: &str) -> Result<Option<String>, String> {
    let name = text.trim();
    if name.is_empty() || name == input.default_skill() {
        return Ok(None);
    }
    if !is_skill_name(name) {
        let label = input.label();
        return Err(format!(
            "\u{201c}{name}\u{201d} can't be the {label} skill: a skill's name has lowercase \
             letters, digits and single hyphens, like fix-ci."
        ));
    }
    Ok(Some(name.to_string()))
}

fn read_settings(texts: [&str; 5]) -> Result<DispatchSettings, String> {
    let [command, research, implement, address_review, repair] = texts;
    let command = command.trim();
    Ok(DispatchSettings {
        skills: SkillNames {
            research: skill_name(Input::Research, research)?,
            implement: skill_name(Input::Implement, implement)?,
            address_review: skill_name(Input::AddressReview, address_review)?,
            repair: skill_name(Input::Repair, repair)?,
        },
        setup_command: (!command.is_empty()).then(|| command.to_string()),
    })
}

impl AppShell {
    pub(crate) fn open_project_settings_dialog(
        &mut self,
        project_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.project_settings_dialog.is_some() {
            return;
        }
        let Some(project) = self.projects.project(project_id).cloned() else {
            return;
        };
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        self.project_settings_dialog = Some(ProjectSettingsDialogState {
            focus_handle,
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            focused: Input::SetupCommand,
            fields: fields_of(&project.dispatch),
            error: None,
        });
        cx.notify();
    }

    fn cancel_project_settings_dialog(&mut self, cx: &mut Context<Self>) {
        if self.project_settings_dialog.take().is_some() {
            cx.notify();
        }
    }

    fn submit_project_settings_dialog(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.project_settings_dialog.as_mut() else {
            return;
        };
        let texts = INPUTS.map(|input| state.field(input).text());
        let settings = match read_settings(texts) {
            Ok(settings) => settings,
            Err(err) => {
                state.error = Some(err);
                cx.notify();
                return;
            }
        };
        let project_id = state.project_id.clone();
        let saved = ProjectsConfig::reload_and_edit(self.paths_ref().as_ref(), |config| {
            let Some(project) = config.projects.iter_mut().find(|p| p.id == project_id) else {
                return Ok(false);
            };
            let changed = project.dispatch != settings;
            project.dispatch = settings;
            Ok(changed)
        });
        match saved {
            Ok((next, _)) => {
                self.project_settings_dialog = None;
                self.projects = next;
                self.mirror_projects_to_sidebar(cx);
            }
            Err(err) => {
                if let Some(state) = self.project_settings_dialog.as_mut() {
                    state.error = Some(format!("Couldn't save projects.json: {err:#}"));
                }
            }
        }
        cx.notify();
    }

    fn focus_project_settings_input(&mut self, input: Input, cx: &mut Context<Self>) {
        if let Some(state) = self.project_settings_dialog.as_mut() {
            state.focused = input;
            self.wake_text_blink(cx);
            cx.notify();
        }
    }

    fn edit_project_settings_input(
        &mut self,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut TextField) -> bool,
    ) {
        let Some(state) = self.project_settings_dialog.as_mut() else {
            return;
        };
        let focused = state.focused;
        if !edit(state.field_mut(focused)) {
            return;
        }
        state.error = None;
        self.wake_text_blink(cx);
        cx.notify();
    }

    pub(crate) fn render_project_settings_dialog(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.project_settings_dialog.as_ref()?;
        let viewport = window.viewport_size();

        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_muted = theme::ink_muted(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let frost = theme::frost_10(theme);
        let accent = theme::accent(theme);
        let canvas = theme::canvas(theme);
        let blink_phase = self.text_blink_phase;

        let input_box = |input: Input| {
            let focused = state.focused == input;
            let mut style = focused_caret_style(theme, blink_phase);
            style.show_caret = focused && blink_phase;
            let placeholder = match input {
                Input::SetupCommand => "Nothing runs",
                _ => input.default_skill(),
            };
            div()
                .id(input.id())
                .flex_1()
                .px_3()
                .h(px(32.0))
                .bg(canvas)
                .border_1()
                .border_color(if focused { accent } else { divider })
                .rounded(px(6.0))
                .text_size(px(13.0))
                .font(theme::font_mono())
                .flex()
                .items_center()
                .cursor_text()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        cx.stop_propagation();
                        window.prevent_default();
                        if let Some(state) = this.project_settings_dialog.as_mut() {
                            let field = state.field_mut(input);
                            if let Some(index) = field.index_for_window_point(event.position) {
                                field.set_caret(index);
                            }
                        }
                        this.focus_project_settings_input(input, cx);
                    }),
                )
                .child(render_input_content(
                    state.field(input),
                    SharedString::from(placeholder),
                    style,
                ))
        };

        let hint = |text: String| div().text_size(px(11.0)).text_color(ink_ghost).child(text);
        let heading = |text: &'static str| {
            div()
                .pt_2()
                .text_size(px(11.0))
                .text_color(accent)
                .child(text)
        };

        let header = div()
            .flex()
            .flex_col()
            .gap_1()
            .px_5()
            .pt_5()
            .child(div().text_size(px(11.0)).text_color(accent).child("PROJECT"))
            .child(div().text_size(px(20.0)).text_color(ink).child("Project settings"))
            .child(div().text_size(px(12.0)).text_color(ink_muted).child(format!(
                "How dispatches from a board run in {}. A field left empty keeps the default.",
                state.project_name
            )));

        let shell = if cfg!(windows) { "PowerShell" } else { "sh -e" };
        let setup = div()
            .flex()
            .flex_col()
            .gap_1()
            .child(heading("SETUP"))
            .child(input_box(Input::SetupCommand))
            .child(hint(format!(
                "Runs with {shell} in each new worktree whose default branch has no \
                 {SETUP_SCRIPT}, before the agent starts."
            )));

        let skill_row = |input: Input| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .w(px(110.0))
                        .text_size(px(12.0))
                        .text_color(ink_dim)
                        .child(input.label()),
                )
                .child(input_box(input))
        };
        let skills = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(heading("SKILLS"))
            .child(skill_row(Input::Research))
            .child(skill_row(Input::Implement))
            .child(skill_row(Input::AddressReview))
            .child(skill_row(Input::Repair))
            .child(hint(
                "The skill each round tells the agent to use. It finds it in the repository's \
                 skills or in your own; without one, CodeScope's own prompt runs."
                    .to_string(),
            ));

        let body = div()
            .px_5()
            .flex()
            .flex_col()
            .gap_3()
            .child(setup)
            .child(skills);

        let cancel_btn = div()
            .id("project-settings-cancel")
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
                    this.cancel_project_settings_dialog(cx);
                }),
            )
            .child("Cancel");

        let save_btn = div()
            .id("project-settings-save")
            .px_4()
            .py_2()
            .text_size(px(13.0))
            .text_color(canvas)
            .bg(accent)
            .rounded(px(6.0))
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    this.submit_project_settings_dialog(cx);
                }),
            )
            .child("Save");

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
            .pb_2()
            .track_focus(&state.focus_handle)
            .key_context("ProjectSettingsDialog")
            .on_key_down(cx.listener(handle_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .child(header)
            .child(body)
            .children(state.error.clone().map(|err| {
                div()
                    .px_5()
                    .text_size(px(12.0))
                    .text_color(theme::danger())
                    .child(err)
            }))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .justify_end()
                    .gap_2()
                    .px_5()
                    .pb_5()
                    .child(cancel_btn)
                    .child(save_btn),
            );

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
                cx.listener(|this, _, _, cx| this.cancel_project_settings_dialog(cx)),
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
            let line = text.lines().next().unwrap_or_default().trim().to_string();
            shell.edit_project_settings_input(cx, |field| field.insert_str(&line));
        }
        return;
    }
    match keystroke.key.as_str() {
        "escape" => shell.cancel_project_settings_dialog(cx),
        "enter" => shell.submit_project_settings_dialog(cx),
        "tab" => {
            let step = if keystroke.modifiers.shift { -1 } else { 1 };
            if let Some(focused) = shell.project_settings_dialog.as_ref().map(|s| s.focused) {
                shell.focus_project_settings_input(focused.after(step), cx);
            }
        }
        "backspace" => shell.edit_project_settings_input(cx, TextField::backspace),
        "delete" => shell.edit_project_settings_input(cx, TextField::delete_forward),
        "left" => shell.edit_project_settings_input(cx, TextField::move_left),
        "right" => shell.edit_project_settings_input(cx, TextField::move_right),
        "home" => shell.edit_project_settings_input(cx, TextField::move_home),
        "end" => shell.edit_project_settings_input(cx, TextField::move_end),
        _ => {
            let typed: String = keystroke
                .key_char
                .as_deref()
                .unwrap_or_default()
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            if !typed.is_empty() {
                shell.edit_project_settings_input(cx, |field| field.insert_str(&typed));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_form_sets_nothing() {
        let settings = read_settings(["", " ", "", "", ""]).unwrap();
        assert!(settings.is_empty());
    }

    #[test]
    fn a_command_and_names_are_kept_without_the_spaces_around_them() {
        let settings = read_settings([" npm ci ", "dig", "", " answer-review", "fix-ci "]).unwrap();
        assert_eq!(settings.setup_command.as_deref(), Some("npm ci"));
        assert_eq!(settings.skills.research.as_deref(), Some("dig"));
        assert_eq!(settings.skills.implement, None);
        assert_eq!(settings.skills.address_review.as_deref(), Some("answer-review"));
        assert_eq!(settings.skills.repair.as_deref(), Some("fix-ci"));
    }

    #[test]
    fn a_default_name_typed_out_sets_nothing() {
        let settings = read_settings(["", "research", "implement", "address-review", "repair"]);
        assert!(settings.unwrap().is_empty());
    }

    #[test]
    fn a_name_that_can_t_name_a_skill_says_which_and_why() {
        let err = read_settings(["", "", "", "", "Fix CI"]).unwrap_err();
        assert_eq!(
            err,
            "\u{201c}Fix CI\u{201d} can't be the Repair skill: a skill's name has lowercase \
             letters, digits and single hyphens, like fix-ci."
        );
    }

    #[test]
    fn the_settings_open_as_they_were_saved() {
        let settings = DispatchSettings {
            skills: SkillNames {
                implement: Some("build-it".into()),
                ..SkillNames::default()
            },
            setup_command: Some("npm ci".into()),
        };
        let fields = fields_of(&settings);
        let texts = INPUTS.map(|input| fields[input.index()].text());
        assert_eq!(texts, ["npm ci", "", "build-it", "", ""]);
        assert_eq!(read_settings(texts).unwrap(), settings);
    }

    #[test]
    fn tab_goes_round_the_fields_both_ways() {
        assert_eq!(Input::SetupCommand.after(1), Input::Research);
        assert_eq!(Input::Repair.after(1), Input::SetupCommand);
        assert_eq!(Input::SetupCommand.after(-1), Input::Repair);
    }
}
