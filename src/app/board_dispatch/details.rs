//! What the item drawer reads when it opens (ADR-0027): the item's moves on the board, the
//! research findings on it with their answers, and its pull request's Test by hand.

use std::collections::HashMap;
use std::time::Instant;

use codescope_core::board::ItemKind;
use codescope_core::board::answers::{Answer, Comment, chosen};
use codescope_core::board::findings::{
    DecisionPoint, Paragraph, PostedSummary, plain_paragraphs, plain_text, posted_findings,
    posted_summary,
};
use codescope_core::board::github::{self, ColumnMove};
use codescope_core::board::pull_request::test_by_hand;
use codescope_core::time::{secs_since, span};
use gpui::{App, AppContext as _, Context};

use super::{AppShell, Discussion, discussion, fresh_access};

type Checked = Vec<((String, String), Option<bool>)>;

/// The read of the open drawer's item; opening a drawer starts a new one.
pub(crate) struct DrawerRead {
    item_id: String,
    started: Instant,
    /// `None` while the read runs.
    result: Option<Result<Found, String>>,
}

struct Found {
    moves: Vec<ColumnMove>,
    test_by_hand: Option<String>,
    findings: Option<FoundFindings>,
}

struct FoundFindings {
    url: String,
    summary: Option<PostedSummary>,
    points: Vec<DecisionPoint>,
    answers: Vec<Answer>,
}

struct DetailsInput {
    board_id: String,
    content_id: String,
    repository: String,
    pull_request: Option<u32>,
    findings_url: Option<String>,
    known: HashMap<(String, String), bool>,
}

impl AppShell {
    /// Read the moves, findings and Test by hand of `item_id` on the board at `board_url` for
    /// its drawer; a draft issue has none of them.
    pub(crate) fn read_item_details(
        &mut self,
        board_url: &str,
        item_id: &str,
        cx: &mut Context<Self>,
    ) {
        self.board.details = None;
        let Some(input) = self.details_input(board_url, item_id, cx) else {
            return;
        };
        let started = Instant::now();
        self.board.details = Some(DrawerRead {
            item_id: item_id.to_string(),
            started,
            result: None,
        });
        cx.spawn(async move |this, cx| {
            let (result, checked) = cx.background_spawn(async move { read(input) }).await;
            let _ = this.update(cx, |this, cx| {
                this.board.dispatch.keep_access(checked);
                if let Some(read) = this.board.details.as_mut().filter(|r| r.started == started) {
                    read.result = Some(result);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn details_input(&self, board_url: &str, item_id: &str, cx: &App) -> Option<DetailsInput> {
        let (board, _) = self.read_board(board_url)?;
        let item = board.items.iter().find(|item| item.id == item_id)?;
        let record = self
            .dispatch_records(cx)
            .into_iter()
            .map(|(_, _, _, record)| record)
            .find(|r| r.board_url == board_url && r.item_id == item_id);
        let watch = self.board.dispatch.watches.get(item_id);
        let pull_request = match item.kind {
            ItemKind::PullRequest => item.number,
            ItemKind::Issue => watch
                .and_then(|w| w.pull_request.as_ref())
                .map(|pr| pr.number)
                .or_else(|| record.as_ref()?.pull_request.as_ref().map(|posted| posted.number))
                .or_else(|| watch?.reported.as_ref().map(|posted| posted.number)),
            ItemKind::Draft => return None,
        };
        Some(DetailsInput {
            board_id: board.id.clone(),
            content_id: item.content_id.clone()?,
            repository: item.repository.clone()?,
            pull_request,
            findings_url: record.and_then(|r| r.findings).map(|posted| posted.comment_url),
            known: fresh_access(&self.board.dispatch.access),
        })
    }
}

fn read(input: DetailsInput) -> (Result<Found, String>, Checked) {
    let mut checked = Vec::new();
    let pull_request = input.pull_request.map(|number| (input.repository.as_str(), number));
    let details =
        match github::fetch_item_details(&input.board_id, &input.content_id, pull_request) {
            Ok(details) => details,
            Err(err) => return (Err(format!("{err:#}")), checked),
        };
    let findings = found_findings(
        &details.comments,
        &input.repository,
        input.findings_url.as_deref(),
        &input.known,
        &mut checked,
    );
    let found = Found {
        moves: details.moves,
        test_by_hand: details.pull_request_body.as_deref().and_then(test_by_hand),
        findings,
    };
    (Ok(found), checked)
}

fn found_findings(
    comments: &[Comment],
    repository: &str,
    dispatched_url: Option<&str>,
    known: &HashMap<(String, String), bool>,
    checked: &mut Checked,
) -> Option<FoundFindings> {
    let Some((comment, summary)) = comments
        .iter()
        .rev()
        .find_map(|c| posted_summary(&c.body).map(|summary| (c, summary)))
    else {
        return dispatched_url.map(|url| FoundFindings {
            url: url.to_string(),
            summary: None,
            points: Vec::new(),
            answers: Vec::new(),
        });
    };
    let has_points = posted_findings(&comment.body)
        .is_some_and(|findings| !findings.decision_points.is_empty());
    let Discussion { points, answers } = if has_points {
        discussion(comments, repository, &comment.url, "", known, checked)
    } else {
        Discussion::default()
    };
    Some(FoundFindings {
        url: comment.url.clone(),
        summary: Some(summary),
        points,
        answers,
    })
}

/// What the drawer shows of its read.
pub(crate) enum DetailsView {
    /// A draft issue, or an item no read was started for.
    None,
    Reading,
    Failed(String),
    Read(ReadView),
}

pub(crate) struct ReadView {
    /// The Test by hand section of its pull request's body, or of the dispatch's.
    pub test_by_hand: Option<Vec<Paragraph>>,
    pub findings: Option<FindingsView>,
    /// Oldest first.
    pub moves: Vec<MoveRow>,
}

pub(crate) struct FindingsView {
    pub url: String,
    /// The cause and the approach; `None` when the comment is older than the 50 read.
    pub summary: Option<(Vec<Paragraph>, Vec<Paragraph>)>,
    pub points: Vec<PointView>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PointView {
    /// `B1 Which branch?`
    pub question: String,
    /// `A · Stack it.`: the option the latest answer chose, else the advised one.
    pub choice: String,
    /// Who chose it and the URL of their comment; `None` while only the advice says so.
    pub answered: Option<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MoveRow {
    /// The column it moved into.
    pub column: String,
    /// `you · 6 min ago`.
    pub by: String,
}

/// The drawer's view of `read`, while that is the read of `item_id`.
pub(crate) fn details_view(
    read: Option<&DrawerRead>,
    item_id: &str,
    viewer: Option<&str>,
    now: f64,
) -> DetailsView {
    let Some(read) = read.filter(|read| read.item_id == item_id) else {
        return DetailsView::None;
    };
    let found = match &read.result {
        None => return DetailsView::Reading,
        Some(Err(message)) => return DetailsView::Failed(message.clone()),
        Some(Ok(found)) => found,
    };
    let findings = found.findings.as_ref().map(|findings| FindingsView {
        url: findings.url.clone(),
        summary: findings.summary.as_ref().map(|summary| {
            (plain_paragraphs(&summary.cause), plain_paragraphs(&summary.approach))
        }),
        points: point_views(&findings.points, &findings.answers, viewer),
    });
    DetailsView::Read(ReadView {
        test_by_hand: found
            .test_by_hand
            .as_deref()
            .map(plain_paragraphs)
            .filter(|paragraphs| !paragraphs.is_empty()),
        findings,
        moves: move_rows(&found.moves, viewer, now),
    })
}

fn point_views(
    points: &[DecisionPoint],
    answers: &[Answer],
    viewer: Option<&str>,
) -> Vec<PointView> {
    let chosen = chosen(answers);
    points
        .iter()
        .map(|point| {
            let picked = chosen.iter().find(|(id, _)| *id == point.id).map(|(_, letter)| *letter);
            let letter = picked.unwrap_or(point.advice);
            let label = point
                .options
                .iter()
                .find(|option| option.letter == letter)
                .map(|option| option.label_and_detail().0);
            let answered = picked.and_then(|letter| {
                let answer = answers
                    .iter()
                    .rev()
                    .find(|a| a.choices.iter().any(|(id, l)| *id == point.id && *l == letter))?;
                Some((who(Some(&answer.author), viewer), answer.url.clone()))
            });
            PointView {
                question: format!("{} {}", point.id, plain_text(&point.question))
                    .trim_end()
                    .to_string(),
                choice: match label {
                    Some(label) => format!("{letter} · {label}"),
                    None => letter.to_string(),
                },
                answered,
            }
        })
        .collect()
}

fn move_rows(moves: &[ColumnMove], viewer: Option<&str>, now: f64) -> Vec<MoveRow> {
    moves
        .iter()
        .map(|change| {
            let by = if change.was_automated {
                "a workflow".to_string()
            } else {
                who(change.actor.as_deref(), viewer)
            };
            MoveRow {
                column: change.to.clone().unwrap_or_else(|| "No status".to_string()),
                by: match secs_since(&change.at, now) {
                    Some(secs) => format!("{by} · {} ago", span(secs)),
                    None => by,
                },
            }
        })
        .collect()
}

fn who(login: Option<&str>, viewer: Option<&str>) -> String {
    match login {
        Some(login) if viewer.is_some_and(|v| v.eq_ignore_ascii_case(login)) => "you".to_string(),
        Some(login) => format!("@{login}"),
        None => "ghost".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codescope_core::board::findings::{DecisionOption, check_findings, comment_body};

    fn now() -> f64 {
        codescope_core::time::parse_iso8601_secs("2026-11-27T22:00:00Z").unwrap()
    }

    fn point(id: &str, advice: char) -> DecisionPoint {
        DecisionPoint {
            id: id.into(),
            question: "Which `branch`?".into(),
            context: String::new(),
            options: vec![
                DecisionOption {
                    letter: 'A',
                    text: "Stack it. On #479.".into(),
                },
                DecisionOption {
                    letter: 'B',
                    text: "Wait.".into(),
                },
            ],
            advice,
        }
    }

    fn answer(author: &str, url: &str, choices: &[(&str, char)]) -> Answer {
        Answer {
            author: author.into(),
            created_at: "2026-11-27T21:00:00Z".into(),
            url: url.into(),
            choices: choices.iter().map(|(id, letter)| (id.to_string(), *letter)).collect(),
            body: String::new(),
        }
    }

    fn comment(url: &str, body: &str) -> Comment {
        Comment {
            url: url.into(),
            author: Some("sanne".into()),
            body: body.into(),
            created_at: "2026-11-27T21:00:00Z".into(),
        }
    }

    fn findings_comment(url: &str, cause: &str) -> Comment {
        let text = format!(
            "# Findings for #485\n\n## Cause\n\n{cause}\n\n## Evidence\n\nThe log.\n\n\
             ## Approach\n\nPost it.\n"
        );
        let findings = check_findings(&text).unwrap();
        comment(url, &comment_body(&text, &findings, ItemKind::Issue))
    }

    #[test]
    fn a_point_shows_the_latest_choice_and_who_made_it_else_the_advice() {
        let points = [point("B1", 'A'), point("B2", 'A')];
        let answers = [
            answer("sanne", "https://c/1", &[("B1", 'A')]),
            answer("Maui1911", "https://c/2", &[("B1", 'B')]),
        ];
        assert_eq!(
            point_views(&points, &answers, Some("maui1911")),
            vec![
                PointView {
                    question: "B1 Which branch?".into(),
                    choice: "B · Wait.".into(),
                    answered: Some(("you".into(), "https://c/2".into())),
                },
                PointView {
                    question: "B2 Which branch?".into(),
                    choice: "A · Stack it.".into(),
                    answered: None,
                },
            ]
        );
    }

    #[test]
    fn a_move_says_where_it_went_who_made_it_and_how_long_ago() {
        let change = |to: Option<&str>, actor: Option<&str>, was_automated: bool| ColumnMove {
            from: None,
            to: to.map(str::to_string),
            actor: actor.map(str::to_string),
            was_automated,
            at: "2026-11-27T21:54:00Z".into(),
        };
        let moves = [
            change(Some("Ready"), Some("maui1911"), false),
            change(Some("In review"), Some("sanne"), false),
            change(Some("Done"), Some("github-project-automation"), true),
            change(None, None, false),
        ];
        let row = |column: &str, by: &str| MoveRow {
            column: column.into(),
            by: by.into(),
        };
        assert_eq!(
            move_rows(&moves, Some("maui1911"), now()),
            vec![
                row("Ready", "you · 6 min ago"),
                row("In review", "@sanne · 6 min ago"),
                row("Done", "a workflow · 6 min ago"),
                row("No status", "ghost · 6 min ago"),
            ]
        );
    }

    #[test]
    fn the_view_follows_the_read_of_its_own_item_only() {
        let reading = DrawerRead {
            item_id: "PVTI_1".into(),
            started: Instant::now(),
            result: None,
        };
        let view = |read: Option<&DrawerRead>, item_id| details_view(read, item_id, None, now());
        assert!(matches!(view(Some(&reading), "PVTI_1"), DetailsView::Reading));
        assert!(matches!(view(Some(&reading), "PVTI_2"), DetailsView::None));
        assert!(matches!(view(None, "PVTI_1"), DetailsView::None));
        let failed = DrawerRead {
            result: Some(Err("HTTP 502".into())),
            ..reading
        };
        assert!(matches!(
            view(Some(&failed), "PVTI_1"),
            DetailsView::Failed(message) if message == "HTTP 502"
        ));
    }

    #[test]
    fn a_read_shows_test_by_hand_and_the_findings_as_plain_paragraphs() {
        let found = Found {
            moves: Vec::new(),
            test_by_hand: Some("- Open the board.\n- Click **the** card.".into()),
            findings: Some(FoundFindings {
                url: "https://c/f".into(),
                summary: Some(PostedSummary {
                    cause: "Nothing reads\nthe file.".into(),
                    approach: "Post it.".into(),
                }),
                points: Vec::new(),
                answers: Vec::new(),
            }),
        };
        let read = DrawerRead {
            item_id: "PVTI_1".into(),
            started: Instant::now(),
            result: Some(Ok(found)),
        };
        let DetailsView::Read(view) = details_view(Some(&read), "PVTI_1", None, now()) else {
            panic!("the read is done");
        };
        let texts = |paragraphs: &[Paragraph]| -> Vec<String> {
            paragraphs.iter().map(|p| p.text.clone()).collect()
        };
        assert_eq!(
            texts(&view.test_by_hand.unwrap()),
            ["• Open the board.", "• Click the card."]
        );
        let (cause, approach) = view.findings.unwrap().summary.unwrap();
        assert_eq!(texts(&cause), ["Nothing reads the file."]);
        assert_eq!(texts(&approach), ["Post it."]);
    }

    #[test]
    fn the_newest_findings_comment_is_the_one_shown() {
        let comments = [
            findings_comment("https://c/1", "The first round."),
            comment("https://c/2", "Why?"),
            findings_comment("https://c/3", "The second round."),
            comment("https://c/4", "Thanks."),
        ];
        let mut checked = Vec::new();
        let known = HashMap::new();
        let found =
            found_findings(&comments, "o/r", Some("https://c/1"), &known, &mut checked).unwrap();
        assert_eq!(found.url, "https://c/3");
        assert_eq!(found.summary.unwrap().cause, "The second round.");
        assert!(found.points.is_empty() && checked.is_empty());
    }

    #[test]
    fn without_a_findings_comment_among_those_read_only_the_dispatchs_link_is_left() {
        let comments = [comment("https://c/9", "B1 A")];
        let known = HashMap::new();
        let mut checked = Vec::new();
        assert!(found_findings(&comments, "o/r", None, &known, &mut checked).is_none());
        let dispatched =
            found_findings(&comments, "o/r", Some("https://c/f"), &known, &mut checked).unwrap();
        assert_eq!(dispatched.url, "https://c/f");
        assert_eq!(dispatched.summary, None);
        assert!(dispatched.points.is_empty() && dispatched.answers.is_empty());
        assert!(checked.is_empty());
    }
}
