//! A GitHub Projects board, read with `gh api graphql`. One page of 100 items costs one point of
//! the 5,000-point hourly limit; `gh project item-list` would cost about a hundred.

use std::collections::HashMap;
use std::io::Write as _;
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde::de::DeserializeOwned;

use super::answers::Comment;
use super::dispatch::StatusChange;
use super::pull_request::{
    Checks, FailedCheck, Health, LinkedPullRequest, Mergeable, PullRequest, PullRequestComment,
    PullRequestHead, PullRequestState,
};
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
      id title url closed
      field(name: "Status") {{ ... on ProjectV2SingleSelectField {{ id options {{ id name }} }} }}
      items(first: 100, after: $cursor) {{
        pageInfo {{ hasNextPage endCursor }}
        nodes {{
          id isArchived updatedAt
          fieldValueByName(name: "Status") {{ ... on ProjectV2ItemFieldSingleSelectValue {{ optionId updatedAt }} }}
          content {{
            __typename
            ... on DraftIssue {{ title }}
            ... on Issue {{ id number title url state repository {{ nameWithOwner }} assignees(first: 10) {{ nodes {{ login }} }} }}
            ... on PullRequest {{ id number title url state repository {{ nameWithOwner }} assignees(first: 10) {{ nodes {{ login }} }} }}
          }}
        }}
      }}
    }}
  }}
}}"#
    )
}

/// The board workflow that moves an issue to a column of its own when a pull request is linked to
/// it, which can take the item back out of *In review*.
pub const LINKED_PULL_REQUEST_WORKFLOW: &str = "Pull request linked to issue";

/// The query for the board's built-in workflows, apart from [`board_query`]: GitHub refuses them
/// to anyone who can't change the board, and that refusal empties the whole board.
pub fn workflows_query(owner_kind: BoardOwner) -> String {
    let owner = match owner_kind {
        BoardOwner::User => "user",
        BoardOwner::Organization => "organization",
    };
    format!(
        r#"query($owner: String!, $number: Int!) {{
  {owner}(login: $owner) {{
    projectV2(number: $number) {{ workflows(first: 50) {{ nodes {{ name enabled }} }} }}
  }}
}}"#
    )
}

/// The names of the board's built-in workflows that are on.
pub fn fetch_enabled_workflows(board: &BoardRef) -> Result<Vec<String>> {
    let query = workflows_query(board.owner_kind);
    let fields = [format!("query={query}"), format!("owner={}", board.owner)];
    parse_enabled_workflows(&run_graphql(&fields, &[format!("number={}", board.number)])?)
}

/// Parse [`workflows_query`]'s response.
pub fn parse_enabled_workflows(json: &str) -> Result<Vec<String>> {
    let data: WorkflowsData = parse_data(json)?;
    let project = data
        .user
        .or(data.organization)
        .and_then(|owner| owner.project_v2)
        .ok_or_else(|| anyhow!("board not found"))?;
    Ok(project
        .workflows
        .nodes
        .into_iter()
        .flatten()
        .filter(|workflow| workflow.enabled)
        .map(|workflow| workflow.name)
        .collect())
}

/// Whether workflows that are on include [`LINKED_PULL_REQUEST_WORKFLOW`].
pub fn moves_back_from_review(enabled: &[String]) -> bool {
    enabled.iter().any(|name| name == LINKED_PULL_REQUEST_WORKFLOW)
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
    read_pages(|cursor| {
        let mut fields = vec![format!("query={query}"), format!("owner={}", board.owner)];
        if let Some(cursor) = cursor {
            fields.push(format!("cursor={cursor}"));
        }
        run_graphql(&fields, &[format!("number={}", board.number)])
    })
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

/// The signed-in `gh` user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewer {
    pub login: String,
    /// The user's node id, which assigning needs.
    pub id: String,
}

pub fn fetch_viewer() -> Result<Viewer> {
    let stdout = run_graphql(&["query={ viewer { login id } }".to_string()], &[])?;
    let data: ViewerData = parse_data(&stdout)?;
    Ok(Viewer {
        login: data.viewer.login,
        id: data.viewer.id,
    })
}

const TRIGGER_ITEMS_QUERY: &str = r#"query($ids: [ID!]!) {
  nodes(ids: $ids) {
    __typename
    ... on Issue {
      id timelineItems(last: 100, itemTypes: [PROJECT_V2_ITEM_STATUS_CHANGED_EVENT]) { nodes { ...change } }
      closedByPullRequestsReferences(first: 10, includeClosedPrs: true) { nodes { ...linked } }
    }
    ... on PullRequest {
      id headRefName isCrossRepository maintainerCanModify headRepository { nameWithOwner }
      timelineItems(last: 100, itemTypes: [PROJECT_V2_ITEM_STATUS_CHANGED_EVENT]) { nodes { ...change } }
    }
  }
}
fragment change on ProjectV2ItemStatusChangedEvent {
  createdAt status wasAutomated actor { login } project { id }
}
fragment linked on PullRequest {
  number state createdAt closedAt isCrossRepository headRefName
}"#;

/// What the board poll reads about the issues and pull requests in trigger columns, by content
/// id.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TriggerReads {
    /// The last status change on the board; items without one are left out.
    pub changes: HashMap<String, StatusChange>,
    /// Each issue's linked pull requests, open or not, up to ten.
    pub linked: HashMap<String, Vec<LinkedPullRequest>>,
    /// The branch each pull request merges from.
    pub heads: HashMap<String, PullRequestHead>,
}

/// The last status change on board `board_id` of each issue or pull request in `content_ids`, the
/// linked pull requests of each issue, and the branch of each pull request.
pub fn fetch_trigger_reads(board_id: &str, content_ids: &[String]) -> Result<TriggerReads> {
    let mut reads = TriggerReads::default();
    for chunk in content_ids.chunks(100) {
        let mut args = vec![format!("query={TRIGGER_ITEMS_QUERY}")];
        args.extend(chunk.iter().map(|id| format!("ids[]={id}")));
        let stdout = run_graphql(&args, &[])?;
        let page = parse_trigger_reads(&stdout, board_id)?;
        reads.changes.extend(page.changes);
        reads.linked.extend(page.linked);
        reads.heads.extend(page.heads);
    }
    Ok(reads)
}

/// Parse [`fetch_trigger_reads`]' response, keeping the changes made on `board_id`.
pub fn parse_trigger_reads(json: &str, board_id: &str) -> Result<TriggerReads> {
    let data: NodesData = parse_data(json)?;
    let mut reads = TriggerReads::default();
    for node in data.nodes.into_iter().flatten() {
        let Some(id) = node.id else {
            continue;
        };
        let change = node.timeline_items.and_then(|items| {
            items
                .nodes
                .into_iter()
                .flatten()
                .rfind(|e| e.project.as_ref().is_some_and(|p| p.id == board_id))
        });
        if let Some(change) = change {
            let change = StatusChange {
                actor: change.actor.map(|a| a.login),
                was_automated: change.was_automated,
                status: change.status,
                created_at: change.created_at,
            };
            reads.changes.insert(id.clone(), change);
        }
        if let (Some(branch), Some(is_cross_repository)) =
            (node.head_ref_name, node.is_cross_repository)
        {
            let head = PullRequestHead {
                branch,
                is_cross_repository,
                repository: node.head_repository.map(|r| r.name_with_owner),
                maintainer_can_modify: node.maintainer_can_modify.unwrap_or(false),
            };
            reads.heads.insert(id.clone(), head);
        }
        if let Some(linked) = node.closed_by_pull_requests_references {
            let linked = linked
                .nodes
                .into_iter()
                .flatten()
                .map(|pr| LinkedPullRequest {
                    number: pr.number,
                    open: pr.state == "OPEN",
                    head_ref_name: pr.head_ref_name,
                    is_cross_repository: pr.is_cross_repository,
                    created_at: pr.created_at,
                    closed_at: pr.closed_at,
                })
                .collect();
            reads.linked.insert(id, linked);
        }
    }
    Ok(reads)
}

const COMMENTS_QUERY: &str = r#"query($ids: [ID!]!) {
  nodes(ids: $ids) {
    __typename
    ... on Issue { id comments(last: 50) { nodes { ...comment } } }
    ... on PullRequest { id comments(last: 50) { nodes { ...comment } } }
  }
}
fragment comment on IssueComment { url body createdAt author { login } }"#;

/// The last 50 comments of each issue or pull request in `content_ids`, oldest first, keyed by
/// content id.
pub fn fetch_comments(content_ids: &[String]) -> Result<HashMap<String, Vec<Comment>>> {
    let mut comments = HashMap::new();
    for chunk in content_ids.chunks(100) {
        let mut args = vec![format!("query={COMMENTS_QUERY}")];
        args.extend(chunk.iter().map(|id| format!("ids[]={id}")));
        let stdout = run_graphql(&args, &[])?;
        comments.extend(parse_comments(&stdout)?);
    }
    Ok(comments)
}

/// Parse [`fetch_comments`]' response.
pub fn parse_comments(json: &str) -> Result<HashMap<String, Vec<Comment>>> {
    let data: CommentsData = parse_data(json)?;
    Ok(data
        .nodes
        .into_iter()
        .flatten()
        .filter_map(|node| {
            let comments = node
                .comments?
                .nodes
                .into_iter()
                .flatten()
                .map(|c| Comment {
                    url: c.url,
                    author: c.author.map(|a| a.login),
                    body: c.body,
                    created_at: c.created_at,
                })
                .collect();
            Some((node.id?, comments))
        })
        .collect())
}

/// Put an item in a column.
pub fn set_status(board: &Board, item_id: &str, column_id: &str) -> Result<()> {
    let query = r#"mutation($board: ID!, $item: ID!, $field: ID!, $option: String!) {
  updateProjectV2ItemFieldValue(input: { projectId: $board, itemId: $item, fieldId: $field,
    value: { singleSelectOptionId: $option } }) { clientMutationId }
}"#;
    let stdout = run_graphql(
        &[
            format!("query={query}"),
            format!("board={}", board.id),
            format!("item={item_id}"),
            format!("field={}", board.status_field_id),
            format!("option={column_id}"),
        ],
        &[],
    )?;
    parse_data::<serde_json::Value>(&stdout).map(|_| ())
}

/// Assign an issue or pull request to a user, by node ids.
pub fn assign(content_id: &str, user_id: &str) -> Result<()> {
    let query = r#"mutation($item: ID!, $user: ID!) {
  addAssigneesToAssignable(input: { assignableId: $item, assigneeIds: [$user] }) { clientMutationId }
}"#;
    let stdout = run_graphql(
        &[
            format!("query={query}"),
            format!("item={content_id}"),
            format!("user={user_id}"),
        ],
        &[],
    )?;
    parse_data::<serde_json::Value>(&stdout).map(|_| ())
}

/// Comment on an issue or pull request by its node id; returns the comment's URL. The request
/// goes through stdin, so a long body is not cut off by the command line's length limit.
pub fn add_comment(subject_id: &str, body: &str) -> Result<NewComment> {
    let query = r#"mutation($subject: ID!, $body: String!) {
  addComment(input: { subjectId: $subject, body: $body }) {
    commentEdge { node { url createdAt } }
  }
}"#;
    let request = serde_json::json!({
        "query": query,
        "variables": { "subject": subject_id, "body": body },
    });
    let args = ["graphql", "--input", "-"].map(String::from);
    let stdout = run_gh_api_with_input(&args, Some(request.to_string()))?;
    parse_new_comment(&stdout)
}

/// A comment [`add_comment`] posted.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewComment {
    pub url: String,
    /// ISO 8601 in UTC, by GitHub's clock.
    pub created_at: String,
}

/// Parse [`add_comment`]'s response.
pub fn parse_new_comment(json: &str) -> Result<NewComment> {
    let data: AddCommentData = parse_data(json)?;
    Ok(data.add_comment.comment_edge.node)
}

const BEFORE_POSTING_QUERY: &str = r#"query($item: ID!, $subject: ID!) {
  item: node(id: $item) {
    ... on ProjectV2Item {
      fieldValueByName(name: "Status") { ... on ProjectV2ItemFieldSingleSelectValue { optionId } }
    }
  }
  subject: node(id: $subject) {
    ... on Issue { comments(last: 50) { ...Posted } }
    ... on PullRequest { comments(last: 50) { ...Posted } }
  }
}
fragment Posted on IssueCommentConnection { nodes { url createdAt viewerDidAuthor body } }"#;

/// What posting a research round's findings checks on GitHub first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BeforePosting {
    /// The column the item is in now; `None` without a status or off the board.
    pub column_id: Option<String>,
    /// The URL of this very comment, when the signed-in user already posted it this round.
    pub posted: Option<String>,
}

/// Read the board item `item_id`'s column, and look among the last 50 comments on the issue or
/// pull request `subject_id` for `body` posted by the signed-in user after `since`.
pub fn before_posting(
    item_id: &str,
    subject_id: &str,
    since: &str,
    body: &str,
) -> Result<BeforePosting> {
    let args = graphql_args(
        &[
            format!("query={BEFORE_POSTING_QUERY}"),
            format!("item={item_id}"),
            format!("subject={subject_id}"),
        ],
        &[],
    );
    let (stdout, failure) = gh_output(&args, None)?;
    parse_before_posting(&String::from_utf8_lossy(&stdout), since, body).map_err(|err| {
        match failure {
            Some(message) => anyhow!(message),
            None => err,
        }
    })
}

/// Parse [`before_posting`]'s response, in which an item off the board is `NOT_FOUND`; `since`
/// is ISO 8601, as GitHub writes `createdAt`.
pub fn parse_before_posting(json: &str, since: &str, body: &str) -> Result<BeforePosting> {
    let response: Response<BeforePostingData> =
        serde_json::from_str(json).context("parse the gh response")?;
    let unexpected: Vec<String> = response
        .errors
        .unwrap_or_default()
        .into_iter()
        .filter(|error| !error.is_not_found("item"))
        .map(|error| error.message)
        .collect();
    if !unexpected.is_empty() {
        bail!("{}", unexpected.join("; "));
    }
    let data = response.data.ok_or_else(|| anyhow!("the response has no data"))?;
    let same = |posted: &str| posted.replace("\r\n", "\n").trim_end() == body.trim_end();
    let posted = data
        .subject
        .and_then(|subject| subject.comments)
        .into_iter()
        .flat_map(|comments| comments.nodes.into_iter().flatten())
        .filter(|c| c.viewer_did_author && c.created_at.as_str() > since)
        .rfind(|c| same(&c.body))
        .map(|c| c.url);
    Ok(BeforePosting {
        column_id: data.item.and_then(|item| item.field_value_by_name?.option_id),
        posted,
    })
}

/// Whether `login` can push to `repository` (`owner/name`): admin, maintain or write. False too
/// when GitHub won't say, which it does unless the signed-in user can push.
pub fn can_write(repository: &str, login: &str) -> Result<bool> {
    write_access(run_gh_api(&[
        format!("repos/{repository}/collaborators/{login}/permission"),
        "--jq".to_string(),
        ".permission".to_string(),
    ]))
}

fn write_access(permission: Result<String>) -> Result<bool> {
    match permission {
        Ok(permission) => Ok(matches!(permission.trim(), "admin" | "maintain" | "write")),
        Err(err) => {
            let message = err.to_string();
            let refused = message.contains("HTTP 404")
                || (message.contains("HTTP 403") && message.contains("Must have push access"));
            if refused { Ok(false) } else { Err(err) }
        }
    }
}

const PULL_REQUESTS_QUERY: &str = r#"query($owner: String!, $name: String!, $branch: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(headRefName: $branch, states: [OPEN, MERGED], first: 5,
      orderBy: { field: CREATED_AT, direction: DESC }) {
      nodes {
        id number url state isDraft isCrossRepository headRefOid
        baseRefName mergeable mergeStateStatus
        reviews(states: [APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED]) { totalCount }
        comments(last: 50) { ...conversation }
        commits(last: 1) { nodes { commit { statusCheckRollup { state contexts(first: 100) {
          nodes {
            __typename
            ... on CheckRun { name status conclusion detailsUrl }
            ... on StatusContext { context state targetUrl }
          }
        } } } } }
      }
    }
  }
}
fragment conversation on IssueCommentConnection {
  nodes { url createdAt viewerDidAuthor author { __typename login } }
}"#;

/// The open and merged pull requests from `branch` of `repository` (`owner/name`) itself, newest
/// first; those from forks with a branch of the same name are left out.
pub fn fetch_pull_requests(repository: &str, branch: &str) -> Result<Vec<PullRequest>> {
    let (owner, name) = repository
        .split_once('/')
        .ok_or_else(|| anyhow!("{repository} is not an owner/name repository"))?;
    let stdout = run_graphql(
        &[
            format!("query={PULL_REQUESTS_QUERY}"),
            format!("owner={owner}"),
            format!("name={name}"),
            format!("branch={branch}"),
        ],
        &[],
    )?;
    parse_pull_requests(&stdout)
}

/// Parse [`fetch_pull_requests`]' response.
pub fn parse_pull_requests(json: &str) -> Result<Vec<PullRequest>> {
    let data: PullRequestsData = parse_data(json)?;
    let repository = data
        .repository
        .ok_or_else(|| anyhow!("repository not found"))?;
    Ok(repository
        .pull_requests
        .nodes
        .into_iter()
        .flatten()
        .filter(|node| !node.is_cross_repository)
        .filter_map(into_pull_request)
        .collect())
}

fn into_pull_request(node: PullRequestNode) -> Option<PullRequest> {
    let state = match node.state.as_str() {
        "OPEN" => PullRequestState::Open,
        "MERGED" => PullRequestState::Merged,
        _ => return None,
    };
    let rollup = node
        .commits
        .nodes
        .into_iter()
        .flatten()
        .next()
        .and_then(|c| c.commit.status_check_rollup);
    let checks = Checks::from_rollup(rollup.as_ref().map(|r| r.state.as_str()));
    let contexts = rollup
        .and_then(|r| r.contexts)
        .map(|c| c.nodes.into_iter().flatten().collect::<Vec<_>>())
        .unwrap_or_default();
    let health = Health {
        base: node.base_ref_name.unwrap_or_default(),
        mergeable: match node.mergeable.as_deref() {
            Some("MERGEABLE") => Mergeable::Yes,
            Some("CONFLICTING") => Mergeable::Conflicts,
            _ => Mergeable::Unknown,
        },
        behind: node.merge_state_status.as_deref() == Some("BEHIND"),
        reviewed: node.reviews.is_some_and(|r| r.total_count > 0),
        checks_running: contexts.iter().any(CheckContext::running),
        failed_checks: contexts.into_iter().filter_map(CheckContext::failed).collect(),
    };
    Some(PullRequest {
        id: node.id,
        number: node.number,
        url: node.url,
        state,
        is_draft: node.is_draft,
        checks,
        head_oid: node.head_ref_oid,
        health,
        comments: node.comments.map(conversation).unwrap_or_default(),
    })
}

fn conversation(comments: PullRequestComments) -> Vec<PullRequestComment> {
    comments
        .nodes
        .into_iter()
        .flatten()
        .map(|comment| PullRequestComment {
            url: comment.url,
            author: comment.author.filter(|a| a.typename == "User").map(|a| a.login),
            created_at: comment.created_at,
            by_viewer: comment.viewer_did_author,
        })
        .collect()
}

const PULL_REQUEST_QUERY: &str = r#"query($id: ID!) {
  node(id: $id) {
    ... on PullRequest {
      id number url state isDraft isCrossRepository headRefOid
      baseRefName mergeable mergeStateStatus
      reviews(states: [APPROVED, CHANGES_REQUESTED, COMMENTED, DISMISSED]) { totalCount }
      comments(last: 50) { ...conversation }
      commits(last: 1) { nodes { commit { statusCheckRollup { state contexts(first: 100) {
        nodes {
          __typename
          ... on CheckRun { name status conclusion detailsUrl }
          ... on StatusContext { context state targetUrl }
        }
      } } } } }
    }
  }
}
fragment conversation on IssueCommentConnection {
  nodes { url createdAt viewerDidAuthor author { __typename login } }
}"#;

/// The pull request with node id `id`; `None` when it was closed without a merge.
pub fn fetch_pull_request(id: &str) -> Result<Option<PullRequest>> {
    let stdout = run_graphql(&[format!("query={PULL_REQUEST_QUERY}"), format!("id={id}")], &[])?;
    parse_pull_request(&stdout)
}

/// Parse [`fetch_pull_request`]'s response.
pub fn parse_pull_request(json: &str) -> Result<Option<PullRequest>> {
    let data: PullRequestData = parse_data(json)?;
    let node = data.node.ok_or_else(|| anyhow!("pull request not found"))?;
    Ok(into_pull_request(node))
}

const THREAD_REPLIES_QUERY: &str = r#"query($id: ID!) {
  node(id: $id) {
    ... on PullRequest {
      reviewThreads(last: 50) {
        nodes { comments(last: 10) { nodes { viewerDidAuthor createdAt } } }
      }
    }
  }
}"#;

/// When the signed-in user last commented in a review thread of the pull request with node id
/// `pull_request_id`, ISO 8601; looks at the last 10 comments of its last 50 threads.
pub fn fetch_last_thread_reply(pull_request_id: &str) -> Result<Option<String>> {
    let stdout = run_graphql(
        &[
            format!("query={THREAD_REPLIES_QUERY}"),
            format!("id={pull_request_id}"),
        ],
        &[],
    )?;
    parse_last_thread_reply(&stdout)
}

/// Parse [`fetch_last_thread_reply`]'s response.
pub fn parse_last_thread_reply(json: &str) -> Result<Option<String>> {
    let data: ThreadRepliesData = parse_data(json)?;
    let threads = data.node.ok_or_else(|| anyhow!("pull request not found"))?;
    Ok(threads
        .review_threads
        .nodes
        .into_iter()
        .flatten()
        .flat_map(|thread| thread.comments.nodes.into_iter().flatten())
        .filter(|comment| comment.viewer_did_author)
        .map(|comment| comment.created_at)
        .max_by(|a, b| {
            let secs = |at: &str| crate::time::parse_iso8601_secs(at).unwrap_or(f64::MIN);
            secs(a).total_cmp(&secs(b))
        }))
}

/// Create `branch` at commit `oid` on the repository of issue `issue_id` (a node id), linked to
/// that issue, so a pull request from it closes the issue.
pub fn create_linked_branch(issue_id: &str, oid: &str, branch: &str) -> Result<()> {
    let query = r#"mutation($issue: ID!, $oid: GitObjectID!, $name: String!) {
  createLinkedBranch(input: { issueId: $issue, oid: $oid, name: $name }) { linkedBranch { id } }
}"#;
    let stdout = run_graphql(
        &[
            format!("query={query}"),
            format!("issue={issue_id}"),
            format!("oid={oid}"),
            format!("name={branch}"),
        ],
        &[],
    )?;
    parse_linked_branch(&stdout, branch)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkedBranchData {
    create_linked_branch: Option<LinkedBranchPayload>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkedBranchPayload {
    linked_branch: Option<serde_json::Value>,
}

fn parse_linked_branch(json: &str, branch: &str) -> Result<()> {
    let data: LinkedBranchData = parse_data(json)?;
    let made = data.create_linked_branch.and_then(|p| p.linked_branch);
    if made.is_none() {
        bail!("GitHub didn't make {branch}");
    }
    Ok(())
}

/// Mark a draft pull request ready for review, by its node id.
pub fn mark_ready_for_review(pull_request_id: &str) -> Result<()> {
    let query = r#"mutation($id: ID!) {
  markPullRequestReadyForReview(input: { pullRequestId: $id }) { clientMutationId }
}"#;
    let stdout = run_graphql(
        &[format!("query={query}"), format!("id={pull_request_id}")],
        &[],
    )?;
    parse_data::<serde_json::Value>(&stdout).map(|_| ())
}

fn run_graphql(raw_fields: &[String], typed_fields: &[String]) -> Result<String> {
    run_gh_api(&graphql_args(raw_fields, typed_fields))
}

fn graphql_args(raw_fields: &[String], typed_fields: &[String]) -> Vec<String> {
    let mut args = vec!["graphql".to_string()];
    for field in raw_fields {
        args.push("-f".to_string());
        args.push(field.clone());
    }
    for field in typed_fields {
        args.push("-F".to_string());
        args.push(field.clone());
    }
    args
}

fn run_gh_api(args: &[String]) -> Result<String> {
    run_gh_api_with_input(args, None)
}

fn run_gh_api_with_input(args: &[String], input: Option<String>) -> Result<String> {
    let (stdout, failure) = gh_output(args, input)?;
    if let Some(message) = failure {
        bail!("{message}");
    }
    String::from_utf8(stdout).context("gh printed something other than UTF-8")
}

/// What `gh api` printed and its error: `gh` fails on a GraphQL error but prints the response.
fn gh_output(args: &[String], input: Option<String>) -> Result<(Vec<u8>, Option<String>)> {
    let mut command = gh_api();
    command.args(args);
    let mut child = command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => anyhow!("GitHub CLI (gh) is not installed"),
            _ => anyhow!(err).context("run gh"),
        })?;
    let writer = match (input, child.stdin.take()) {
        (Some(input), Some(mut stdin)) => {
            Some(std::thread::spawn(move || stdin.write_all(input.as_bytes())))
        }
        _ => None,
    };
    let output = child.wait_with_output().context("wait for gh")?;
    let written = writer.map(std::thread::JoinHandle::join);
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("gh failed")
            .trim();
        let message = message.strip_prefix("gh: ").unwrap_or(message).to_string();
        return Ok((output.stdout, Some(message)));
    }
    if let Some(written) = written {
        written.map_err(|_| anyhow!("writing to gh panicked"))?.context("write to gh")?;
    }
    Ok((output.stdout, None))
}

fn parse_data<T: DeserializeOwned>(json: &str) -> Result<T> {
    let response: Response<T> = serde_json::from_str(json).context("parse the gh response")?;
    if let Some(errors) = response.errors.filter(|e| !e.is_empty()) {
        let messages: Vec<String> = errors.into_iter().map(|e| e.message).collect();
        bail!("{}", messages.join("; "));
    }
    response
        .data
        .ok_or_else(|| anyhow!("the response has no data"))
}

/// Parse one page of [`board_query`]'s response.
pub fn parse_page(json: &str) -> Result<BoardPage> {
    let data: Data = parse_data(json)?;
    let project = data
        .user
        .or(data.organization)
        .and_then(|owner| owner.project_v2)
        .ok_or_else(|| anyhow!("board not found"))?;
    let (status_field_id, options) = project
        .field
        .and_then(|field| Some((field.id?, field.options?)))
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
            id: project.id,
            status_field_id,
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
    let (column_id, column_set_at) = node
        .field_value_by_name
        .map(|v| (v.option_id, v.updated_at))
        .unwrap_or_default();
    let (kind, linked) = match node.content? {
        Content::DraftIssue { title } => {
            return Some(BoardItem {
                id: node.id,
                kind: ItemKind::Draft,
                number: None,
                title,
                url: None,
                repository: None,
                content_id: None,
                assignees: Vec::new(),
                state: ItemState::Open,
                column_id,
                column_set_at,
                updated_at: node.updated_at,
            });
        }
        Content::Issue(c) => (ItemKind::Issue, c),
        Content::PullRequest(c) => (ItemKind::PullRequest, c),
        Content::Other => return None,
    };
    Some(BoardItem {
        id: node.id,
        kind,
        number: Some(linked.number),
        title: linked.title,
        url: Some(linked.url),
        repository: Some(linked.repository.name_with_owner),
        content_id: Some(linked.id),
        assignees: linked
            .assignees
            .map(|a| a.nodes.into_iter().flatten().map(|u| u.login).collect())
            .unwrap_or_default(),
        state: item_state(&linked.state),
        column_id,
        column_set_at,
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
struct Response<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddCommentData {
    add_comment: AddCommentPayload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddCommentPayload {
    comment_edge: CommentEdge,
}

#[derive(Deserialize)]
struct CommentEdge {
    node: NewComment,
}

#[derive(Deserialize)]
struct BeforePostingData {
    item: Option<ItemStatus>,
    subject: Option<Subject>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ItemStatus {
    field_value_by_name: Option<StatusValue>,
}

#[derive(Deserialize)]
struct Subject {
    comments: Option<Comments>,
}

#[derive(Deserialize)]
struct Comments {
    nodes: Vec<Option<IssueComment>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueComment {
    url: String,
    created_at: String,
    viewer_did_author: bool,
    body: String,
}

#[derive(Deserialize)]
struct ViewerData {
    viewer: ViewerNode,
}

#[derive(Deserialize)]
struct ViewerNode {
    login: String,
    id: String,
}

#[derive(Deserialize)]
struct NodesData {
    nodes: Vec<Option<TimelineOwner>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TimelineOwner {
    id: Option<String>,
    timeline_items: Option<TimelineItems>,
    closed_by_pull_requests_references: Option<LinkedList>,
    head_ref_name: Option<String>,
    is_cross_repository: Option<bool>,
    maintainer_can_modify: Option<bool>,
    head_repository: Option<Repository>,
}

#[derive(Deserialize)]
struct LinkedList {
    nodes: Vec<Option<LinkedNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkedNode {
    number: u32,
    state: String,
    created_at: String,
    closed_at: Option<String>,
    is_cross_repository: bool,
    head_ref_name: String,
}

#[derive(Deserialize)]
struct TimelineItems {
    nodes: Vec<Option<StatusChangeNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatusChangeNode {
    created_at: String,
    status: String,
    was_automated: bool,
    actor: Option<Login>,
    project: Option<ProjectId>,
}

#[derive(Deserialize)]
struct Login {
    login: String,
}

#[derive(Deserialize)]
struct CommentsData {
    nodes: Vec<Option<CommentsOwner>>,
}

#[derive(Deserialize)]
struct CommentsOwner {
    id: Option<String>,
    comments: Option<CommentList>,
}

#[derive(Deserialize)]
struct CommentList {
    nodes: Vec<Option<IssueCommentNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueCommentNode {
    url: String,
    body: String,
    created_at: String,
    author: Option<Login>,
}

#[derive(Deserialize)]
struct PullRequestData {
    node: Option<PullRequestNode>,
}

#[derive(Deserialize)]
struct PullRequestsData {
    repository: Option<PullRequestsOwner>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestsOwner {
    pull_requests: PullRequestList,
}

#[derive(Deserialize)]
struct PullRequestList {
    nodes: Vec<Option<PullRequestNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestNode {
    id: String,
    number: u32,
    url: String,
    state: String,
    is_draft: bool,
    is_cross_repository: bool,
    head_ref_oid: String,
    commits: CommitList,
    #[serde(default)]
    base_ref_name: Option<String>,
    #[serde(default)]
    mergeable: Option<String>,
    #[serde(default)]
    merge_state_status: Option<String>,
    #[serde(default)]
    reviews: Option<Count>,
    #[serde(default)]
    comments: Option<PullRequestComments>,
}

#[derive(Deserialize)]
struct PullRequestComments {
    nodes: Vec<Option<PullRequestCommentNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequestCommentNode {
    url: String,
    created_at: String,
    viewer_did_author: bool,
    author: Option<Actor>,
}

#[derive(Deserialize)]
struct Actor {
    #[serde(rename = "__typename")]
    typename: String,
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Count {
    total_count: u32,
}

#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum CheckContext {
    #[serde(rename_all = "camelCase")]
    CheckRun {
        name: String,
        status: String,
        conclusion: Option<String>,
        details_url: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    StatusContext {
        context: String,
        state: String,
        target_url: Option<String>,
    },
}

impl CheckContext {
    fn running(&self) -> bool {
        match self {
            CheckContext::CheckRun { status, .. } => status != "COMPLETED",
            CheckContext::StatusContext { state, .. } => {
                matches!(state.as_str(), "PENDING" | "EXPECTED")
            }
        }
    }

    fn failed(self) -> Option<FailedCheck> {
        let (name, url) = match self {
            CheckContext::CheckRun {
                name,
                conclusion: Some(conclusion),
                details_url,
                ..
            } if matches!(
                conclusion.as_str(),
                "FAILURE" | "TIMED_OUT" | "CANCELLED" | "STARTUP_FAILURE" | "ACTION_REQUIRED"
            ) =>
            {
                (name, details_url)
            }
            CheckContext::StatusContext {
                context,
                state,
                target_url,
            } if matches!(state.as_str(), "FAILURE" | "ERROR") => (context, target_url),
            _ => return None,
        };
        Some(FailedCheck {
            name,
            url: url.unwrap_or_default(),
        })
    }
}

#[derive(Deserialize)]
struct ThreadRepliesData {
    node: Option<ThreadsOwner>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadsOwner {
    review_threads: ThreadList,
}

#[derive(Deserialize)]
struct ThreadList {
    nodes: Vec<Option<ThreadNode>>,
}

#[derive(Deserialize)]
struct ThreadNode {
    comments: ThreadCommentList,
}

#[derive(Deserialize)]
struct ThreadCommentList {
    nodes: Vec<Option<ThreadComment>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadComment {
    viewer_did_author: bool,
    created_at: String,
}

#[derive(Deserialize)]
struct CommitList {
    nodes: Vec<Option<CommitNode>>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: Commit,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Commit {
    status_check_rollup: Option<Rollup>,
}

#[derive(Deserialize)]
struct Rollup {
    state: String,
    #[serde(default)]
    contexts: Option<ContextList>,
}

#[derive(Deserialize)]
struct ContextList {
    nodes: Vec<Option<CheckContext>>,
}

#[derive(Deserialize)]
struct ProjectId {
    id: String,
}

#[derive(Deserialize)]
struct GraphqlError {
    message: String,
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    path: Vec<serde_json::Value>,
}

impl GraphqlError {
    fn is_not_found(&self, field: &str) -> bool {
        self.kind.as_deref() == Some("NOT_FOUND") && self.path == [serde_json::json!(field)]
    }
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
struct WorkflowsData {
    user: Option<WorkflowsOwner>,
    organization: Option<WorkflowsOwner>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowsOwner {
    project_v2: Option<WorkflowsProject>,
}

#[derive(Deserialize)]
struct WorkflowsProject {
    workflows: WorkflowList,
}

#[derive(Deserialize)]
struct WorkflowList {
    nodes: Vec<Option<WorkflowNode>>,
}

#[derive(Deserialize)]
struct WorkflowNode {
    name: String,
    enabled: bool,
}

#[derive(Deserialize)]
struct Project {
    id: String,
    title: String,
    url: String,
    closed: bool,
    field: Option<StatusField>,
    items: Items,
}

#[derive(Deserialize)]
struct StatusField {
    id: Option<String>,
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
    updated_at: Option<String>,
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
    id: String,
    number: u32,
    title: String,
    url: String,
    state: String,
    repository: Repository,
    assignees: Option<Assignees>,
}

#[derive(Deserialize)]
struct Assignees {
    nodes: Vec<Option<Login>>,
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
      "id":"PVT_1","title":"CodeScope","url":"https://github.com/users/maui1911/projects/3",
      "closed":false,
      "field":{"id":"PVTSSF_1","options":[{"id":"f1","name":"Todo"},{"id":"f2","name":"Ready"}]},
      "items":{"pageInfo":{"hasNextPage":true,"endCursor":"Y3Vy"},"nodes":[
        {"id":"PVTI_1","isArchived":false,"updatedAt":"2026-11-20T00:55:34Z",
         "fieldValueByName":{"optionId":"f2","updatedAt":"2026-11-20T00:55:30Z"},
         "content":{"__typename":"Issue","id":"I_411","number":411,"title":"Fuzzy paths",
           "url":"https://github.com/maui1911/CodeScope/issues/411","state":"OPEN",
           "repository":{"nameWithOwner":"maui1911/CodeScope"},
           "assignees":{"nodes":[{"login":"maui1911"}]}}},
        {"id":"PVTI_2","isArchived":false,"updatedAt":"2026-11-21T00:00:00Z",
         "fieldValueByName":null,
         "content":{"__typename":"PullRequest","id":"PR_414","number":414,
           "title":"Group the bell","url":"https://github.com/maui1911/CodeScope/pull/414",
           "state":"MERGED","repository":{"nameWithOwner":"maui1911/CodeScope"},
           "assignees":{"nodes":[]}}},
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
    fn a_linked_branch_counts_only_when_github_returns_it() {
        let made = r#"{"data":{"createLinkedBranch":{"linkedBranch":{"id":"LB_1"}}}}"#;
        assert!(parse_linked_branch(made, "14-palette").is_ok());
        let refused = r#"{"data":{"createLinkedBranch":{"linkedBranch":null}}}"#;
        let err = parse_linked_branch(refused, "14-palette").unwrap_err();
        assert_eq!(err.to_string(), "GitHub didn't make 14-palette");
        let nothing = r#"{"data":{"createLinkedBranch":null}}"#;
        assert!(parse_linked_branch(nothing, "14-palette").is_err());
    }

    #[test]
    fn reads_which_of_the_board_s_workflows_are_on() {
        let json = r#"{"data":{"organization":{"projectV2":{"workflows":{"nodes":[
          {"name":"Item closed","enabled":true},
          {"name":"Pull request linked to issue","enabled":false},
          null,
          {"name":"Pull request merged","enabled":true}
        ]}}}}}"#;
        let on = parse_enabled_workflows(json).unwrap();
        assert_eq!(on, ["Item closed", "Pull request merged"]);
        assert!(!moves_back_from_review(&on));
        let linked = vec![LINKED_PULL_REQUEST_WORKFLOW.to_string()];
        assert!(moves_back_from_review(&linked));
        let refused = r#"{"data":{"organization":{"projectV2":null}},"errors":[
          {"type":"FORBIDDEN","message":"You do not have permission to access workflows"}]}"#;
        let err = parse_enabled_workflows(refused).unwrap_err();
        assert!(format!("{err:#}").contains("permission"), "{err:#}");
        assert!(workflows_query(BoardOwner::User).contains("user(login: $owner)"));
    }

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
        assert_eq!(board.id, "PVT_1");
        assert_eq!(board.status_field_id, "PVTSSF_1");
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
        assert_eq!(issue.column_set_at.as_deref(), Some("2026-11-20T00:55:30Z"));
        assert_eq!(issue.state, ItemState::Open);
        assert_eq!(issue.content_id.as_deref(), Some("I_411"));
        assert_eq!(issue.assignees, vec!["maui1911"]);

        let pr = &board.items[1];
        assert_eq!(pr.kind, ItemKind::PullRequest);
        assert_eq!(pr.state, ItemState::Merged);
        assert_eq!(pr.column_id, None);

        let draft = &board.items[2];
        assert_eq!(draft.kind, ItemKind::Draft);
        assert_eq!(draft.number, None);
        assert_eq!(draft.title, "Theme picker");
        assert_eq!(draft.column_id, None);
        assert_eq!(draft.column_set_at, None);
        assert_eq!(draft.content_id, None);
    }

    #[test]
    fn api_calls_go_to_github_com() {
        let command = gh_api();
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["api", "--hostname", "github.com"]);
    }

    #[test]
    fn a_null_item_is_skipped() {
        let json = PAGE.replacen(r#""nodes":["#, r#""nodes":[null,"#, 1);
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
            r#""field":{"id":"PVTSSF_1","options":[{"id":"f1","name":"Todo"},{"id":"f2","name":"Ready"}]}"#,
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
    fn keeps_the_last_status_change_on_this_board() {
        let json = r#"{"data":{"nodes":[
          {"__typename":"Issue","id":"I_411","timelineItems":{"nodes":[
            {"createdAt":"2026-11-25T09:00:00Z","status":"Research","wasAutomated":false,
             "actor":{"login":"maui1911"},"project":{"id":"PVT_1"}},
            {"createdAt":"2026-11-25T10:00:00Z","status":"Ready","wasAutomated":false,
             "actor":{"login":"lead"},"project":{"id":"PVT_1"}},
            {"createdAt":"2026-11-25T11:00:00Z","status":"Doing","wasAutomated":true,
             "actor":null,"project":{"id":"PVT_OTHER"}}]}},
          {"__typename":"Issue","id":"I_412","timelineItems":{"nodes":[
            {"createdAt":"2026-11-25T11:00:00Z","status":"Todo","wasAutomated":false,
             "actor":null,"project":{"id":"PVT_OTHER"}}]}},
          null
        ]}}"#;
        let changes = parse_trigger_reads(json, "PVT_1").unwrap().changes;
        assert_eq!(changes.len(), 1);
        assert_eq!(
            changes["I_411"],
            StatusChange {
                actor: Some("lead".into()),
                was_automated: false,
                status: "Ready".into(),
                created_at: "2026-11-25T10:00:00Z".into(),
            }
        );
    }

    #[test]
    fn reads_each_issues_linked_pull_requests() {
        let json = r#"{"data":{"nodes":[
          {"__typename":"Issue","id":"I_432","timelineItems":{"nodes":[]},
           "closedByPullRequestsReferences":{"nodes":[
            {"number":12,"state":"OPEN","createdAt":"2026-11-24T10:00:00Z","closedAt":null,
             "isCrossRepository":true,"headRefName":"fix-paths"},
            {"number":9,"state":"MERGED","createdAt":"2026-11-20T10:00:00Z",
             "closedAt":"2026-11-21T10:00:00Z","isCrossRepository":false,"headRefName":"9-old"}]}},
          {"__typename":"PullRequest","id":"PR_12","timelineItems":{"nodes":[]}}
        ]}}"#;
        let linked = parse_trigger_reads(json, "PVT_1").unwrap().linked;
        assert_eq!(linked.len(), 1, "a pull request item has no linked pull requests");
        assert!(linked.contains_key("I_432"));
        assert_eq!(
            linked["I_432"],
            vec![
                LinkedPullRequest {
                    number: 12,
                    open: true,
                    head_ref_name: "fix-paths".into(),
                    is_cross_repository: true,
                    created_at: "2026-11-24T10:00:00Z".into(),
                    closed_at: None,
                },
                LinkedPullRequest {
                    number: 9,
                    open: false,
                    head_ref_name: "9-old".into(),
                    is_cross_repository: false,
                    created_at: "2026-11-20T10:00:00Z".into(),
                    closed_at: Some("2026-11-21T10:00:00Z".into()),
                },
            ]
        );
    }

    #[test]
    fn reads_the_branch_each_pull_request_merges_from() {
        let json = r#"{"data":{"nodes":[
          {"__typename":"PullRequest","id":"PR_420","headRefName":"keep-palette-open",
           "isCrossRepository":false,"maintainerCanModify":false,
           "headRepository":{"nameWithOwner":"maui1911/CodeScope"},"timelineItems":{"nodes":[]}},
          {"__typename":"PullRequest","id":"PR_421","headRefName":"main",
           "isCrossRepository":true,"maintainerCanModify":true,
           "headRepository":{"nameWithOwner":"octo/CodeScope"},"timelineItems":{"nodes":[]}},
          {"__typename":"PullRequest","id":"PR_422","headRefName":"fix",
           "isCrossRepository":true,"maintainerCanModify":false,"headRepository":null,
           "timelineItems":{"nodes":[]}},
          {"__typename":"Issue","id":"I_432","timelineItems":{"nodes":[]},
           "closedByPullRequestsReferences":{"nodes":[]}}
        ]}}"#;
        let heads = parse_trigger_reads(json, "PVT_1").unwrap().heads;
        assert_eq!(heads.len(), 3, "an issue has no branch");
        assert_eq!(
            heads["PR_420"],
            PullRequestHead {
                branch: "keep-palette-open".into(),
                is_cross_repository: false,
                repository: Some("maui1911/CodeScope".into()),
                maintainer_can_modify: false,
            }
        );
        assert_eq!(
            heads["PR_421"],
            PullRequestHead {
                branch: "main".into(),
                is_cross_repository: true,
                repository: Some("octo/CodeScope".into()),
                maintainer_can_modify: true,
            }
        );
        assert_eq!(heads["PR_422"].repository, None, "a deleted fork");
    }

    #[test]
    fn reads_the_url_and_github_s_time_of_a_new_comment() {
        let json = r#"{"data":{"addComment":{"commentEdge":{"node":{
          "url":"https://github.com/maui1911/CodeScope/issues/408#issuecomment-1",
          "createdAt":"2026-11-27T13:18:24Z"}}}}}"#;
        assert_eq!(
            parse_new_comment(json).unwrap(),
            NewComment {
                url: "https://github.com/maui1911/CodeScope/issues/408#issuecomment-1".into(),
                created_at: "2026-11-27T13:18:24Z".into(),
            }
        );
    }

    #[test]
    fn reads_the_column_and_the_same_findings_the_user_posted_since_the_round_began() {
        let marker = "<!-- dispatch v=1 phase=research outcome=findings by=codescope -->";
        let body = format!("D.\n\n**Next step:** move it.\n{marker}\n");
        let comment = |url: &str, at: &str, mine: bool, body: &str| {
            serde_json::json!({
                "url": url, "createdAt": at, "viewerDidAuthor": mine, "body": body,
            })
        };
        let comments = serde_json::json!({ "nodes": [
            comment("u-earlier", "2026-11-25T09:00:00Z", true, &body),
            comment("u-theirs", "2026-11-25T10:05:00Z", false, &body),
            comment("u-round", "2026-11-25T10:06:00Z", true, &format!("C.\n{marker}\n")),
            comment("u-mine", "2026-11-25T10:07:00Z", true, &body.replace('\n', "\r\n")),
            comment("u-chat", "2026-11-25T10:08:00Z", true, "Looks good."),
        ] });
        let json = serde_json::json!({ "data": {
            "item": { "fieldValueByName": { "optionId": "opt-researching" } },
            "subject": { "comments": comments },
        } })
        .to_string();
        let since = "2026-11-25T10:00:00Z";
        let before = parse_before_posting(&json, since, &body).unwrap();
        assert_eq!(before.column_id.as_deref(), Some("opt-researching"));
        assert_eq!(before.posted.as_deref(), Some("u-mine"));

        let later = parse_before_posting(&json, "2026-11-25T10:07:00Z", &body).unwrap();
        assert_eq!(later.posted, None, "a comment from before the round is not this round's");
        let other = parse_before_posting(&json, since, "E.\n").unwrap();
        assert_eq!(other.posted, None, "other findings are not these");
        let bare = parse_before_posting(r#"{"data":{"item":null,"subject":{}}}"#, since, &body);
        let bare = bare.unwrap();
        assert_eq!((bare.column_id, bare.posted), (None, None));
    }

    #[test]
    fn an_item_taken_off_the_board_is_in_no_column_but_other_errors_fail() {
        let not_found = |path: &str| {
            serde_json::json!({
                "type": "NOT_FOUND", "path": [path], "message": "Could not resolve to a node",
            })
        };
        let response = |error: serde_json::Value| {
            serde_json::json!({
                "data": { "item": null, "subject": { "comments": { "nodes": [] } } },
                "errors": [error],
            })
            .to_string()
        };
        let since = "2026-11-25T10:00:00Z";
        let removed = parse_before_posting(&response(not_found("item")), since, "D.").unwrap();
        assert_eq!(removed, BeforePosting::default());
        assert!(parse_before_posting(&response(not_found("subject")), since, "D.").is_err());
        let forbidden = serde_json::json!({
            "type": "FORBIDDEN", "path": ["item"], "message": "Resource not accessible",
        });
        let err = parse_before_posting(&response(forbidden), since, "D.").unwrap_err();
        assert_eq!(err.to_string(), "Resource not accessible");
    }

    #[test]
    fn reads_comments_by_content_id_and_keeps_deleted_authors() {
        let json = r#"{"data":{"nodes":[
          {"__typename":"Issue","id":"I_1","comments":{"nodes":[
            {"url":"https://c/1","body":"B1 A","createdAt":"2026-11-25T18:00:00Z",
             "author":{"login":"sanne"}},
            {"url":"https://c/2","body":"Hm.","createdAt":"2026-11-25T18:01:00Z","author":null}
          ]}},
          {"__typename":"Issue","id":"I_2","comments":{"nodes":[]}},
          null
        ]}}"#;
        let comments = parse_comments(json).unwrap();
        assert_eq!(comments.len(), 2);
        assert!(comments["I_2"].is_empty());
        assert_eq!(
            comments["I_1"],
            vec![
                Comment {
                    url: "https://c/1".into(),
                    author: Some("sanne".into()),
                    body: "B1 A".into(),
                    created_at: "2026-11-25T18:00:00Z".into(),
                },
                Comment {
                    url: "https://c/2".into(),
                    author: None,
                    body: "Hm.".into(),
                    created_at: "2026-11-25T18:01:00Z".into(),
                },
            ]
        );
    }

    #[test]
    fn reads_the_pull_requests_on_a_branch_but_not_those_from_forks() {
        let json = r#"{"data":{"repository":{"pullRequests":{"nodes":[
          {"id":"PR_3","number":417,"url":"https://github.com/maui1911/CodeScope/pull/417",
           "state":"OPEN","isDraft":true,"isCrossRepository":false,"headRefOid":"c417",
           "commits":{"nodes":[{"commit":{"statusCheckRollup":{"state":"PENDING"}}}]}},
          {"id":"PR_2","number":416,"url":"https://github.com/maui1911/CodeScope/pull/416",
           "state":"OPEN","isDraft":false,"isCrossRepository":true,"headRefOid":"c416",
           "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}},
          {"id":"PR_1","number":415,"url":"https://github.com/maui1911/CodeScope/pull/415",
           "state":"MERGED","isDraft":false,"isCrossRepository":false,"headRefOid":"c415",
           "commits":{"nodes":[]}}
        ]}}}}"#;
        let found = parse_pull_requests(json).unwrap();
        assert_eq!(
            found,
            vec![
                PullRequest {
                    id: "PR_3".into(),
                    number: 417,
                    url: "https://github.com/maui1911/CodeScope/pull/417".into(),
                    state: PullRequestState::Open,
                    is_draft: true,
                    checks: Checks::Running,
                    head_oid: "c417".into(),
                    health: Health::default(),
                    comments: Vec::new(),
                },
                PullRequest {
                    id: "PR_1".into(),
                    number: 415,
                    url: "https://github.com/maui1911/CodeScope/pull/415".into(),
                    state: PullRequestState::Merged,
                    is_draft: false,
                    checks: Checks::None,
                    head_oid: "c415".into(),
                    health: Health::default(),
                    comments: Vec::new(),
                },
            ]
        );
        let missing = parse_pull_requests(r#"{"data":{"repository":null}}"#).unwrap_err();
        assert_eq!(missing.to_string(), "repository not found");
    }

    #[test]
    fn reads_one_pull_request_by_its_id() {
        let node = |state: &str| {
            format!(
                r#"{{"data":{{"node":{{"id":"PR_420","number":420,
                  "url":"https://github.com/maui1911/CodeScope/pull/420","state":"{state}",
                  "isDraft":true,"isCrossRepository":false,"headRefOid":"c420",
                  "commits":{{"nodes":[{{"commit":{{"statusCheckRollup":{{"state":"SUCCESS"}}}}}}]}}
                }}}}}}"#
            )
        };
        let open = parse_pull_request(&node("OPEN")).unwrap();
        assert_eq!(
            open,
            Some(PullRequest {
                id: "PR_420".into(),
                number: 420,
                url: "https://github.com/maui1911/CodeScope/pull/420".into(),
                state: PullRequestState::Open,
                is_draft: true,
                checks: Checks::Passing,
                head_oid: "c420".into(),
                health: Health::default(),
                comments: Vec::new(),
            })
        );
        let merged = parse_pull_request(&node("MERGED")).unwrap().unwrap();
        assert_eq!(merged.state, PullRequestState::Merged);
        assert_eq!(parse_pull_request(&node("CLOSED")).unwrap(), None);
        assert!(parse_pull_request(r#"{"data":{"node":null}}"#).is_err());
    }

    #[test]
    fn reads_what_stands_between_a_pull_request_and_its_merge() {
        let node = |mergeable: &str, merge_state: &str, reviews: u32| {
            format!(
                r#"{{"data":{{"node":{{"id":"PR_420","number":420,
                  "url":"https://github.com/maui1911/CodeScope/pull/420","state":"OPEN",
                  "isDraft":false,"isCrossRepository":false,"headRefOid":"c420",
                  "baseRefName":"main","mergeable":"{mergeable}",
                  "mergeStateStatus":"{merge_state}","reviews":{{"totalCount":{reviews}}},
                  "commits":{{"nodes":[{{"commit":{{"statusCheckRollup":{{"state":"FAILURE",
                    "contexts":{{"nodes":[
                      {{"__typename":"CheckRun","name":"build","status":"COMPLETED",
                        "conclusion":"FAILURE","detailsUrl":"https://ci/build"}},
                      {{"__typename":"CheckRun","name":"lint","status":"IN_PROGRESS",
                        "conclusion":null,"detailsUrl":null}},
                      {{"__typename":"CheckRun","name":"docs","status":"COMPLETED",
                        "conclusion":"SKIPPED","detailsUrl":"https://ci/docs"}},
                      {{"__typename":"CheckRun","name":"slow","status":"COMPLETED",
                        "conclusion":"TIMED_OUT","detailsUrl":"https://ci/slow"}},
                      {{"__typename":"StatusContext","context":"ci/legacy","state":"ERROR",
                        "targetUrl":"https://ci/legacy"}},
                      {{"__typename":"StatusContext","context":"deploy","state":"SUCCESS",
                        "targetUrl":null}}
                    ]}}}}}}}}]}}
                }}}}}}"#
            )
        };
        let health = |json: &str| parse_pull_request(json).unwrap().unwrap().health;
        let check = |name: &str, url: &str| FailedCheck {
            name: name.into(),
            url: url.into(),
        };
        assert_eq!(
            health(&node("CONFLICTING", "BEHIND", 1)),
            Health {
                base: "main".into(),
                mergeable: Mergeable::Conflicts,
                behind: true,
                reviewed: true,
                failed_checks: vec![
                    check("build", "https://ci/build"),
                    check("slow", "https://ci/slow"),
                    check("ci/legacy", "https://ci/legacy"),
                ],
                checks_running: true,
            }
        );
        let clean = health(&node("MERGEABLE", "UNSTABLE", 0));
        assert_eq!((clean.mergeable, clean.behind, clean.reviewed), (Mergeable::Yes, false, false));
        assert_eq!(health(&node("UNKNOWN", "UNKNOWN", 0)).mergeable, Mergeable::Unknown);
    }

    #[test]
    fn reads_who_wrote_each_conversation_comment_on_a_pull_request() {
        let json = r#"{"data":{"node":{"id":"PR_420","number":420,
          "url":"https://github.com/maui1911/CodeScope/pull/420","state":"OPEN",
          "isDraft":false,"isCrossRepository":false,"headRefOid":"c420","commits":{"nodes":[]},
          "comments":{"nodes":[
            {"url":"https://c/1","createdAt":"2026-11-27T10:00:00Z","viewerDidAuthor":false,
             "author":{"__typename":"User","login":"sanne"}},
            {"url":"https://c/2","createdAt":"2026-11-27T10:01:00Z","viewerDidAuthor":true,
             "author":{"__typename":"User","login":"maui1911"}},
            {"url":"https://c/3","createdAt":"2026-11-27T10:02:00Z","viewerDidAuthor":false,
             "author":{"__typename":"Bot","login":"github-actions"}},
            {"url":"https://c/4","createdAt":"2026-11-27T10:03:00Z","viewerDidAuthor":false,
             "author":null},
            null
          ]}
        }}}"#;
        let comment = |n: u32, author: Option<&str>, by_viewer: bool| PullRequestComment {
            url: format!("https://c/{n}"),
            author: author.map(String::from),
            created_at: format!("2026-11-27T10:0{}:00Z", n - 1),
            by_viewer,
        };
        assert_eq!(
            parse_pull_request(json).unwrap().unwrap().comments,
            vec![
                comment(1, Some("sanne"), false),
                comment(2, Some("maui1911"), true),
                comment(3, None, false),
                comment(4, None, false),
            ]
        );
        for query in [PULL_REQUEST_QUERY, PULL_REQUESTS_QUERY] {
            assert!(query.contains("comments(last: 50) { ...conversation }"), "{query}");
        }
    }

    #[test]
    fn the_last_review_thread_reply_is_the_viewers_newest_comment_in_any_thread() {
        let json = r#"{"data":{"node":{"reviewThreads":{"nodes":[
          {"comments":{"nodes":[
            {"viewerDidAuthor":false,"createdAt":"2026-11-26T10:00:00Z"},
            {"viewerDidAuthor":true,"createdAt":"2026-11-26T11:00:00Z"}]}},
          {"comments":{"nodes":[
            {"viewerDidAuthor":true,"createdAt":"2026-11-26T12:00:00Z"},
            {"viewerDidAuthor":false,"createdAt":"2026-11-26T13:00:00Z"},
            null]}},
          null
        ]}}}}"#;
        assert_eq!(
            parse_last_thread_reply(json).unwrap(),
            Some("2026-11-26T12:00:00Z".to_string())
        );
        let none = r#"{"data":{"node":{"reviewThreads":{"nodes":[
          {"comments":{"nodes":[{"viewerDidAuthor":false,"createdAt":"2026-11-26T10:00:00Z"}]}}
        ]}}}}"#;
        assert_eq!(parse_last_thread_reply(none).unwrap(), None);
        let gone = parse_last_thread_reply(r#"{"data":{"node":null}}"#).unwrap_err();
        assert_eq!(gone.to_string(), "pull request not found");
    }

    #[test]
    fn a_missing_board_is_an_error() {
        let err = parse_page(r#"{"data":{"organization":{"projectV2":null}}}"#).unwrap_err();
        assert_eq!(err.to_string(), "board not found");
    }

    #[test]
    fn write_access_is_only_what_github_confirms() {
        for (permission, can_write) in
            [("admin\n", true), ("maintain", true), ("write", true), ("read", false)]
        {
            assert_eq!(write_access(Ok(permission.into())).unwrap(), can_write, "{permission}");
        }
        let not_a_collaborator = anyhow::anyhow!("gh: Not Found (HTTP 404)");
        assert!(!write_access(Err(not_a_collaborator)).unwrap());
        let viewer_cant_push = anyhow::anyhow!(
            "gh: Must have push access to view collaborator permission. (HTTP 403)"
        );
        assert!(!write_access(Err(viewer_cant_push)).unwrap());
        for refused in [
            "Resource protected by organization SAML enforcement. (HTTP 403)",
            "API rate limit exceeded for user ID 1. (HTTP 403)",
            "connection reset",
        ] {
            assert!(write_access(Err(anyhow::anyhow!(refused))).is_err(), "{refused}");
        }
    }
}
