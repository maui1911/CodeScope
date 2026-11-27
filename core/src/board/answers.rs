//! Answers to decision points, as the board protocol shapes them: the comment CodeScope posts
//! for the user, and which comments on the issue or pull request count as answers.

use super::ItemKind;
use super::findings::{DecisionPoint, is_marker_line};

/// A comment on an issue or pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    pub url: String,
    /// `None` for a deleted account.
    pub author: Option<String>,
    pub body: String,
    /// ISO 8601 in UTC, as GitHub writes it.
    pub created_at: String,
}

/// A comment that answers posted findings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub author: String,
    pub created_at: String,
    pub url: String,
    /// The `B1 A` pairs the comment starts with, for points and options the findings have.
    pub choices: Vec<(String, char)>,
    pub body: String,
}

/// `B1 A, B2 B`.
pub fn choices_text(choices: &[(String, char)]) -> String {
    choices
        .iter()
        .map(|(id, letter)| format!("{id} {letter}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The comment that answers: the choices, then the note as a paragraph of its own.
pub fn answer_body(choices: &[(String, char)], note: &str) -> String {
    [choices_text(choices), note.trim().to_string()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The choices a comment starts with, such as `B1 A, B2 B` or `b1: a; b2=b`. Pairs for a point or
/// option the findings don't have are skipped; a later pair for the same point wins.
pub fn parse_choices(body: &str, points: &[DecisionPoint]) -> Vec<(String, char)> {
    let mut choices: Vec<(String, char)> = Vec::new();
    let mut rest = body;
    loop {
        rest = rest.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '`'));
        let Some((id, letter, after)) = choice(rest) else {
            break;
        };
        rest = after;
        let Some(point) = points.iter().find(|p| p.id.eq_ignore_ascii_case(&id)) else {
            continue;
        };
        if !point.has_option(letter) {
            continue;
        }
        choices.retain(|(known, _)| *known != point.id);
        choices.push((point.id.clone(), letter));
    }
    choices
}

/// `B1 A` at the start of `text` → `("B1", 'A', rest)`. A lower-case letter counts only when a
/// separator or the next pair follows it, so `b1 a bit` is prose.
fn choice(text: &str) -> Option<(String, char, &str)> {
    let rest = text.strip_prefix(['B', 'b'])?;
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if digits == 0 {
        return None;
    }
    let number = &rest[..digits];
    let rest = rest[digits..].trim_start_matches([' ', '\t', ':', '=']);
    let mut chars = rest.chars();
    let letter = chars.next().filter(char::is_ascii_alphabetic)?;
    let after = chars.as_str();
    if after.starts_with(|c: char| c.is_alphanumeric()) {
        return None;
    }
    if letter.is_ascii_lowercase() {
        let tail = after.trim_start_matches([' ', '\t']);
        let separated = tail.is_empty()
            || tail.starts_with([',', ';', '.', '`', '\n', '\r'])
            || choice(tail).is_some();
        if !separated {
            return None;
        }
    }
    Some((format!("B{number}"), letter.to_ascii_uppercase(), after))
}

/// The comments after the findings comment that may answer it: newer than `since` (the move that
/// started the research), without a dispatch marker, and not by a bot. Oldest first. When the
/// findings comment is not among `comments`, every comment is after it.
pub fn after_findings<'a>(
    comments: &'a [Comment],
    findings_url: &str,
    since: &str,
) -> Vec<&'a Comment> {
    let start = comments
        .iter()
        .position(|c| c.url == findings_url)
        .map_or(0, |index| index + 1);
    comments[start..]
        .iter()
        .filter(|c| c.created_at.as_str() > since)
        .filter(|c| !c.body.lines().any(is_marker_line))
        .filter(|c| c.author.as_deref().is_some_and(|a| !a.ends_with("[bot]")))
        .collect()
}

/// The comments of [`after_findings`] whose author `can_write` to the repository.
pub fn find_answers(
    comments: &[Comment],
    findings_url: &str,
    since: &str,
    points: &[DecisionPoint],
    can_write: impl Fn(&str) -> bool,
) -> Vec<Answer> {
    after_findings(comments, findings_url, since)
        .into_iter()
        .filter_map(|comment| {
            let author = comment.author.clone()?;
            can_write(&author).then(|| Answer {
                choices: parse_choices(&comment.body, points),
                author,
                created_at: comment.created_at.clone(),
                url: comment.url.clone(),
                body: comment.body.clone(),
            })
        })
        .collect()
}

/// Per point, the option the latest answer chose, in point order.
pub fn chosen(answers: &[Answer]) -> Vec<(String, char)> {
    let mut chosen: Vec<(String, char)> = Vec::new();
    for (id, letter) in answers.iter().flat_map(|a| a.choices.iter()) {
        chosen.retain(|(known, _)| known != id);
        chosen.push((id.clone(), *letter));
    }
    chosen.sort_by_key(|(id, _)| id[1..].parse::<u32>().unwrap_or(u32::MAX));
    chosen
}

/// The `## Answers` section of the implementation task file. `None` when the comments couldn't
/// be read.
pub fn answers_section(
    answers: Option<&[Answer]>,
    findings_url: &str,
    kind: ItemKind,
) -> String {
    let noun = kind.noun();
    let Some(answers) = answers else {
        return format!(
            "## Answers\n\nCodeScope couldn't read them. They are in the {noun}'s comments after \
             the findings: {findings_url}\n"
        );
    };
    if answers.is_empty() {
        return format!(
            "## Answers\n\nNobody answered the findings on the {noun} before it moved to Ready. \
             Where they leave a choice, take the advice.\n"
        );
    }
    let mut section = format!(
        "## Answers\n\nThese comments on the {noun} answer the findings, oldest first. Where \
         they choose an option, it replaces the advice.\n"
    );
    for answer in answers {
        let quoted: Vec<String> = answer
            .body
            .trim()
            .lines()
            .map(|line| format!("> {line}").trim_end().to_string())
            .collect();
        section.push_str(&format!(
            "\nFrom @{} at {} ({}):\n\n{}\n",
            answer.author,
            answer.created_at,
            answer.url,
            quoted.join("\n")
        ));
    }
    let chosen = chosen(answers);
    if !chosen.is_empty() {
        section.push_str(&format!("\nChosen: {}.\n", choices_text(&chosen)));
    }
    section
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::board::findings::DecisionOption;

    fn point(id: &str, letters: &[char]) -> DecisionPoint {
        DecisionPoint {
            id: id.into(),
            question: "Q?".into(),
            context: String::new(),
            options: letters
                .iter()
                .map(|&letter| DecisionOption {
                    letter,
                    text: "x".into(),
                })
                .collect(),
            advice: letters[0],
        }
    }

    fn points() -> Vec<DecisionPoint> {
        vec![point("B1", &['A', 'B']), point("B2", &['A', 'B', 'C'])]
    }

    fn pairs(list: &[(&str, char)]) -> Vec<(String, char)> {
        list.iter().map(|(id, l)| (id.to_string(), *l)).collect()
    }

    fn comment(n: u32, author: &str, body: &str, at: &str) -> Comment {
        Comment {
            url: format!("https://github.com/o/r/issues/1#issuecomment-{n}"),
            author: Some(author.into()),
            body: body.into(),
            created_at: at.into(),
        }
    }

    #[test]
    fn reads_the_choices_a_comment_starts_with() {
        let points = points();
        let both = pairs(&[("B1", 'A'), ("B2", 'C')]);
        assert_eq!(parse_choices("B1 A, B2 C\n\nKeep it small.", &points), both);
        assert_eq!(parse_choices("`B1 A, B2 C`", &points), both);
        assert_eq!(parse_choices("b1: a; b2=c", &points), both);
        assert_eq!(parse_choices("B1 A B2 C", &points), both);
        assert_eq!(parse_choices("B1 B. Because it is simpler.", &points), pairs(&[("B1", 'B')]));
        assert_eq!(parse_choices("B1 A, B1 B", &points), pairs(&[("B1", 'B')]));
    }

    #[test]
    fn skips_unknown_points_and_options_and_reads_prose_as_prose() {
        let points = points();
        assert_eq!(parse_choices("B3 A, B1 D, B2 B", &points), pairs(&[("B2", 'B')]));
        assert!(parse_choices("b1 a bit later, please", &points).is_empty());
        assert!(parse_choices("I'd go with B1 A", &points).is_empty());
        assert!(parse_choices("B1 Alright", &points).is_empty());
    }

    #[test]
    fn the_answer_comment_is_the_choices_and_the_note() {
        let choices = pairs(&[("B1", 'A'), ("B2", 'B')]);
        assert_eq!(answer_body(&choices, "  "), "B1 A, B2 B");
        assert_eq!(answer_body(&choices, "Keep it small.\n"), "B1 A, B2 B\n\nKeep it small.");
        assert_eq!(answer_body(&[], "Only this."), "Only this.");
    }

    #[test]
    fn answers_are_comments_after_the_findings_by_people_with_write_access() {
        let findings = comment(2, "maui1911", "## Cause\n\nX.\n<!-- dispatch v=1 -->", "T10:05");
        let comments = vec![
            comment(1, "maui1911", "B1 B", "T09:00"),
            findings.clone(),
            comment(3, "dependabot[bot]", "B1 B", "T10:06"),
            comment(4, "maui1911", "Done.\n<!-- dispatch v=1 phase=implementation -->", "T10:07"),
            comment(5, "outsider", "B1 B", "T10:08"),
            comment(6, "sanne", "B1 B, B2 C", "T10:09"),
            comment(7, "maui1911", "Also keep the old rows.", "T10:10"),
            comment(8, "maui1911", "Done.\n<!--DISPATCH v=1 phase=review -->", "T10:11"),
        ];
        let found = find_answers(&comments, &findings.url, "T10:00", &points(), |login| {
            login != "outsider"
        });
        let summary: Vec<(&str, Vec<(String, char)>)> = found
            .iter()
            .map(|a| (a.author.as_str(), a.choices.clone()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("sanne", pairs(&[("B1", 'B'), ("B2", 'C')])),
                ("maui1911", Vec::new()),
            ]
        );
        assert_eq!(found[1].body, "Also keep the old rows.");
    }

    #[test]
    fn without_the_findings_comment_every_newer_comment_is_after_it() {
        let comments = vec![
            comment(1, "maui1911", "Earlier talk.", "T09:00"),
            comment(9, "maui1911", "B1 A", "T11:00"),
        ];
        let found = after_findings(&comments, "https://gone", "T10:00");
        assert_eq!(found, vec![&comments[1]]);
    }

    #[test]
    fn the_latest_answer_wins_per_point() {
        let answer = |choices: &[(&str, char)]| Answer {
            author: "a".into(),
            created_at: "t".into(),
            url: "u".into(),
            choices: pairs(choices),
            body: String::new(),
        };
        let answers = [
            answer(&[("B2", 'A'), ("B1", 'A')]),
            answer(&[]),
            answer(&[("B1", 'B')]),
        ];
        assert_eq!(chosen(&answers), pairs(&[("B1", 'B'), ("B2", 'A')]));
    }

    #[test]
    fn the_task_file_section_quotes_each_answer() {
        let answers = [Answer {
            author: "sanne".into(),
            created_at: "2026-11-25T18:00:00Z".into(),
            url: "https://c/1".into(),
            choices: pairs(&[("B1", 'B')]),
            body: "B1 B\n\nKeep it small.\n".into(),
        }];
        assert_eq!(
            answers_section(Some(&answers), "https://f", ItemKind::Issue),
            "## Answers\n\nThese comments on the issue answer the findings, oldest first. Where \
             they choose an option, it replaces the advice.\n\n\
             From @sanne at 2026-11-25T18:00:00Z (https://c/1):\n\n\
             > B1 B\n>\n> Keep it small.\n\n\
             Chosen: B1 B.\n"
        );
        assert!(
            answers_section(Some(&[]), "https://f", ItemKind::Issue).contains("Nobody answered")
        );
        assert!(
            answers_section(None, "https://f", ItemKind::Issue)
                .ends_with("after the findings: https://f\n")
        );
    }

    #[test]
    fn the_answers_to_research_on_a_pull_request_item_are_on_the_pull_request() {
        let answers = [Answer {
            author: "sanne".into(),
            created_at: "2026-11-25T18:00:00Z".into(),
            url: "https://c/1".into(),
            choices: pairs(&[("B1", 'B')]),
            body: "B1 B\n".into(),
        }];
        let item = ItemKind::PullRequest;
        let section = answers_section(Some(&answers), "https://f", item);
        assert!(
            section.starts_with("## Answers\n\nThese comments on the pull request answer"),
            "{section}"
        );
        assert!(
            answers_section(Some(&[]), "https://f", item)
                .contains("Nobody answered the findings on the pull request before")
        );
        assert!(
            answers_section(None, "https://f", item)
                .contains("They are in the pull request's comments after the findings")
        );
    }
}
