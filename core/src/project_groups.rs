//! User-defined project groups in the sidebar (#374).
//!
//! A group is a named, ordered bucket of projects. Every project sits
//! in exactly one group or in the fixed "Other" section
//! (`Project::group_id == None`), which always renders last. With no
//! groups defined the sidebar renders exactly as before: one headerless
//! section holding every project.
//!
//! Order is manual and lives in the data itself: groups are ordered by
//! their position in [`ProjectsConfig::project_groups`], and projects
//! within a section by their relative position in
//! [`ProjectsConfig::projects`]. Moving a project therefore means moving
//! its row in that list, which shifts project *indices* — callers that
//! hold an index (the sidebar's selection) must re-resolve it by id.
//!
//! Kept free of gpui so it can be TDD'd.

use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::projects::{ProjectGroup, ProjectsConfig};

/// Label of the fixed section that holds ungrouped projects. Reserved:
/// a user group can't take this name, or two "Other" headers would
/// render with different behaviour.
pub const OTHER_SECTION_LABEL: &str = "Other";

/// One sidebar section: a user group, or "Other" (`group: None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarSection<'a> {
    pub group: Option<&'a ProjectGroup>,
    /// Indices into [`ProjectsConfig::projects`], in display order.
    pub projects: Vec<usize>,
}

/// The sidebar's sections, top to bottom.
///
/// * No groups → a single headerless section (`group: None`) with every
///   project, so a user who never makes a group sees no change.
/// * Otherwise every group in order — empty ones too, so there is
///   something to drop a project on — followed by "Other", also always
///   present. A project whose `group_id` names no existing group lands
///   in "Other" rather than vanishing.
pub fn sidebar_sections(cfg: &ProjectsConfig) -> Vec<SidebarSection<'_>> {
    if cfg.project_groups.is_empty() {
        return vec![SidebarSection { group: None, projects: (0..cfg.projects.len()).collect() }];
    }
    let mut sections: Vec<SidebarSection<'_>> = cfg
        .project_groups
        .iter()
        .map(|g| SidebarSection { group: Some(g), projects: Vec::new() })
        .collect();
    let mut other = Vec::new();
    for (idx, project) in cfg.projects.iter().enumerate() {
        let slot = project
            .group_id
            .as_deref()
            .and_then(|gid| cfg.project_groups.iter().position(|g| g.id == gid));
        match slot {
            Some(s) => sections[s].projects.push(idx),
            None => other.push(idx),
        }
    }
    sections.push(SidebarSection { group: None, projects: other });
    sections
}

/// Trimmed, validated group name: non-empty, not the reserved
/// [`OTHER_SECTION_LABEL`], and not already used by another group
/// (case-insensitive — two headers that read the same are a trap).
fn validate_group_name<'a>(
    cfg: &ProjectsConfig,
    name: &'a str,
    except_id: Option<&str>,
) -> Result<&'a str> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        bail!("group name cannot be empty");
    }
    if trimmed.eq_ignore_ascii_case(OTHER_SECTION_LABEL) {
        bail!("\"{OTHER_SECTION_LABEL}\" is reserved");
    }
    if cfg
        .project_groups
        .iter()
        .any(|g| Some(g.id.as_str()) != except_id && g.name.eq_ignore_ascii_case(trimmed))
    {
        bail!("a group named \"{trimmed}\" already exists");
    }
    Ok(trimmed)
}

/// Append a new group named `name` (trimmed) and return its id.
pub fn create_group(cfg: &mut ProjectsConfig, name: &str) -> Result<String> {
    let name = validate_group_name(cfg, name, None)?.to_string();
    let id = uuid::Uuid::new_v4().to_string();
    cfg.project_groups.push(ProjectGroup { id: id.clone(), name });
    Ok(id)
}

/// Rename group `id`. `Ok(false)` when the trimmed name is unchanged.
pub fn rename_group(cfg: &mut ProjectsConfig, id: &str, name: &str) -> Result<bool> {
    let name = validate_group_name(cfg, name, Some(id))?.to_string();
    let Some(group) = cfg.project_groups.iter_mut().find(|g| g.id == id) else {
        bail!("group '{id}' not found");
    };
    if group.name == name {
        return Ok(false);
    }
    group.name = name;
    Ok(true)
}

/// Delete group `id`. Its projects move to "Other", keeping their
/// relative order; nothing else changes — no project or session is
/// removed, which is why the UI doesn't ask for confirmation.
pub fn delete_group(cfg: &mut ProjectsConfig, id: &str) -> Result<()> {
    let before = cfg.project_groups.len();
    cfg.project_groups.retain(|g| g.id != id);
    if cfg.project_groups.len() == before {
        bail!("group '{id}' not found");
    }
    for project in &mut cfg.projects {
        if project.group_id.as_deref() == Some(id) {
            project.group_id = None;
        }
    }
    Ok(())
}

/// Move project `project_id` into `group_id` (`None` = "Other"),
/// landing at the bottom of that section. `Ok(false)` when it is
/// already there. Shifts project indices — see the module docs.
pub fn move_project_to_group(
    cfg: &mut ProjectsConfig,
    project_id: &str,
    group_id: Option<&str>,
) -> Result<bool> {
    if let Some(gid) = group_id
        && !cfg.project_groups.iter().any(|g| g.id == gid)
    {
        bail!("group '{gid}' not found");
    }
    let Some(idx) = cfg.projects.iter().position(|p| p.id == project_id) else {
        bail!("project '{project_id}' not found");
    };
    if cfg.projects[idx].group_id.as_deref() == group_id {
        return Ok(false);
    }
    let mut project = cfg.projects.remove(idx);
    project.group_id = group_id.map(str::to_owned);
    cfg.projects.push(project);
    Ok(true)
}

/// Where a dragged project lands (#374).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectDropTarget {
    /// Directly above this project, in its section.
    Before(String),
    /// Directly below this project, in its section.
    After(String),
    /// At the bottom of this section (`None` = "Other") — a drop on a
    /// section header.
    IntoGroup(Option<String>),
}

/// Move `project_id` to `target`. Dropping next to a project adopts
/// that project's section; dropping on a header behaves like
/// [`move_project_to_group`]. `Ok(false)` when nothing moved (dropped
/// on itself, or into the section it is already in). Shifts project
/// indices — see the module docs.
pub fn move_project(cfg: &mut ProjectsConfig, project_id: &str, target: &ProjectDropTarget) -> Result<bool> {
    let (anchor, after) = match target {
        ProjectDropTarget::IntoGroup(group) => {
            return move_project_to_group(cfg, project_id, group.as_deref());
        }
        ProjectDropTarget::Before(anchor) => (anchor.as_str(), false),
        ProjectDropTarget::After(anchor) => (anchor.as_str(), true),
    };
    let Some(from) = cfg.projects.iter().position(|p| p.id == project_id) else {
        bail!("project '{project_id}' not found");
    };
    if anchor == project_id {
        return Ok(false);
    }
    let Some(anchor_idx) = cfg.projects.iter().position(|p| p.id == anchor) else {
        bail!("project '{anchor}' not found");
    };
    // The anchor's *effective* section: a dangling group id renders in
    // "Other", so the dragged project joins "Other" too.
    let group = cfg.projects[anchor_idx]
        .group_id
        .clone()
        .filter(|gid| cfg.project_groups.iter().any(|g| &g.id == gid));
    let mut to = if after { anchor_idx + 1 } else { anchor_idx };
    if from < to {
        to -= 1;
    }
    if from == to && cfg.projects[from].group_id == group {
        return Ok(false);
    }
    let mut project = cfg.projects.remove(from);
    project.group_id = group;
    cfg.projects.insert(to, project);
    Ok(true)
}

/// Move group `group_id` so it renders directly before `before`, or
/// last (just above "Other") when `before` is `None`. `Ok(false)` when
/// the order doesn't change.
pub fn move_group(cfg: &mut ProjectsConfig, group_id: &str, before: Option<&str>) -> Result<bool> {
    let Some(from) = cfg.project_groups.iter().position(|g| g.id == group_id) else {
        bail!("group '{group_id}' not found");
    };
    if before == Some(group_id) {
        return Ok(false);
    }
    let mut to = match before {
        Some(b) => match cfg.project_groups.iter().position(|g| g.id == b) {
            Some(idx) => idx,
            None => bail!("group '{b}' not found"),
        },
        None => cfg.project_groups.len(),
    };
    if from < to {
        to -= 1;
    }
    if from == to {
        return Ok(false);
    }
    let group = cfg.project_groups.remove(from);
    cfg.project_groups.insert(to, group);
    Ok(true)
}

/// State of one live session, as a dot on a collapsed sidebar row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionDot {
    /// Agent is working (`Busy` / `PendingToolUse`) — red.
    Busy,
    /// Session is live and waiting on the user — green.
    Idle,
}

/// Dots for a run of worktree paths, in the order given: each path's
/// live sessions in `live` order, paths with no live session skipped.
/// The caller passes paths in sidebar row order (projects in section
/// order, then each project's worktree rows), so the dots read in the
/// same order as the rows they stand in for.
pub fn collect_session_dots<'a>(
    paths: impl IntoIterator<Item = &'a str>,
    live: &HashMap<String, Vec<SessionDot>>,
) -> Vec<SessionDot> {
    paths
        .into_iter()
        .filter_map(|p| live.get(p))
        .flat_map(|dots| dots.iter().copied())
        .collect()
}

/// Cap `dots` at `max`, returning the shown dots and how many were cut
/// off (rendered as "+N").
pub fn cap_dots(mut dots: Vec<SessionDot>, max: usize) -> (Vec<SessionDot>, usize) {
    let overflow = dots.len().saturating_sub(max);
    dots.truncate(max);
    (dots, overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::projects::Project;

    fn project(name: &str, group: Option<&str>) -> Project {
        let mut p = Project::new(format!("/dev/{name}"));
        p.id = name.to_string();
        p.group_id = group.map(str::to_owned);
        p
    }

    fn group(id: &str, name: &str) -> ProjectGroup {
        ProjectGroup { id: id.into(), name: name.into() }
    }

    fn cfg(groups: Vec<ProjectGroup>, projects: Vec<Project>) -> ProjectsConfig {
        ProjectsConfig { project_groups: groups, projects, ..ProjectsConfig::default() }
    }

    fn names(cfg: &ProjectsConfig, section: &SidebarSection<'_>) -> Vec<String> {
        section.projects.iter().map(|&i| cfg.projects[i].id.clone()).collect()
    }

    #[test]
    fn without_groups_one_headerless_section_holds_everything() {
        let c = cfg(vec![], vec![project("a", None), project("b", None)]);
        let sections = sidebar_sections(&c);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].group.is_none());
        assert_eq!(names(&c, &sections[0]), ["a", "b"]);
    }

    #[test]
    fn groups_in_order_then_other_last_including_empty_ones() {
        let c = cfg(
            vec![group("g1", "Code"), group("g2", "Admin"), group("g3", "Empty")],
            vec![
                project("a", Some("g2")),
                project("b", None),
                project("c", Some("g1")),
                project("d", Some("g2")),
            ],
        );
        let sections = sidebar_sections(&c);
        let headers: Vec<Option<&str>> =
            sections.iter().map(|s| s.group.map(|g| g.name.as_str())).collect();
        assert_eq!(headers, [Some("Code"), Some("Admin"), Some("Empty"), None]);
        assert_eq!(names(&c, &sections[0]), ["c"]);
        // Within a section, list order is display order.
        assert_eq!(names(&c, &sections[1]), ["a", "d"]);
        assert!(sections[2].projects.is_empty());
        assert_eq!(names(&c, &sections[3]), ["b"]);
    }

    #[test]
    fn project_with_unknown_group_lands_in_other() {
        let c = cfg(vec![group("g1", "Code")], vec![project("a", Some("gone"))]);
        let sections = sidebar_sections(&c);
        assert_eq!(names(&c, &sections[1]), ["a"]);
    }

    #[test]
    fn create_group_trims_and_rejects_empty_duplicate_and_reserved_names() {
        let mut c = cfg(vec![], vec![]);
        let id = create_group(&mut c, "  Code ").unwrap();
        assert_eq!(c.project_groups, [group(&id, "Code")]);
        assert!(create_group(&mut c, "   ").is_err());
        assert!(create_group(&mut c, "code").is_err());
        assert!(create_group(&mut c, "other").is_err());
        assert_eq!(c.project_groups.len(), 1);
    }

    #[test]
    fn rename_group_validates_and_reports_no_op() {
        let mut c = cfg(vec![group("g1", "Code"), group("g2", "Admin")], vec![]);
        assert!(rename_group(&mut c, "g1", "Apps").unwrap());
        assert_eq!(c.project_groups[0].name, "Apps");
        assert!(!rename_group(&mut c, "g1", " Apps ").unwrap());
        // Case-only rename of itself is allowed; colliding with another isn't.
        assert!(rename_group(&mut c, "g1", "APPS").unwrap());
        assert!(rename_group(&mut c, "g1", "admin").is_err());
        assert!(rename_group(&mut c, "nope", "X").is_err());
    }

    #[test]
    fn delete_group_moves_its_projects_to_other_and_keeps_everything_else() {
        let mut c = cfg(
            vec![group("g1", "Code"), group("g2", "Admin")],
            vec![project("a", Some("g1")), project("b", Some("g2")), project("c", Some("g1"))],
        );
        delete_group(&mut c, "g1").unwrap();
        assert_eq!(c.project_groups, [group("g2", "Admin")]);
        assert_eq!(c.projects.len(), 3);
        let sections = sidebar_sections(&c);
        assert_eq!(names(&c, &sections[0]), ["b"]);
        assert_eq!(names(&c, &sections[1]), ["a", "c"]);
        assert!(delete_group(&mut c, "g1").is_err());
    }

    #[test]
    fn deleting_the_last_group_restores_the_headerless_sidebar() {
        let mut c = cfg(vec![group("g1", "Code")], vec![project("a", Some("g1"))]);
        delete_group(&mut c, "g1").unwrap();
        let sections = sidebar_sections(&c);
        assert_eq!(sections.len(), 1);
        assert!(sections[0].group.is_none());
    }

    #[test]
    fn move_project_to_group_lands_at_the_bottom_of_the_target() {
        let mut c = cfg(
            vec![group("g1", "Code")],
            vec![project("a", Some("g1")), project("b", None), project("c", Some("g1"))],
        );
        assert!(move_project_to_group(&mut c, "b", Some("g1")).unwrap());
        let sections = sidebar_sections(&c);
        assert_eq!(names(&c, &sections[0]), ["a", "c", "b"]);
        assert!(sections[1].projects.is_empty());

        assert!(move_project_to_group(&mut c, "a", None).unwrap());
        let sections = sidebar_sections(&c);
        assert_eq!(names(&c, &sections[0]), ["c", "b"]);
        assert_eq!(names(&c, &sections[1]), ["a"]);
    }

    #[test]
    fn move_project_to_its_own_group_is_a_no_op_and_unknown_targets_fail() {
        let mut c = cfg(vec![group("g1", "Code")], vec![project("a", Some("g1")), project("b", None)]);
        assert!(!move_project_to_group(&mut c, "a", Some("g1")).unwrap());
        assert_eq!(c.projects[0].id, "a", "a no-op must not reorder");
        assert!(move_project_to_group(&mut c, "a", Some("nope")).is_err());
        assert!(move_project_to_group(&mut c, "nope", None).is_err());
    }

    fn order(cfg: &ProjectsConfig) -> Vec<Vec<String>> {
        sidebar_sections(cfg).iter().map(|s| names(cfg, s)).collect()
    }

    fn drag_cfg() -> ProjectsConfig {
        cfg(
            vec![group("g1", "Code"), group("g2", "Admin")],
            vec![
                project("a", Some("g1")),
                project("b", Some("g1")),
                project("c", Some("g2")),
                project("d", None),
            ],
        )
    }

    #[test]
    fn drop_before_or_after_a_project_in_the_same_section_reorders() {
        let mut c = drag_cfg();
        assert!(move_project(&mut c, "b", &ProjectDropTarget::Before("a".into())).unwrap());
        assert_eq!(order(&c), [vec!["b", "a"], vec!["c"], vec!["d"]]);
        assert!(move_project(&mut c, "b", &ProjectDropTarget::After("a".into())).unwrap());
        assert_eq!(order(&c), [vec!["a", "b"], vec!["c"], vec!["d"]]);
    }

    #[test]
    fn drop_next_to_a_project_in_another_section_joins_that_section() {
        let mut c = drag_cfg();
        assert!(move_project(&mut c, "d", &ProjectDropTarget::Before("b".into())).unwrap());
        assert_eq!(order(&c), [vec!["a", "d", "b"], vec!["c"], vec![]]);
        assert_eq!(c.projects.iter().find(|p| p.id == "d").unwrap().group_id.as_deref(), Some("g1"));

        assert!(move_project(&mut c, "a", &ProjectDropTarget::After("c".into())).unwrap());
        assert_eq!(order(&c), [vec!["d", "b"], vec!["c", "a"], vec![]]);
    }

    #[test]
    fn drop_onto_itself_or_its_current_slot_is_a_no_op() {
        let mut c = drag_cfg();
        assert!(!move_project(&mut c, "a", &ProjectDropTarget::Before("a".into())).unwrap());
        // "a" already sits right above "b".
        assert!(!move_project(&mut c, "a", &ProjectDropTarget::Before("b".into())).unwrap());
        assert!(!move_project(&mut c, "b", &ProjectDropTarget::After("a".into())).unwrap());
        assert_eq!(order(&c), [vec!["a", "b"], vec!["c"], vec!["d"]]);
    }

    #[test]
    fn drop_on_a_header_lands_at_the_bottom_of_that_section() {
        let mut c = drag_cfg();
        assert!(move_project(&mut c, "a", &ProjectDropTarget::IntoGroup(Some("g2".into()))).unwrap());
        assert!(move_project(&mut c, "c", &ProjectDropTarget::IntoGroup(None)).unwrap());
        assert_eq!(order(&c), [vec!["b"], vec!["a"], vec!["d", "c"]]);
    }

    #[test]
    fn drop_next_to_a_project_with_a_dangling_group_joins_other() {
        let mut c = drag_cfg();
        c.projects[3].group_id = Some("gone".into());
        assert!(move_project(&mut c, "a", &ProjectDropTarget::After("d".into())).unwrap());
        let a = c.projects.iter().find(|p| p.id == "a").unwrap();
        assert_eq!(a.group_id, None);
        assert_eq!(order(&c), [vec!["b"], vec!["c"], vec!["d", "a"]]);
    }

    #[test]
    fn move_project_rejects_unknown_ids() {
        let mut c = drag_cfg();
        assert!(move_project(&mut c, "nope", &ProjectDropTarget::Before("a".into())).is_err());
        assert!(move_project(&mut c, "a", &ProjectDropTarget::Before("nope".into())).is_err());
    }

    #[test]
    fn move_group_reorders_and_reports_no_ops() {
        let mut c = cfg(vec![group("g1", "A"), group("g2", "B"), group("g3", "C")], vec![]);
        let ids = |c: &ProjectsConfig| c.project_groups.iter().map(|g| g.id.clone()).collect::<Vec<_>>();
        assert!(move_group(&mut c, "g3", Some("g1")).unwrap());
        assert_eq!(ids(&c), ["g3", "g1", "g2"]);
        assert!(move_group(&mut c, "g3", None).unwrap());
        assert_eq!(ids(&c), ["g1", "g2", "g3"]);
        assert!(!move_group(&mut c, "g1", Some("g2")).unwrap());
        assert!(!move_group(&mut c, "g3", None).unwrap());
        assert!(!move_group(&mut c, "g2", Some("g2")).unwrap());
        assert!(move_group(&mut c, "nope", None).is_err());
        assert!(move_group(&mut c, "g1", Some("nope")).is_err());
    }

    #[test]
    fn session_dots_follow_path_order_and_skip_paths_without_sessions() {
        use SessionDot::{Busy, Idle};
        let live: HashMap<String, Vec<SessionDot>> = [
            ("/b".to_string(), vec![Idle]),
            ("/a".to_string(), vec![Busy, Idle]),
        ]
        .into();
        assert_eq!(collect_session_dots(["/a", "/x", "/b"], &live), [Busy, Idle, Idle]);
        assert!(collect_session_dots(["/x"], &live).is_empty());
    }

    #[test]
    fn cap_dots_reports_the_overflow() {
        use SessionDot::{Busy, Idle};
        assert_eq!(cap_dots(vec![Busy, Idle, Idle], 2), (vec![Busy, Idle], 1));
        assert_eq!(cap_dots(vec![Busy], 6), (vec![Busy], 0));
    }
}
