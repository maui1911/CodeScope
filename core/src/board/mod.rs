//! Boards on a host: the work intake for dispatch (ADR-0027, terms in
//! `GLOSSARY.md`). This module is host-neutral; [`github`] reads a
//! GitHub Projects board through `gh`.

pub mod answers;
pub mod dispatch;
pub mod findings;
pub mod github;
pub mod pull_request;
pub mod repair;
pub mod reply;

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

/// Whose namespace a board lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardOwner {
    User,
    Organization,
}

/// A board as its URL names it: `https://github.com/{users|orgs}/<owner>/projects/<number>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardRef {
    pub owner_kind: BoardOwner,
    pub owner: String,
    pub number: u32,
}

impl BoardRef {
    /// Accepts a board URL with or without scheme, view, query or trailing slash.
    pub fn parse_url(url: &str) -> Option<BoardRef> {
        let url = url.trim();
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url);
        let rest = rest.strip_prefix("www.").unwrap_or(rest);
        let path = rest.strip_prefix("github.com/")?.split(['?', '#']).next()?;
        let mut parts = path.split('/');
        let owner_kind = match parts.next()? {
            "users" => BoardOwner::User,
            "orgs" => BoardOwner::Organization,
            _ => return None,
        };
        let owner = parts.next()?;
        if !is_login(owner) || parts.next()? != "projects" {
            return None;
        }
        let number = parts.next()?.parse::<u32>().ok().filter(|n| *n > 0)?;
        Some(BoardRef {
            owner_kind,
            owner: owner.to_string(),
            number,
        })
    }

    pub fn url(&self) -> String {
        let kind = match self.owner_kind {
            BoardOwner::User => "users",
            BoardOwner::Organization => "orgs",
        };
        format!(
            "https://github.com/{kind}/{}/projects/{}",
            self.owner, self.number
        )
    }

    /// Whether both name one board; logins are compared without case, as GitHub does.
    pub fn same_board(&self, other: &BoardRef) -> bool {
        self.owner_kind == other.owner_kind
            && self.number == other.number
            && self.owner.eq_ignore_ascii_case(&other.owner)
    }
}

/// Letters, digits, `-` and `_` (managed accounts end in `_<shortcode>`), not starting with `-`.
fn is_login(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// One column of a board; `id` is opaque and only meaningful to its host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    #[default]
    Issue,
    PullRequest,
    /// A draft issue, which has no number, URL or repository yet.
    Draft,
}

impl ItemKind {
    /// The word for it in a sentence.
    pub fn noun(self) -> &'static str {
        match self {
            ItemKind::Issue => "issue",
            ItemKind::PullRequest => "pull request",
            ItemKind::Draft => "draft issue",
        }
    }
}

/// The state of the issue or pull request behind an item; a draft is `Open`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemState {
    Open,
    Closed,
    Merged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardItem {
    /// The host's id for the item on this board, not the issue's.
    pub id: String,
    pub kind: ItemKind,
    pub number: Option<u32>,
    pub title: String,
    pub url: Option<String>,
    /// `owner/name` of the repository the issue or pull request lives in.
    pub repository: Option<String>,
    /// The host's id for the issue or pull request itself; `None` for a draft.
    pub content_id: Option<String>,
    /// Logins the issue or pull request is assigned to.
    pub assignees: Vec<String>,
    pub state: ItemState,
    /// Id of the [`Column`] the item is in; `None` when it has no status.
    pub column_id: Option<String>,
    /// When the item was put in that column, ISO 8601; `None` when it has no status.
    pub column_set_at: Option<String>,
    /// ISO 8601, as the host reports it.
    pub updated_at: String,
}

/// A board as read in one go. Archived items are left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Board {
    /// The host's id for the board, which writes to it need.
    pub id: String,
    /// The host's id for the status field whose options are the columns.
    pub status_field_id: String,
    pub title: String,
    pub url: String,
    pub closed: bool,
    pub columns: Vec<Column>,
    pub items: Vec<BoardItem>,
    /// Set when the board holds more items than one read takes; the rest are left out.
    pub truncated: bool,
}

impl Board {
    pub fn column(&self, id: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.id == id)
    }

    /// The role of the column an item is in, if that column has one.
    pub fn role_of(&self, item: &BoardItem, mapping: &ColumnMapping) -> Option<ColumnRole> {
        self.role_of_column(item.column_id.as_deref()?, mapping)
    }

    /// The role of the column with id `column_id`, if it has one.
    pub fn role_of_column(&self, column_id: &str, mapping: &ColumnMapping) -> Option<ColumnRole> {
        ColumnRole::ALL.into_iter().find(|role| {
            mapped_column(self, mapping, *role).is_some_and(|c| c.id == column_id)
        })
    }
}

/// What a column means to CodeScope. Serialised in camelCase as a `settings.json` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColumnRole {
    Todo,
    Research,
    Researching,
    NeedsDecision,
    Ready,
    InProgress,
    InReview,
    Done,
}

/// Who moves items into a column with a given role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleKind {
    /// Where items rest until the user moves them.
    Rest,
    /// Filled by the user; CodeScope starts a phase for each item.
    Trigger,
    /// Only CodeScope moves items here.
    Progress,
    /// Set by the host's own workflows.
    Host,
}

impl ColumnRole {
    pub const ALL: [ColumnRole; 8] = [
        ColumnRole::Todo,
        ColumnRole::Research,
        ColumnRole::Researching,
        ColumnRole::NeedsDecision,
        ColumnRole::Ready,
        ColumnRole::InProgress,
        ColumnRole::InReview,
        ColumnRole::Done,
    ];

    pub fn kind(self) -> RoleKind {
        match self {
            ColumnRole::Todo => RoleKind::Rest,
            ColumnRole::Research | ColumnRole::Ready => RoleKind::Trigger,
            ColumnRole::Researching
            | ColumnRole::NeedsDecision
            | ColumnRole::InProgress
            | ColumnRole::InReview => RoleKind::Progress,
            ColumnRole::Done => RoleKind::Host,
        }
    }

    /// The column name suggested for this role on a new board.
    pub fn label(self) -> &'static str {
        match self {
            ColumnRole::Todo => "Todo",
            ColumnRole::Research => "Research",
            ColumnRole::Researching => "Researching",
            ColumnRole::NeedsDecision => "Needs decision",
            ColumnRole::Ready => "Ready",
            ColumnRole::InProgress => "In progress",
            ColumnRole::InReview => "In review",
            ColumnRole::Done => "Done",
        }
    }

    fn aliases(self) -> &'static [&'static str] {
        match self {
            ColumnRole::Todo => &["todo", "to do", "backlog"],
            ColumnRole::Research => &["research"],
            ColumnRole::Researching => &["researching"],
            ColumnRole::NeedsDecision => &["needs decision"],
            ColumnRole::Ready => &["ready"],
            ColumnRole::InProgress => &["in progress"],
            ColumnRole::InReview => &["in review"],
            ColumnRole::Done => &["done"],
        }
    }
}

/// Which column plays which role on a board, by column name. Missing roles are unmapped.
pub type ColumnMapping = BTreeMap<ColumnRole, String>;

/// Lower case, letters and digits only, one space between words: `🔍 In-Progress` → `in progress`.
pub fn normalize_column_name(name: &str) -> String {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map every role whose name, or an alias of it, is one of the board's columns.
pub fn suggest_mapping(columns: &[Column]) -> ColumnMapping {
    let mut mapping = ColumnMapping::new();
    for role in ColumnRole::ALL {
        let found = role.aliases().iter().find_map(|alias| {
            columns
                .iter()
                .find(|c| normalize_column_name(&c.name) == *alias)
        });
        if let Some(column) = found {
            mapping.insert(role, column.name.clone());
        }
    }
    mapping
}

/// The stored mapping, with every role whose column is not on the board filled in by
/// [`suggest_mapping`]; a column added on the host later is picked up without a remap. A column
/// gets one role: the first stored one in [`ColumnRole::ALL`] order, else its suggested one.
pub fn effective_mapping(board: &Board, mapping: &ColumnMapping) -> ColumnMapping {
    let mut effective = ColumnMapping::new();
    let mut claimed: Vec<&str> = Vec::new();
    let suggested = suggest_mapping(&board.columns);
    for source in [mapping, &suggested] {
        for role in source.keys() {
            if effective.contains_key(role) {
                continue;
            }
            if let Some(column) = mapped_column(board, source, *role)
                && !claimed.contains(&column.id.as_str())
            {
                claimed.push(&column.id);
                effective.insert(*role, column.name.clone());
            }
        }
    }
    effective
}

/// The column a role maps to: the one with exactly that name, else the only one whose
/// [`normalize_column_name`] matches.
pub fn mapped_column<'a>(
    board: &'a Board,
    mapping: &ColumnMapping,
    role: ColumnRole,
) -> Option<&'a Column> {
    let stored = mapping.get(&role)?;
    if let Some(exact) = board.columns.iter().find(|c| c.name == *stored) {
        return Some(exact);
    }
    let name = normalize_column_name(stored);
    if name.is_empty() {
        return None;
    }
    let mut alike = board
        .columns
        .iter()
        .filter(|c| normalize_column_name(&c.name) == name);
    let column = alike.next()?;
    alike.next().is_none().then_some(column)
}

/// Roles without a column on this board, in [`ColumnRole::ALL`] order.
pub fn missing_roles(board: &Board, mapping: &ColumnMapping) -> Vec<ColumnRole> {
    ColumnRole::ALL
        .into_iter()
        .filter(|role| mapped_column(board, mapping, *role).is_none())
        .collect()
}

/// A board as `settings.json` keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BoardSettings {
    pub url: String,
    /// Read leniently: a role this build doesn't know, or a name that isn't text, is skipped.
    #[serde(deserialize_with = "known_roles")]
    pub columns: ColumnMapping,
    /// When the board was added, ISO 8601; moves made before then start nothing.
    pub added_at: String,
    /// Keys this build doesn't know, kept so a save doesn't drop what a newer build wrote.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

fn known_roles<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ColumnMapping, D::Error> {
    let serde_json::Value::Object(entries) = serde_json::Value::deserialize(deserializer)? else {
        return Ok(ColumnMapping::new());
    };
    Ok(entries
        .into_iter()
        .filter_map(|(key, name)| {
            let role = serde_json::from_value(serde_json::Value::String(key)).ok()?;
            Some((role, name.as_str()?.to_string()))
        })
        .collect())
}

/// The four columns of the board panel; research and implementation each gather three host columns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelGroup {
    Todo,
    Research,
    Implementation,
    Done,
}

impl PanelGroup {
    pub const ALL: [PanelGroup; 4] = [
        PanelGroup::Todo,
        PanelGroup::Research,
        PanelGroup::Implementation,
        PanelGroup::Done,
    ];

    pub fn label(self) -> &'static str {
        match self {
            PanelGroup::Todo => "Todo",
            PanelGroup::Research => "Research",
            PanelGroup::Implementation => "Implementation",
            PanelGroup::Done => "Done",
        }
    }

    fn roles(self) -> &'static [ColumnRole] {
        match self {
            PanelGroup::Todo => &[ColumnRole::Todo],
            PanelGroup::Research => &[
                ColumnRole::Research,
                ColumnRole::Researching,
                ColumnRole::NeedsDecision,
            ],
            PanelGroup::Implementation => &[
                ColumnRole::Ready,
                ColumnRole::InProgress,
                ColumnRole::InReview,
            ],
            PanelGroup::Done => &[ColumnRole::Done],
        }
    }
}

/// One host column inside a panel column. `column == None` collects the items without a status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelSection {
    pub column: Option<String>,
    pub role: Option<ColumnRole>,
    /// Indexes into [`Board::items`], in board order.
    pub items: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelColumn {
    pub group: PanelGroup,
    pub sections: Vec<PanelSection>,
}

/// Lay a board out in the panel's four columns. Columns without a role, and items without a
/// status, go under Todo after the Todo column itself.
pub fn panel_columns(board: &Board, mapping: &ColumnMapping) -> Vec<PanelColumn> {
    let items_in = |column_id: &str| -> Vec<usize> {
        board
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.column_id.as_deref() == Some(column_id))
            .map(|(index, _)| index)
            .collect()
    };
    let roled: Vec<(ColumnRole, &Column)> = ColumnRole::ALL
        .into_iter()
        .filter_map(|role| mapped_column(board, mapping, role).map(|c| (role, c)))
        .collect();

    PanelGroup::ALL
        .into_iter()
        .map(|group| {
            let mut sections: Vec<PanelSection> = group
                .roles()
                .iter()
                .filter_map(|role| roled.iter().find(|(r, _)| r == role))
                .map(|(role, column)| PanelSection {
                    column: Some(column.name.clone()),
                    role: Some(*role),
                    items: items_in(&column.id),
                })
                .collect();
            if group == PanelGroup::Todo {
                for column in &board.columns {
                    if roled.iter().all(|(_, c)| c.id != column.id) {
                        sections.push(PanelSection {
                            column: Some(column.name.clone()),
                            role: None,
                            items: items_in(&column.id),
                        });
                    }
                }
                let without_status: Vec<usize> = board
                    .items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| {
                        item.column_id
                            .as_deref()
                            .and_then(|id| board.column(id))
                            .is_none()
                    })
                    .map(|(index, _)| index)
                    .collect();
                if !without_status.is_empty() {
                    sections.push(PanelSection {
                        column: None,
                        role: None,
                        items: without_status,
                    });
                }
            }
            PanelColumn { group, sections }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(id: &str, name: &str) -> Column {
        Column {
            id: id.to_string(),
            name: name.to_string(),
        }
    }

    fn item(id: &str, column_id: Option<&str>) -> BoardItem {
        BoardItem {
            id: id.to_string(),
            kind: ItemKind::Issue,
            number: Some(1),
            title: id.to_string(),
            url: None,
            repository: Some("maui1911/CodeScope".to_string()),
            content_id: Some(format!("I_{id}")),
            assignees: Vec::new(),
            state: ItemState::Open,
            column_id: column_id.map(str::to_string),
            column_set_at: None,
            updated_at: String::new(),
        }
    }

    fn board(columns: Vec<Column>, items: Vec<BoardItem>) -> Board {
        Board {
            id: "PVT_1".into(),
            status_field_id: "PVTSSF_1".into(),
            title: "CodeScope".into(),
            url: String::new(),
            closed: false,
            columns,
            items,
            truncated: false,
        }
    }

    #[test]
    fn parses_user_and_org_board_urls() {
        let user = BoardRef::parse_url("https://github.com/users/maui1911/projects/3").unwrap();
        assert_eq!(user.owner_kind, BoardOwner::User);
        assert_eq!(user.owner, "maui1911");
        assert_eq!(user.number, 3);
        let org =
            BoardRef::parse_url("github.com/orgs/my-org/projects/12/views/2?layout=board").unwrap();
        assert_eq!(org.owner_kind, BoardOwner::Organization);
        assert_eq!(org.number, 12);
        assert_eq!(org.url(), "https://github.com/orgs/my-org/projects/12");
    }

    #[test]
    fn rejects_urls_that_are_not_boards() {
        for url in [
            "",
            "https://github.com/maui1911/CodeScope",
            "https://github.com/maui1911/CodeScope/projects/1",
            "https://github.com/users/maui1911/projects/",
            "https://github.com/users/maui1911/projects/0",
            "https://github.com/users/-flag/projects/1",
            "https://gitlab.com/users/maui1911/projects/1",
        ] {
            assert_eq!(BoardRef::parse_url(url), None, "{url}");
        }
    }

    #[test]
    fn parses_logins_with_an_underscore() {
        let board = BoardRef::parse_url("https://github.com/users/jdoe_acme/projects/2").unwrap();
        assert_eq!(board.owner, "jdoe_acme");
    }

    #[test]
    fn the_same_board_under_another_casing_is_the_same_board() {
        let known = BoardRef::parse_url("https://github.com/users/maui1911/projects/3").unwrap();
        let same = BoardRef::parse_url("github.com/users/Maui1911/projects/3").unwrap();
        assert!(known.same_board(&same));
        for other in [
            "https://github.com/users/maui1911/projects/4",
            "https://github.com/orgs/maui1911/projects/3",
            "https://github.com/users/maui1912/projects/3",
        ] {
            assert!(!known.same_board(&BoardRef::parse_url(other).unwrap()), "{other}");
        }
    }

    #[test]
    fn normalizes_column_names() {
        assert_eq!(normalize_column_name("🔍 In-Progress "), "in progress");
        assert_eq!(normalize_column_name("Needs  decision"), "needs decision");
    }

    #[test]
    fn suggests_roles_from_column_names() {
        let columns = vec![
            column("a", "Backlog"),
            column("b", "Todo"),
            column("c", "🔬 Research"),
            column("d", "In Progress"),
            column("e", "Done"),
        ];
        let mapping = suggest_mapping(&columns);
        assert_eq!(
            mapping.get(&ColumnRole::Todo).map(String::as_str),
            Some("Todo")
        );
        assert_eq!(
            mapping.get(&ColumnRole::Research).map(String::as_str),
            Some("🔬 Research")
        );
        assert_eq!(
            mapping.get(&ColumnRole::InProgress).map(String::as_str),
            Some("In Progress")
        );
        assert_eq!(mapping.get(&ColumnRole::Ready), None);
    }

    #[test]
    fn backlog_stands_in_for_todo_when_there_is_no_todo_column() {
        let mapping = suggest_mapping(&[column("a", "Backlog"), column("b", "Ready")]);
        assert_eq!(
            mapping.get(&ColumnRole::Todo).map(String::as_str),
            Some("Backlog")
        );
    }

    #[test]
    fn reports_roles_whose_column_is_gone() {
        let b = board(vec![column("a", "Todo"), column("b", "Done")], vec![]);
        let mut mapping = suggest_mapping(&b.columns);
        mapping.insert(ColumnRole::Ready, "Ready".into());
        let missing = missing_roles(&b, &mapping);
        assert!(missing.contains(&ColumnRole::Ready));
        assert!(!missing.contains(&ColumnRole::Todo));
        assert_eq!(missing.len(), 6);
    }

    #[test]
    fn lays_out_items_by_role_with_the_rest_under_todo() {
        let b = board(
            vec![
                column("t", "Todo"),
                column("r", "Research"),
                column("p", "Paused"),
                column("i", "In progress"),
                column("d", "Done"),
            ],
            vec![
                item("in-research", Some("r")),
                item("paused", Some("p")),
                item("no-status", None),
                item("stale-column", Some("gone")),
                item("working", Some("i")),
                item("todo", Some("t")),
                item("done", Some("d")),
            ],
        );
        let layout = panel_columns(&b, &suggest_mapping(&b.columns));
        let groups: Vec<PanelGroup> = layout.iter().map(|c| c.group).collect();
        assert_eq!(groups, PanelGroup::ALL);

        let todo = &layout[0].sections;
        assert_eq!(todo.len(), 3);
        assert_eq!(todo[0].role, Some(ColumnRole::Todo));
        assert_eq!(todo[0].items, vec![5]);
        assert_eq!(todo[1].column.as_deref(), Some("Paused"));
        assert_eq!(todo[1].role, None);
        assert_eq!(todo[1].items, vec![1]);
        assert_eq!(todo[2].column, None);
        assert_eq!(todo[2].items, vec![2, 3]);

        assert_eq!(layout[1].sections.len(), 1);
        assert_eq!(layout[1].sections[0].items, vec![0]);
        assert_eq!(layout[2].sections[0].role, Some(ColumnRole::InProgress));
        assert_eq!(layout[2].sections[0].items, vec![4]);
        assert_eq!(layout[3].sections[0].items, vec![6]);
    }

    #[test]
    fn mapping_matches_a_renamed_column_with_other_casing() {
        let b = board(vec![column("r", "READY")], vec![item("x", Some("r"))]);
        let mapping = ColumnMapping::from([(ColumnRole::Ready, "Ready".to_string())]);
        let layout = panel_columns(&b, &mapping);
        assert_eq!(layout[2].sections[0].role, Some(ColumnRole::Ready));
        assert_eq!(layout[2].sections[0].items, vec![0]);
    }

    #[test]
    fn a_mapped_name_matches_its_own_column_before_a_lookalike() {
        let b = board(
            vec![
                column("a", "In progress"),
                column("b", "In-progress"),
                column("c", "🚀"),
                column("d", "🔥"),
            ],
            vec![],
        );
        let mapped = |name: &str| {
            let mapping = ColumnMapping::from([(ColumnRole::InProgress, name.to_string())]);
            mapped_column(&b, &mapping, ColumnRole::InProgress).map(|c| c.id.clone())
        };
        assert_eq!(mapped("In-progress").as_deref(), Some("b"));
        assert_eq!(mapped("🔥").as_deref(), Some("d"));
        assert_eq!(mapped("IN PROGRESS"), None, "two columns look like it");
        assert_eq!(mapped("✨"), None, "an emoji-only name is not a wildcard");
    }

    #[test]
    fn a_column_gets_one_role() {
        let b = board(vec![column("t", "Todo"), column("r", "Ready")], vec![]);
        let stored = ColumnMapping::from([
            (ColumnRole::Todo, "Ready".to_string()),
            (ColumnRole::Ready, "Ready".to_string()),
        ]);
        let effective = effective_mapping(&b, &stored);
        assert_eq!(
            effective.get(&ColumnRole::Todo).map(String::as_str),
            Some("Ready")
        );
        assert_eq!(effective.get(&ColumnRole::Ready), None);

        let stored = ColumnMapping::from([(ColumnRole::Ready, "Todo".to_string())]);
        let effective = effective_mapping(&b, &stored);
        assert_eq!(
            effective.get(&ColumnRole::Ready).map(String::as_str),
            Some("Todo")
        );
        assert_eq!(effective.get(&ColumnRole::Todo), None, "Todo's column is taken");
    }

    #[test]
    fn role_of_finds_the_role_of_the_items_column() {
        let b = board(
            vec![column("t", "Todo"), column("p", "Paused"), column("r", "Ready")],
            vec![item("ready", Some("r")), item("paused", Some("p")), item("none", None)],
        );
        let mapping = suggest_mapping(&b.columns);
        assert_eq!(b.role_of(&b.items[0], &mapping), Some(ColumnRole::Ready));
        assert_eq!(b.role_of(&b.items[1], &mapping), None);
        assert_eq!(b.role_of(&b.items[2], &mapping), None);
    }

    #[test]
    fn effective_mapping_keeps_stored_choices_and_fills_the_rest() {
        let b = board(
            vec![
                column("a", "Backlog"),
                column("b", "Todo"),
                column("c", "Ready"),
            ],
            vec![],
        );
        let stored = ColumnMapping::from([
            (ColumnRole::Todo, "Backlog".to_string()),
            (ColumnRole::InReview, "Review".to_string()),
        ]);
        let effective = effective_mapping(&b, &stored);
        assert_eq!(
            effective.get(&ColumnRole::Todo).map(String::as_str),
            Some("Backlog")
        );
        assert_eq!(
            effective.get(&ColumnRole::Ready).map(String::as_str),
            Some("Ready")
        );
        assert_eq!(effective.get(&ColumnRole::InReview), None);
    }

    #[test]
    fn board_settings_round_trip_with_camel_case_role_keys() {
        let settings = BoardSettings {
            url: "https://github.com/users/maui1911/projects/3".into(),
            columns: ColumnMapping::from([(ColumnRole::NeedsDecision, "Needs decision".into())]),
            added_at: "2026-11-25T10:00:00Z".into(),
            ..BoardSettings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(
            json.contains(r#""needsDecision":"Needs decision""#),
            "{json}"
        );
        assert_eq!(
            serde_json::from_str::<BoardSettings>(&json).unwrap(),
            settings
        );
    }

    #[test]
    fn board_settings_skip_unknown_roles_and_keep_unknown_keys() {
        let json = r#"{"url":"https://github.com/users/maui1911/projects/3",
            "columns":{"ready":"Ready","inreview":"Review","todo":5},
            "addedAt":"2026-11-25T10:00:00Z"}"#;
        let settings: BoardSettings = serde_json::from_str(json).unwrap();
        assert_eq!(
            settings.columns,
            ColumnMapping::from([(ColumnRole::Ready, "Ready".to_string())])
        );
        let saved = serde_json::to_string(&settings).unwrap();
        assert!(saved.contains(r#""addedAt":"2026-11-25T10:00:00Z""#), "{saved}");

        let settings: BoardSettings =
            serde_json::from_str(r#"{"url":"u","columns":"Ready"}"#).unwrap();
        assert!(settings.columns.is_empty());
    }
}
