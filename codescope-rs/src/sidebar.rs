//! Left-rail PROJECTS sidebar.
//!
//! Single-purpose view: lists every entry in the loaded
//! [`codescope_core::ProjectsConfig`] and lets the user select one.
//! The `+` button kicks off a directory picker and, on success,
//! appends a new project and persists `projects.json`. Removal /
//! editing are still TODO and live behind right-click + a command
//! palette in the C# build.
//!
//! Layout (240 px wide):
//!
//! ```text
//! ┌───────────────┐
//! │ PROJECTS    + │ ← heading + add (file picker)
//! ├───────────────┤
//! │ filter…       │ ← (placeholder, wired later)
//! ├───────────────┤
//! │ ▍ project A   │ ← active = accent rail + frost bg
//! │   project B   │
//! │   project C   │
//! └───────────────┘
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use codescope_core::{
    AppPaths, LayoutState, Project, ProjectsConfig, Session, Theme, Worktree, git,
};
use gpui::{
    AnyElement, Context, EventEmitter, InteractiveElement, IntoElement, MouseButton,
    ParentElement, PathPromptOptions, Render, SharedString, Styled, Window, div, px,
};

use crate::theme;

/// Events the sidebar emits to the AppShell. Sidebar owns project /
/// session persistence; AppShell owns terminals + tab strip. They meet
/// here.
#[derive(Clone, Debug)]
pub enum SidebarEvent {
    /// User asked us to open a session in `working_directory`. Could
    /// be a freshly-created worktree or an existing one being
    /// re-opened — the receiver doesn't need to care.
    OpenSession {
        working_directory: PathBuf,
        title: SharedString,
    },
}

/// Width of the sidebar pane. The C# build uses 240 with a
/// resizable splitter; we keep it fixed for now and add the splitter
/// when there's a second column to balance against.
pub const SIDEBAR_WIDTH: f32 = 240.0;

pub struct Sidebar {
    projects: ProjectsConfig,
    /// Index of the currently-selected project. `None` when no
    /// projects exist yet.
    selected: Option<usize>,
    theme: Arc<Theme>,
    /// Where `projects.json` and `layout.json` live. Threaded in so
    /// add/remove + selection changes can persist without re-detecting
    /// the env.
    paths: Arc<AppPaths>,
    /// In-memory copy of `layout.json` — kept in sync as the user
    /// changes selection so a save-on-change writes out the full
    /// (correct) struct, not just the field we touched.
    layout: LayoutState,
}

impl Sidebar {
    pub fn new(
        projects: ProjectsConfig,
        layout: LayoutState,
        theme: Arc<Theme>,
        paths: Arc<AppPaths>,
    ) -> Self {
        // Restore last-opened project if it still exists. Falls back
        // to the first project when the saved id is gone (project
        // removed between sessions) or absent (first launch).
        let selected = match layout.selected_project_id.as_deref() {
            Some(id) => projects.projects.iter().position(|p| p.id == id),
            None => None,
        }
        .or_else(|| (!projects.projects.is_empty()).then_some(0));
        Self { projects, selected, theme, paths, layout }
    }

    pub fn select(&mut self, idx: usize, cx: &mut Context<Self>) {
        if idx >= self.projects.projects.len() || self.selected == Some(idx) {
            // Out-of-range or re-clicking the active row — no-op,
            // skip the synchronous `layout.json` write.
            return;
        }
        self.selected = Some(idx);
        self.layout.selected_project_id =
            Some(self.projects.projects[idx].id.clone());
        self.save_layout();
        cx.notify();
    }

    /// Persist `layout.json` after selection / sidebar-visibility
    /// changes. No debounce — selection is user-driven and slow.
    fn save_layout(&self) {
        if let Err(err) = self.layout.save(&self.paths) {
            eprintln!("warning: failed to save layout.json: {err:#}");
        }
    }

    /// The project the user currently has selected, if any. AppShell
    /// reads this when spawning a new tab so the terminal lands in
    /// the right cwd.
    pub fn active_project(&self) -> Option<&Project> {
        self.selected.and_then(|idx| self.projects.projects.get(idx))
    }

    /// Apply a fresh theme snapshot. Called by the AppShell when the
    /// user changes themes — the sidebar redraws on the next frame.
    #[allow(dead_code)]
    pub fn apply_theme(&mut self, theme: Arc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme;
        cx.notify();
    }

    /// Open the platform "pick a folder" dialog. On confirm, hand the
    /// path to [`Self::add_project`] which writes `projects.json`
    /// before mutating in-memory state, so a save failure leaves both
    /// the disk and the UI in their previous (consistent) state.
    pub fn open_add_project_picker(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Add project".into()),
        });
        cx.spawn(async move |this, cx| {
            let paths = match rx.await {
                Ok(Ok(Some(paths))) => paths,
                // `Ok(None)` = user cancelled, `Ok(Err(...))` = picker
                // failed to open (Linux). Both end the flow silently
                // — the user already sees what happened on screen.
                Ok(Ok(None)) => return,
                Ok(Err(err)) => {
                    eprintln!("warning: file picker failed: {err:#}");
                    return;
                }
                Err(_) => return,
            };
            if let Some(path) = paths.into_iter().next() {
                let path_str = path.to_string_lossy().into_owned();
                let _ = this.update(cx, |this, cx| {
                    this.add_project(path_str, cx);
                });
            }
        })
        .detach();
    }

    /// Spin up a brand-new session under the project at `idx`. Creates
    /// a fresh worktree on a `session-N` branch (next free index),
    /// records the worktree + session in `projects.json`, then asks
    /// the AppShell to open a tab in the new worktree path.
    ///
    /// Synchronous for now: `git worktree add` is fast on a normal
    /// repo (fractions of a second) and shifting it onto a background
    /// task means juggling the `Project` clone across an await point.
    /// Move it async when somebody complains about the tiny stutter.
    pub fn new_session(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(project) = self.projects.projects.get(idx) else {
            return;
        };
        let branch = project.next_session_branch_name();
        let worktree_path = project.worktree_path_for(&branch);
        let repo_path = PathBuf::from(&project.path);
        let base = project.default_branch.clone();
        let project_name = project.name.clone();

        // Create the worktree on disk first. If git fails, leave both
        // disk and projects.json untouched and surface the message.
        if let Err(err) =
            git::add_worktree(&repo_path, &worktree_path, &branch, Some(&base))
        {
            eprintln!("warning: git worktree add failed: {err:#}");
            return;
        }

        // Persist the new Worktree + Session under the project,
        // clone-then-save so a write failure rolls everything back.
        let worktree_id = uuid::Uuid::new_v4().to_string();
        let session_id = uuid::Uuid::new_v4().to_string();
        let worktree_path_str = worktree_path.to_string_lossy().into_owned();
        let mut next = self.projects.clone();
        let project_mut = &mut next.projects[idx];
        project_mut.worktrees.push(Worktree {
            id: worktree_id.clone(),
            path: worktree_path_str.clone(),
            branch: Some(branch.clone()),
            is_primary: false,
        });
        project_mut.sessions.push(Session {
            id: session_id,
            worktree_path: worktree_path_str.clone(),
            branch: Some(branch.clone()),
            agent_id: project_mut.default_agent_id.clone(),
            display_name: None,
            worktree_id: Some(worktree_id),
            last_opened: Some(now_iso8601()),
            agent_session_id: None,
            closed_at: None,
        });
        if let Err(err) = next.save(&self.paths) {
            eprintln!(
                "warning: failed to save projects.json after creating worktree at {}: {err:#}",
                worktree_path_str,
            );
            // The worktree is still on disk — leaving it there is the
            // safer call (user can `git worktree remove` if needed)
            // than racing to undo it and possibly leaving partial
            // state. Surface the failure and bail.
            return;
        }
        self.projects = next;

        // Tell the AppShell to open a tab in the new worktree. Tab
        // title combines project name + branch so multiple sessions
        // on the same project stay distinguishable in the strip.
        let title: SharedString = format!("{project_name}/{branch}").into();
        cx.emit(SidebarEvent::OpenSession {
            working_directory: worktree_path,
            title,
        });
        cx.notify();
    }

    /// Append a project at `path` and persist. Newly-added project
    /// becomes the selection — the user just chose it, so dropping
    /// them straight into it is what they expect.
    ///
    /// Save-then-commit ordering: we build a candidate `ProjectsConfig`,
    /// write it to disk, and only swap it into `self.projects` (and
    /// touch `selected` / `layout.json`) once the write succeeds. A
    /// failed write therefore leaves both disk and UI in their
    /// previous consistent state, instead of producing an in-memory
    /// row that disappears on relaunch — and (worse) a `layout.json`
    /// pointing at a project id that never made it to `projects.json`.
    pub fn add_project(&mut self, path: String, cx: &mut Context<Self>) {
        // Refuse exact duplicates by path. Two rows pointing at the
        // same directory would let a user "add" the same project
        // twice and then wonder why both rows behave identically.
        if let Some(idx) = self.projects.projects.iter().position(|p| p.path == path) {
            self.select(idx, cx);
            return;
        }
        let project = Project::new(path);
        let new_id = project.id.clone();
        // Clone-then-save: failure leaves `self.projects` untouched.
        let mut next = self.projects.clone();
        next.projects.push(project);
        if let Err(err) = next.save(&self.paths) {
            eprintln!("warning: failed to save projects.json: {err:#}");
            return;
        }
        // Disk is committed; now mirror the change in memory.
        self.projects = next;
        let new_idx = self.projects.projects.len() - 1;
        self.selected = Some(new_idx);
        self.layout.selected_project_id = Some(new_id);
        self.save_layout();
        cx.notify();
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = self.theme.clone();
        let selected = self.selected;
        let rows: Vec<_> = self
            .projects
            .projects
            .iter()
            .enumerate()
            .map(|(idx, p)| (idx, p.id.clone(), SharedString::from(p.name.clone())))
            .collect();

        let heading = div()
            .h(px(40.0))
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .text_size(px(11.0))
            .text_color(theme::ink_muted(&theme))
            .child(div().flex_grow().child("PROJECTS"))
            .child(
                div()
                    .id("sidebar-add")
                    .w(px(20.0))
                    .h(px(20.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_sm()
                    .text_color(theme::ink_ghost(&theme))
                    .cursor_pointer()
                    .hover({
                        let frost = theme::frost_10(&theme);
                        let ink = theme::ink(&theme);
                        move |s| s.bg(frost).text_color(ink)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.open_add_project_picker(window, cx);
                        }),
                    )
                    .child("+"),
            );

        let empty_state = if self.projects.projects.is_empty() {
            Some(
                div()
                    .px_3()
                    .py_4()
                    .text_size(px(12.0))
                    .text_color(theme::ink_ghost(&theme))
                    .child("No projects yet. Click + to add one."),
            )
        } else {
            None
        };

        let mut project_rows: Vec<AnyElement> = Vec::new();
        for (idx, id, name) in rows {
            let active = selected == Some(idx);
            let bg = if active {
                theme::frost_10(&theme)
            } else {
                gpui::transparent_black()
            };
            let rail = if active {
                theme::accent(&theme)
            } else {
                gpui::transparent_black()
            };
            let text_color = if active {
                theme::ink(&theme)
            } else {
                theme::ink_dim(&theme)
            };
            let frost_hover = theme::frost_10(&theme);
            let ink_hover = theme::ink(&theme);

            project_rows.push(
                div()
                    .id(("project", id_hash(&id)))
                    .h(px(32.0))
                    .flex()
                    .flex_row()
                    .items_center()
                    .pr_3()
                    .border_l_2()
                    .border_color(rail)
                    .pl(px(10.0)) // 12px - 2px border = 10
                    .bg(bg)
                    .text_color(text_color)
                    .text_size(px(13.0))
                    .cursor_pointer()
                    .hover(move |s| {
                        if active { s } else { s.bg(frost_hover).text_color(ink_hover) }
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| this.select(idx, cx)),
                    )
                    .child(div().flex_grow().truncate().child(name))
                    .into_any_element(),
            );

            // "+ new session" affordance under the active project. Lives
            // here (not in the heading) so the action is visually scoped
            // to the project it operates on. Indented past the rail
            // border so it reads as a child of the row above.
            if active {
                let frost_session = theme::frost_10(&theme);
                let ink_session = theme::ink(&theme);
                project_rows.push(
                    div()
                        .id(("new-session", id_hash(&id)))
                        .h(px(28.0))
                        .flex()
                        .flex_row()
                        .items_center()
                        .pl(px(24.0))
                        .pr_3()
                        .text_size(px(12.0))
                        .text_color(theme::ink_ghost(&theme))
                        .cursor_pointer()
                        .hover(move |s| s.bg(frost_session).text_color(ink_session))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| this.new_session(idx, cx)),
                        )
                        .child("+ new session")
                        .into_any_element(),
                );
            }
        }

        let mut body = div()
            .flex()
            .flex_col()
            .py_1()
            .children(project_rows);
        if let Some(es) = empty_state {
            body = body.child(es);
        }

        div()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .bg(theme::elevated(&theme))
            .border_r_1()
            .border_color(theme::divider(&theme))
            .child(heading)
            .child(div().h_px().bg(theme::divider(&theme)))
            .child(body)
    }
}

impl EventEmitter<SidebarEvent> for Sidebar {}

/// ISO 8601 / RFC 3339 UTC timestamp without sub-second precision —
/// matches what the C# build writes for `lastOpened`. Bare-bones
/// formatter (no `chrono` dep) because we only need to *write* it;
/// nobody parses it back into a typed time on this side yet.
fn now_iso8601() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // Days-from-epoch arithmetic for a Z-suffixed UTC string. Good
    // enough for a "when did the user last touch this" record; not
    // a stand-in for real time math.
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;
    let h = secs_of_day / 3600;
    let m = (secs_of_day % 3600) / 60;
    let s = secs_of_day % 60;
    let (y, mo, d) = days_to_ymd(days);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Days-since-1970-01-01 → (year, month, day). Civil-from-days, the
/// algorithm from Howard Hinnant's date library — branch-free and
/// correct for the full proleptic Gregorian range we care about.
fn days_to_ymd(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as i64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = (if m <= 2 { y + 1 } else { y }) as i32;
    (y, m, d)
}

/// Cheap hash for use as a gpui element id derived from a string id.
/// `id` itself isn't `Hash` for gpui's id needs but `(static_str, u64)`
/// is — so we shrink the project id to a u64.
fn id_hash(id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    id.hash(&mut hasher);
    hasher.finish()
}
