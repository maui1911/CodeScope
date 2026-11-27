//! Project settings dialog: what a project sets for the board dispatches on it (ADR-0027), kept
//! on the project in `projects.json`. Every field left empty keeps the default.

use std::sync::Arc;

use codescope_core::board::dispatch::Round;
use codescope_core::board::dispatch_settings::{
    DispatchSettings, PhaseAgents, REPAIR_SKILL, SkillNames, is_skill_name,
};
use codescope_core::board::setup::SETUP_SCRIPT;
use codescope_core::{ProjectsConfig, Theme};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    Context, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent, MouseButton,
    MouseDownEvent, ParentElement, SharedString, Styled, Window, anchored, deferred, div, point,
    px,
};

use crate::app::{AppShell, ToastKind};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Research,
    Implementation,
}

impl Phase {
    fn label(self) -> &'static str {
        match self {
            Phase::Research => "Research",
            Phase::Implementation => "Implementation",
        }
    }

    fn chip_id(self) -> &'static str {
        match self {
            Phase::Research => "project-settings-research-agent",
            Phase::Implementation => "project-settings-implementation-agent",
        }
    }

    fn agent(self, agents: &PhaseAgents) -> Option<&str> {
        match self {
            Phase::Research => agents.research.as_deref(),
            Phase::Implementation => agents.implementation.as_deref(),
        }
    }

    fn set_agent(self, agents: &mut PhaseAgents, id: Option<String>) {
        match self {
            Phase::Research => agents.research = id,
            Phase::Implementation => agents.implementation = id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentChip {
    id: Option<String>,
    label: String,
    chosen: bool,
}

fn agent_chips(
    set: Option<&str>,
    choices: &[(String, String)],
    own: Option<&str>,
) -> Vec<AgentChip> {
    let set = set.filter(|id| !id.is_empty());
    let own = own.map_or("Project's agent".to_string(), |name| format!("Project's agent ({name})"));
    let mut chips = vec![AgentChip {
        id: None,
        label: own,
        chosen: set.is_none(),
    }];
    chips.extend(choices.iter().map(|(id, name)| AgentChip {
        id: Some(id.clone()),
        label: name.clone(),
        chosen: set == Some(id.as_str()),
    }));
    if let Some(set) = set.filter(|set| choices.iter().all(|(id, _)| id != set)) {
        chips.push(AgentChip {
            id: Some(set.to_string()),
            label: format!("{set} (unavailable)"),
            chosen: true,
        });
    }
    chips
}

pub(crate) struct ProjectSettingsDialogState {
    focus_handle: FocusHandle,
    project_id: String,
    project_name: String,
    focused: Input,
    fields: [TextField; 5],
    stored_command: Option<String>,
    agents: PhaseAgents,
    agent_choices: Vec<(String, String)>,
    own_agent: Option<String>,
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

fn one_line(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    lines.join("; ")
}

fn pasted_line(input: Input, text: &str) -> Result<String, String> {
    let mut lines = text.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or_default().to_string();
    if lines.next().is_none() {
        return Ok(first);
    }
    Err(match input {
        Input::SetupCommand => format!(
            "The setup command is one line: separate commands with ; or put them in \
             {SETUP_SCRIPT}."
        ),
        _ => "A skill's name is one line.".to_string(),
    })
}

fn fields_of(settings: &DispatchSettings) -> [TextField; 5] {
    let text = |value: &Option<String>| {
        TextField::with_text(value.as_deref().map(one_line).unwrap_or_default())
    };
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

fn read_settings(
    texts: [&str; 5],
    stored_command: Option<&str>,
) -> Result<DispatchSettings, String> {
    let [command, research, implement, address_review, repair] = texts;
    let command = match stored_command {
        Some(stored) if command == one_line(stored) => stored.trim(),
        _ => command.trim(),
    };
    Ok(DispatchSettings {
        skills: SkillNames {
            research: skill_name(Input::Research, research)?,
            implement: skill_name(Input::Implement, implement)?,
            address_review: skill_name(Input::AddressReview, address_review)?,
            repair: skill_name(Input::Repair, repair)?,
        },
        setup_command: (!command.is_empty()).then(|| command.to_string()),
        ..DispatchSettings::default()
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
        match ProjectsConfig::load(self.paths_ref()) {
            Ok(projects) => self.projects = projects,
            Err(err) => {
                let detail = format!("Couldn't read projects.json: {err:#}");
                self.push_toast(
                    ToastKind::Err,
                    "Can't open project settings",
                    Some(detail.into()),
                    cx,
                );
                return;
            }
        }
        let Some(project) = self.projects.project(project_id).cloned() else {
            return;
        };
        let (agent_choices, own_agent) = self.round_agents(&project);
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        self.project_settings_dialog = Some(ProjectSettingsDialogState {
            focus_handle,
            project_id: project.id.clone(),
            project_name: project.name.clone(),
            focused: Input::SetupCommand,
            fields: fields_of(&project.dispatch),
            stored_command: project.dispatch.setup_command.clone(),
            agents: project.dispatch.agents.clone(),
            agent_choices,
            own_agent,
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
        let settings = match read_settings(texts, state.stored_command.as_deref()) {
            Ok(settings) => DispatchSettings {
                agents: state.agents.clone(),
                ..settings
            },
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

    fn choose_round_agent(&mut self, phase: Phase, id: Option<String>, cx: &mut Context<Self>) {
        if let Some(state) = self.project_settings_dialog.as_mut() {
            phase.set_agent(&mut state.agents, id);
            state.error = None;
            cx.notify();
        }
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

        let agent_row = |phase: Phase| {
            let chips = agent_chips(
                phase.agent(&state.agents),
                &state.agent_choices,
                state.own_agent.as_deref(),
            );
            let chips = chips.into_iter().enumerate().map(|(index, chip)| {
                let AgentChip { id, label, chosen } = chip;
                div()
                    .id((phase.chip_id(), index))
                    .px_2()
                    .py(px(3.0))
                    .text_size(px(12.0))
                    .rounded(px(5.0))
                    .border_1()
                    .border_color(if chosen { accent } else { divider })
                    .text_color(if chosen { ink } else { ink_dim })
                    .when(chosen, |chip| chip.bg(frost))
                    .cursor_pointer()
                    .hover(move |style| style.text_color(ink))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.choose_round_agent(phase, id.clone(), cx);
                        }),
                    )
                    .child(label)
            });
            div()
                .flex()
                .flex_row()
                .items_start()
                .gap_3()
                .child(
                    div()
                        .w(px(110.0))
                        .pt(px(3.0))
                        .text_size(px(12.0))
                        .text_color(ink_dim)
                        .child(phase.label()),
                )
                .child(div().flex_1().flex().flex_row().flex_wrap().gap_1().children(chips))
        };
        let agents = div()
            .flex()
            .flex_col()
            .gap_2()
            .child(heading("AGENTS"))
            .child(agent_row(Phase::Research))
            .child(agent_row(Phase::Implementation))
            .child(hint(
                "Implementation also runs the review rounds and the repairs. An agent other \
                 than research's starts a new session in the same worktree, from the findings."
                    .to_string(),
            ));

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
            .child(agents)
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
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        let Some(state) = shell.project_settings_dialog.as_mut() else {
            return;
        };
        match pasted_line(state.focused, &text) {
            Ok(line) => shell.edit_project_settings_input(cx, |field| field.insert_str(&line)),
            Err(err) => {
                state.error = Some(err);
                cx.notify();
            }
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
        let settings = read_settings(["", " ", "", "", ""], None).unwrap();
        assert!(settings.is_empty());
    }

    #[test]
    fn a_command_and_names_are_kept_without_the_spaces_around_them() {
        let texts = [" npm ci ", "dig", "", " answer-review", "fix-ci "];
        let settings = read_settings(texts, None).unwrap();
        assert_eq!(settings.setup_command.as_deref(), Some("npm ci"));
        assert_eq!(settings.skills.research.as_deref(), Some("dig"));
        assert_eq!(settings.skills.implement, None);
        assert_eq!(settings.skills.address_review.as_deref(), Some("answer-review"));
        assert_eq!(settings.skills.repair.as_deref(), Some("fix-ci"));
    }

    #[test]
    fn a_default_name_typed_out_sets_nothing() {
        let texts = ["", "research", "implement", "address-review", "repair"];
        assert!(read_settings(texts, None).unwrap().is_empty());
    }

    #[test]
    fn a_name_that_can_t_name_a_skill_says_which_and_why() {
        let err = read_settings(["", "", "", "", "Fix CI"], None).unwrap_err();
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
            ..DispatchSettings::default()
        };
        let fields = fields_of(&settings);
        let texts = INPUTS.map(|input| fields[input.index()].text());
        assert_eq!(texts, ["npm ci", "", "build-it", "", ""]);
        assert_eq!(read_settings(texts, None).unwrap(), settings);
    }

    #[test]
    fn a_command_of_several_lines_shows_on_one_line_and_stays_as_it_was() {
        let stored = "npm ci\r\n\n  npm run build\n";
        let settings = DispatchSettings {
            setup_command: Some(stored.into()),
            ..DispatchSettings::default()
        };
        let fields = fields_of(&settings);
        let shown = fields[Input::SetupCommand.index()].text();
        assert_eq!(shown, "npm ci; npm run build");
        let kept = read_settings([shown, "", "", "", ""], Some(stored)).unwrap();
        assert_eq!(kept.setup_command.as_deref(), Some(stored.trim()));
        let changed = read_settings(["npm ci", "", "", "", ""], Some(stored)).unwrap();
        assert_eq!(changed.setup_command.as_deref(), Some("npm ci"));
        let cleared = read_settings(["", "", "", "", ""], Some(stored)).unwrap();
        assert_eq!(cleared.setup_command, None);
    }

    #[test]
    fn a_paste_takes_one_line() {
        assert_eq!(pasted_line(Input::Research, "dig\r\n"), Ok("dig".to_string()));
        assert_eq!(pasted_line(Input::SetupCommand, "\n npm ci \n"), Ok("npm ci".to_string()));
        let err = pasted_line(Input::SetupCommand, "npm ci\nnpm run build").unwrap_err();
        assert!(err.starts_with("The setup command is one line"), "{err}");
        assert!(pasted_line(Input::Repair, "fix\nci").is_err());
    }

    #[test]
    fn the_project_s_agent_is_chosen_until_a_phase_names_another() {
        let choices = [
            ("claude-code".to_string(), "Claude Code".to_string()),
            ("codex".to_string(), "Codex".to_string()),
        ];
        let chosen = |chips: Vec<AgentChip>| {
            let labels = chips.iter().map(|chip| chip.label.as_str());
            let chosen: Vec<_> = chips.iter().filter(|chip| chip.chosen).collect();
            assert_eq!(chosen.len(), 1, "{:?}", labels.collect::<Vec<_>>());
            chosen[0].clone()
        };
        let unset = chosen(agent_chips(None, &choices, Some("Claude Code")));
        assert_eq!(unset.id, None);
        assert_eq!(unset.label, "Project's agent (Claude Code)");
        assert_eq!(chosen(agent_chips(Some(""), &choices, None)).id, None);
        let codex = chosen(agent_chips(Some("codex"), &choices, None));
        assert_eq!((codex.id.as_deref(), codex.label.as_str()), (Some("codex"), "Codex"));
    }

    #[test]
    fn an_agent_the_list_lacks_shows_as_unavailable() {
        let choices = [("codex".to_string(), "Codex".to_string())];
        let chips = agent_chips(Some("opencode"), &choices, None);
        let labels: Vec<_> = chips.iter().map(|chip| chip.label.as_str()).collect();
        assert_eq!(labels, ["Project's agent", "Codex", "opencode (unavailable)"]);
        assert!(chips[2].chosen);
    }

    #[test]
    fn tab_goes_round_the_fields_both_ways() {
        assert_eq!(Input::SetupCommand.after(1), Input::Research);
        assert_eq!(Input::Repair.after(1), Input::SetupCommand);
        assert_eq!(Input::SetupCommand.after(-1), Input::Repair);
    }
}
