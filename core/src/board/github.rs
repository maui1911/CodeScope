//! A GitHub Projects board, read with `gh api graphql`. One page of 100 items costs one point of
//! the 5,000-point hourly limit; `gh project item-list` would cost about a hundred.

use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;

use super::{Board, BoardItem, BoardOwner, BoardRef, Column, ItemKind, ItemState};
use crate::process::no_window_command;

const MAX_PAGES_PER_READ: usize = 10;

/// The query for one page of a board's items; `$cursor` is the previous page's end.
pub fn board_query(owner_kind: BoardOwner) -> String {
    let owner = match owner_kind {
        BoardOwner::User => "user",
        BoardOwner::Organization => "organization",
    };
    format!(
        r#"query($owner: String!, $number: Int!, $cursor: String) {{
  {owner}(login: $owner) {{
    projectV2(number: $number) {{
      title url closed
      field(name: "Status") {{ ... on ProjectV2SingleSelectField {{ options {{ id name }} }} }}
      items(first: 100, after: $cursor) {{
        pageInfo {{ hasNextPage endCursor }}
        nodes {{
          id isArchived updatedAt
          fieldValueByName(name: "Status") {{ ... on ProjectV2ItemFieldSingleSelectValue {{ optionId }} }}
          content {{
            __typename
            ... on DraftIssue {{ title }}
            ... on Issue {{ number title url state repository {{ nameWithOwner }} }}
            ... on PullRequest {{ number title url state repository {{ nameWithOwner }} }}
          }}
        }}
      }}
    }}
  }}
}}"#
    )
}

/// One page of a board: the board with this page's items, and the cursor of the next page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoardPage {
    pub board: Board,
    pub next_cursor: Option<String>,
}

/// Read the board, page by page, up to [`MAX_PAGES_PER_READ`] pages.
pub fn fetch_board(board: &BoardRef) -> Result<Board> {
    let query = board_query(board.owner_kind);
    read_pages(|cursor| run_query(&query, board, cursor))
}

fn read_pages(mut fetch_page: impl FnMut(Option<&str>) -> Result<String>) -> Result<Board> {
    let mut cursor: Option<String> = None;
    let mut read: Option<Board> = None;
    for _ in 0..MAX_PAGES_PER_READ {
        let page = parse_page(&fetch_page(cursor.as_deref())?)?;
        match read.as_mut() {
            Some(read) => read.items.extend(page.board.items),
            None => read = Some(page.board),
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    let mut read = read.ok_or_else(|| anyhow!("the board returned no pages"))?;
    read.truncated = cursor.is_some();
    Ok(read)
}

/// `gh api` against github.com, whatever `GH_HOST` says.
pub fn gh_api() -> std::process::Command {
    let mut command = no_window_command("gh");
    command.args(["api", "--hostname", "github.com"]);
    command
}

fn run_query(query: &str, board: &BoardRef, cursor: Option<&str>) -> Result<String> {
    let mut command = gh_api();
    command
        .arg("graphql")
        .arg("-f")
        .arg(format!("query={query}"))
        .arg("-f")
        .arg(format!("owner={}", board.owner))
        .arg("-F")
        .arg(format!("number={}", board.number));
    if let Some(cursor) = cursor {
        command.arg("-f").arg(format!("cursor={cursor}"));
    }
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => anyhow!("GitHub CLI (gh) is not installed"),
            _ => anyhow!(err).context("run gh"),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("gh failed")
            .trim();
        bail!("{}", message.strip_prefix("gh: ").unwrap_or(message));
    }
    String::from_utf8(output.stdout).context("gh printed something other than UTF-8")
}

/// Parse one page of [`board_query`]'s response.
pub fn parse_page(json: &str) -> Result<BoardPage> {
    let response: Response =
        serde_json::from_str(json).context("parse the board query response")?;
    if let Some(errors) = response.errors.filter(|e| !e.is_empty()) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        bail!("{}", messages.join("; "));
    }
    let data = response
        .data
        .ok_or_else(|| anyhow!("the response has no data"))?;
    let project = data
        .user
        .or(data.organization)
        .and_then(|owner| owner.project_v2)
        .ok_or_else(|| anyhow!("board not found"))?;
    let options = project
        .field
        .and_then(|field| field.options)
        .ok_or_else(|| anyhow!("the board has no single-select Status field"))?;
    let columns = options
        .into_iter()
        .map(|o| Column {
            id: o.id,
            name: o.name,
        })
        .collect();
    let items = project
        .items
        .nodes
        .into_iter()
        .flatten()
        .filter_map(into_item)
        .collect();
    let next_cursor = project
        .items
        .page_info
        .has_next_page
        .then_some(project.items.page_info.end_cursor)
        .flatten();
    Ok(BoardPage {
        board: Board {
            title: project.title,
            url: project.url,
            closed: project.closed,
            columns,
            items,
            truncated: false,
        },
        next_cursor,
    })
}

fn into_item(node: ItemNode) -> Option<BoardItem> {
    if node.is_archived {
        return None;
    }
    let column_id = node.field_value_by_name.and_then(|v| v.option_id);
    let (kind, number, title, url, repository, state) = match node.content? {
        Content::DraftIssue { title } => {
            (ItemKind::Draft, None, title, None, None, ItemState::Open)
        }
        Content::Issue(c) => (
            ItemKind::Issue,
            Some(c.number),
            c.title,
            Some(c.url),
            Some(c.repository.name_with_owner),
            item_state(&c.state),
        ),
        Content::PullRequest(c) => (
            ItemKind::PullRequest,
            Some(c.number),
            c.title,
            Some(c.url),
            Some(c.repository.name_with_owner),
            item_state(&c.state),
        ),
        Content::Other => return None,
    };
    Some(BoardItem {
        id: node.id,
        kind,
        number,
        title,
        url,
        repository,
        state,
        column_id,
        updated_at: node.updated_at,
    })
}

fn item_state(state: &str) -> ItemState {
    match state {
        "CLOSED" => ItemState::Closed,
        "MERGED" => ItemState::Merged,
        _ => ItemState::Open,
    }
}

#[derive(Deserialize)]
struct Response {
    data: Option<Data>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Deserialize)]
struct Data {
    user: Option<Owner>,
    organization: Option<Owner>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Owner {
    project_v2: Option<Project>,
}

#[derive(Deserialize)]
struct Project {
    title: String,
    url: String,
    closed: bool,
    field: Option<StatusField>,
    items: Items,
}

#[derive(Deserialize)]
struct StatusField {
    options: Option<Vec<StatusOption>>,
}

#[derive(Deserialize)]
struct StatusOption {
    id: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Items {
    page_info: PageInfo,
    nodes: Vec<Option<ItemNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemNode {
    id: String,
    is_archived: bool,
    updated_at: String,
    field_value_by_name: Option<StatusValue>,
    content: Option<Content>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusValue {
    option_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum Content {
    DraftIssue {
        title: String,
    },
    Issue(LinkedContent),
    PullRequest(LinkedContent),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkedContent {
    number: u32,
    title: String,
    url: String,
    state: String,
    repository: Repository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    name_with_owner: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = r#"{"data":{"user":{"projectV2":{
      "title":"CodeScope","url":"https://github.com/users/maui1911/projects/3","closed":false,
      "field":{"options":[{"id":"f1","name":"Todo"},{"id":"f2","name":"Ready"}]},
      "items":{"pageInfo":{"hasNextPage":true,"endCursor":"Y3Vy"},"nodes":[
        {"id":"PVTI_1","isArchived":false,"updatedAt":"2026-11-20T00:55:34Z",
         "fieldValueByName":{"optionId":"f2"},
         "content":{"__typename":"Issue","number":411,"title":"Fuzzy paths",
           "url":"https://github.com/maui1911/CodeScope/issues/411","state":"OPEN",
           "repository":{"nameWithOwner":"maui1911/CodeScope"}}},
        {"id":"PVTI_2","isArchived":false,"updatedAt":"2026-11-21T00:00:00Z",
         "fieldValueByName":null,
         "content":{"__typename":"PullRequest","number":414,"title":"Group the bell",
           "url":"https://github.com/maui1911/CodeScope/pull/414","state":"MERGED",
           "repository":{"nameWithOwner":"maui1911/CodeScope"}}},
        {"id":"PVTI_3","isArchived":false,"updatedAt":"2026-11-22T00:00:00Z",
         "fieldValueByName":{},
         "content":{"__typename":"DraftIssue","title":"Theme picker"}},
        {"id":"PVTI_4","isArchived":true,"updatedAt":"2026-11-22T00:00:00Z",
         "fieldValueByName":null,
         "content":{"__typename":"DraftIssue","title":"Archived"}},
        {"id":"PVTI_5","isArchived":false,"updatedAt":"2026-11-22T00:00:00Z",
         "fieldValueByName":null,"content":null}
      ]}}}}}"#;

    #[test]
    fn query_asks_the_owner_kind_the_url_names() {
        assert!(board_query(BoardOwner::User).contains("user(login: $owner)"));
        assert!(board_query(BoardOwner::Organization).contains("organization(login: $owner)"));
    }

    #[test]
    fn parses_columns_items_and_the_next_cursor() {
        let page = parse_page(PAGE).unwrap();
        assert_eq!(page.next_cursor.as_deref(), Some("Y3Vy"));
        let board = page.board;
        assert_eq!(board.title, "CodeScope");
        assert_eq!(
            board.columns,
            vec![
                Column {
                    id: "f1".into(),
                    name: "Todo".into()
                },
                Column {
                    id: "f2".into(),
                    name: "Ready".into()
                },
            ]
        );
        assert_eq!(board.items.len(), 3);

        let issue = &board.items[0];
        assert_eq!(issue.kind, ItemKind::Issue);
        assert_eq!(issue.number, Some(411));
        assert_eq!(issue.repository.as_deref(), Some("maui1911/CodeScope"));
        assert_eq!(issue.column_id.as_deref(), Some("f2"));
        assert_eq!(issue.state, ItemState::Open);

        let pr = &board.items[1];
        assert_eq!(pr.kind, ItemKind::PullRequest);
        assert_eq!(pr.state, ItemState::Merged);
        assert_eq!(pr.column_id, None);

        let draft = &board.items[2];
        assert_eq!(draft.kind, ItemKind::Draft);
        assert_eq!(draft.number, None);
        assert_eq!(draft.title, "Theme picker");
        assert_eq!(draft.column_id, None);
    }

    #[test]
    fn api_calls_go_to_github_com() {
        let command = gh_api();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["api", "--hostname", "github.com"]);
    }

    #[test]
    fn a_null_item_is_skipped() {
        let json = PAGE.replace(r#""nodes":["#, r#""nodes":[null,"#);
        assert_eq!(parse_page(&json).unwrap().board.items.len(), 3);
    }

    #[test]
    fn a_board_larger_than_one_read_says_so() {
        let mut pages = 0;
        let board = read_pages(|_| {
            pages += 1;
            Ok(PAGE.to_string())
        })
        .unwrap();
        assert_eq!(pages, MAX_PAGES_PER_READ);
        assert!(board.truncated);
        assert_eq!(board.items.len(), 3 * MAX_PAGES_PER_READ);

        let last = PAGE.replace(r#""hasNextPage":true"#, r#""hasNextPage":false"#);
        let mut cursors = Vec::new();
        let board = read_pages(|cursor| {
            cursors.push(cursor.map(str::to_string));
            Ok(if cursors.len() == 1 { PAGE.to_string() } else { last.clone() })
        })
        .unwrap();
        assert!(!board.truncated);
        assert_eq!(cursors, [None, Some("Y3Vy".to_string())]);
    }

    #[test]
    fn last_page_has_no_cursor() {
        let json = PAGE.replace(r#""hasNextPage":true"#, r#""hasNextPage":false"#);
        assert_eq!(parse_page(&json).unwrap().next_cursor, None);
    }

    #[test]
    fn a_board_without_a_status_field_is_an_error() {
        let json = PAGE.replace(
            r#""field":{"options":[{"id":"f1","name":"Todo"},{"id":"f2","name":"Ready"}]}"#,
            r#""field":null"#,
        );
        let err = parse_page(&json).unwrap_err();
        assert!(err.to_string().contains("Status field"), "{err}");
    }

    #[test]
    fn graphql_errors_are_reported() {
        let json = r#"{"data":{"user":{"projectV2":null}},"errors":[
          {"type":"NOT_FOUND","message":"Could not resolve to a ProjectV2 with the number 9."}]}"#;
        let err = parse_page(json).unwrap_err();
        assert!(err.to_string().contains("number 9"), "{err}");
    }

    #[test]
    fn a_missing_board_is_an_error() {
        let err = parse_page(r#"{"data":{"organization":{"projectV2":null}}}"#).unwrap_err();
        assert_eq!(err.to_string(), "board not found");
    }
}
