//! Research findings as the board protocol shapes them: checked before CodeScope posts them,
//! and turned into the comment that carries the marker.

use std::fmt;

use serde::{Deserialize, Serialize};

const MAX_LENGTH: usize = 60_000;
const MAX_LISTED: usize = 4;
const CAUSE: &str = "## Cause";
const APPROACH: &str = "## Approach";
const DECISION_POINTS: &str = "## Decision points";

/// What a research round produced, as the marker names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Findings,
    DecisionPoints,
}

impl Outcome {
    pub fn marker_value(self) -> &'static str {
        match self {
            Outcome::Findings => "findings",
            Outcome::DecisionPoints => "decision-points",
        }
    }
}

/// One open choice: `### B1 <question>` with lettered options and the advised one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionPoint {
    /// `B1`, `B2`, …
    pub id: String,
    pub question: String,
    pub options: Vec<char>,
    pub advice: char,
}

/// Findings that passed [`check_findings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Findings {
    pub decision_points: Vec<DecisionPoint>,
}

impl Findings {
    pub fn outcome(&self) -> Outcome {
        if self.decision_points.is_empty() {
            Outcome::Findings
        } else {
            Outcome::DecisionPoints
        }
    }
}

/// Why findings can't be posted yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    Missing(&'static str),
    Empty(&'static str),
    ApproachBeforeCause,
    DecisionPointsNotLast,
    /// A `## ` heading about decision points that is not exactly `## Decision points`.
    DecisionPointsHeading,
    /// A `### B<n>` heading outside `## Decision points`.
    StrayDecisionPoint,
    /// A point's heading is not the next `B<n>`, which it holds.
    Numbering(String),
    TooFewOptions(String),
    NoAdvice(String),
    AdviceNotAnOption(String),
    Marker,
    TooLong(usize),
    OpenFence,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::Missing(heading) => write!(f, "no {heading}"),
            Problem::Empty(heading) => write!(f, "{heading} is empty"),
            Problem::ApproachBeforeCause => write!(f, "{APPROACH} comes before {CAUSE}"),
            Problem::DecisionPointsNotLast => {
                write!(f, "{DECISION_POINTS} is not the last section")
            }
            Problem::DecisionPointsHeading => {
                write!(f, "decision points go under a heading that reads {DECISION_POINTS}")
            }
            Problem::StrayDecisionPoint => {
                write!(f, "a ### B heading sits outside {DECISION_POINTS}")
            }
            Problem::Numbering(expected) => {
                write!(f, "decision point {expected} should be headed ### {expected}")
            }
            Problem::TooFewOptions(id) => {
                write!(f, "{id} needs at least two options starting with **A**, **B**")
            }
            Problem::NoAdvice(id) => write!(f, "{id} has no Advice line"),
            Problem::AdviceNotAnOption(id) => {
                write!(f, "{id} advises a letter it has no option for")
            }
            Problem::Marker => {
                write!(f, "it contains a dispatch marker, which only CodeScope writes")
            }
            Problem::TooLong(length) => {
                write!(f, "it is {length} bytes, more than {MAX_LENGTH}")
            }
            Problem::OpenFence => write!(f, "a code block is not closed"),
        }
    }
}

struct Section<'a> {
    heading: String,
    lines: Vec<&'a str>,
}

/// Check findings against the board protocol: `## Cause` before `## Approach`, both with text;
/// other sections anywhere before `## Decision points`, which comes last when there is one.
pub fn check_findings(text: &str) -> Result<Findings, Vec<Problem>> {
    let mut problems = Vec::new();
    if text.len() > MAX_LENGTH {
        problems.push(Problem::TooLong(text.len()));
    }
    if text.split(['\n', '\r']).any(is_marker_line) {
        problems.push(Problem::Marker);
    }
    let top = sections(text, "## ");
    let position = |heading: &str| top.iter().position(|s| s.heading.eq_ignore_ascii_case(heading));

    for heading in [CAUSE, APPROACH] {
        match position(heading) {
            None => problems.push(Problem::Missing(heading)),
            Some(index) if is_blank(&top[index].lines) => {
                problems.push(Problem::Empty(heading))
            }
            Some(_) => {}
        }
    }
    if let (Some(cause), Some(approach)) = (position(CAUSE), position(APPROACH))
        && approach < cause
    {
        problems.push(Problem::ApproachBeforeCause);
    }

    let near_miss = top.iter().any(|s| {
        !s.heading.eq_ignore_ascii_case(DECISION_POINTS)
            && s.heading.to_ascii_lowercase().contains("decision point")
    });
    let points_inside = position(DECISION_POINTS)
        .map(|index| point_headings(&top[index].lines.join("\n")))
        .unwrap_or(0);
    if near_miss {
        problems.push(Problem::DecisionPointsHeading);
    } else if point_headings(text) > points_inside {
        problems.push(Problem::StrayDecisionPoint);
    }

    let mut decision_points = Vec::new();
    if let Some(index) = position(DECISION_POINTS) {
        if index + 1 != top.len() {
            problems.push(Problem::DecisionPointsNotLast);
        }
        let body = top[index].lines.join("\n");
        for (number, point) in sections(&body, "### ").iter().enumerate() {
            match decision_point(point, number + 1) {
                Ok(point) => decision_points.push(point),
                Err(problem) => problems.push(problem),
            }
        }
    }

    if ends_in_code(text) {
        problems.push(Problem::OpenFence);
    }

    if problems.is_empty() {
        Ok(Findings { decision_points })
    } else {
        Err(problems)
    }
}

#[derive(Default)]
struct Fences {
    open: Option<(char, usize)>,
}

impl Fences {
    fn is_code(&mut self, line: &str) -> bool {
        let trimmed = line.trim_start();
        let Some(mark) = trimmed.chars().next().filter(|c| matches!(c, '`' | '~')) else {
            return self.open.is_some();
        };
        let run = trimmed.chars().take_while(|c| *c == mark).count();
        let rest = &trimmed[run..];
        match self.open {
            Some((open, length)) => {
                if mark == open && run >= length && rest.trim().is_empty() {
                    self.open = None;
                }
                true
            }
            None if run >= 3 && !(mark == '`' && rest.contains('`')) => {
                self.open = Some((mark, run));
                true
            }
            None => false,
        }
    }
}

fn ends_in_code(text: &str) -> bool {
    let mut fences = Fences::default();
    for line in text.lines() {
        fences.is_code(line);
    }
    fences.open.is_some()
}

/// The sections that start with a heading of `prefix`'s level, outside fenced code blocks.
/// Text before the first heading is left out.
fn sections<'a>(text: &'a str, prefix: &str) -> Vec<Section<'a>> {
    let mut sections: Vec<Section> = Vec::new();
    let mut fences = Fences::default();
    for line in text.lines() {
        if !fences.is_code(line) && let Some(rest) = line.strip_prefix(prefix) {
            sections.push(Section {
                heading: format!("{prefix}{}", rest.trim()),
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(section) = sections.last_mut() {
            section.lines.push(line);
        }
    }
    sections
}

fn point_headings(text: &str) -> usize {
    sections(text, "### ")
        .iter()
        .filter(|s| {
            let id = s.heading.trim_start_matches('#').split_whitespace().next().unwrap_or("");
            let mut chars = id.chars();
            matches!(chars.next(), Some('B' | 'b'))
                && !chars.as_str().is_empty()
                && chars.all(|c| c.is_ascii_digit())
        })
        .count()
}

fn is_blank(lines: &[&str]) -> bool {
    lines.iter().all(|line| line.trim().is_empty())
}

fn decision_point(section: &Section, number: usize) -> Result<DecisionPoint, Problem> {
    let expected = format!("B{number}");
    let title = section.heading.trim_start_matches('#').trim();
    let (id, question) = title.split_once(char::is_whitespace).unwrap_or((title, ""));
    if !id.eq_ignore_ascii_case(&expected) {
        return Err(Problem::Numbering(expected));
    }
    let options: Vec<char> = section.lines.iter().filter_map(|l| option_letter(l)).collect();
    if options.len() < 2 {
        return Err(Problem::TooFewOptions(expected));
    }
    let advice = section
        .lines
        .iter()
        .find_map(|l| advice_letter(l))
        .ok_or_else(|| Problem::NoAdvice(expected.clone()))?;
    if !options.contains(&advice) {
        return Err(Problem::AdviceNotAnOption(expected));
    }
    Ok(DecisionPoint {
        id: expected,
        question: question.trim().to_string(),
        options,
        advice,
    })
}

/// `- **A** …`, `* **B.** …` or `1. **C:** …` → the letter.
fn option_letter(line: &str) -> Option<char> {
    let item = line.trim_start();
    let item = item
        .strip_prefix("- ")
        .or_else(|| item.strip_prefix("* "))
        .or_else(|| item.strip_prefix("+ "))
        .or_else(|| {
            let digits = item.find(|c: char| !c.is_ascii_digit())?;
            (digits > 0).then(|| item[digits..].strip_prefix(". "))?
        })?;
    let mut chars = item.trim_start().strip_prefix("**")?.chars();
    let letter = chars.next().filter(char::is_ascii_uppercase)?;
    let rest = chars.as_str();
    let rest = rest.strip_prefix(['.', ':', ')']).unwrap_or(rest);
    rest.starts_with("**").then_some(letter)
}

/// `Advice: A, …`, also in bold → the letter, which stands as a word of its own.
fn advice_letter(line: &str) -> Option<char> {
    let plain = line.replace("**", "");
    let plain = plain.trim();
    plain.get(..7).filter(|p| p.eq_ignore_ascii_case("advice:"))?;
    let mut rest = plain[7..].trim_start().chars();
    let letter = rest.next().filter(char::is_ascii_uppercase)?;
    rest.next().is_none_or(|next| !next.is_alphanumeric()).then_some(letter)
}

/// Whether `line` is a dispatch marker, in any spelling GitHub hides as a comment.
pub fn is_marker_line(line: &str) -> bool {
    line.trim_start()
        .strip_prefix("<!--")
        .map(str::trim_start)
        .and_then(|rest| rest.get(..8))
        .is_some_and(|word| word.eq_ignore_ascii_case("dispatch"))
}

/// The comment that posts findings: the text, a `**Next step:**` line and the marker.
pub fn comment_body(text: &str, findings: &Findings) -> String {
    let next_step = match findings.outcome() {
        Outcome::Findings => "move the item to Ready to implement this approach.".to_string(),
        Outcome::DecisionPoints => {
            let example: Vec<String> = findings
                .decision_points
                .iter()
                .map(|p| format!("{} {}", p.id, p.advice))
                .collect();
            format!(
                "answer in a comment such as `{}`, then move the item to Ready.",
                example.join(", ")
            )
        }
    };
    let marker = format!(
        "<!-- dispatch v=1 phase=research outcome={} by=codescope -->",
        findings.outcome().marker_value()
    );
    format!("{}\n\n**Next step:** {next_step}\n{marker}\n", text.trim_end())
}

/// The first few problems joined on one line, and how many more there are.
pub fn problems_list(problems: &[Problem]) -> String {
    let mut list: Vec<String> =
        problems.iter().take(MAX_LISTED).map(ToString::to_string).collect();
    if problems.len() > MAX_LISTED {
        list.push(format!("and {} more", problems.len() - MAX_LISTED));
    }
    list.join("; ")
}

/// One line for the session that wrote the findings, saying what to fix: fixed words and the
/// headings CodeScope numbers itself, nothing taken from the file.
pub fn problems_message(problems: &[Problem]) -> String {
    format!(
        "CodeScope cannot post {} yet: {}. Fix the file and stop.",
        super::dispatch::FINDINGS_FILE,
        problems_list(problems)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_POINTS: &str = "# Findings for #408

## Cause

Nothing reads the file.

## Evidence

```markdown
## Approach
not a heading inside a fence
```

## Approach

Post it.

## Decision points

### B1 Which branch?
Context.

- **A** Stack it.
- **B.** Wait.
- **C:** Fold it in.

Advice: A, it keeps the slice small.

### B2 Extra sections?

1. **A** Allow them.
2. **B** Reject them.

**Advice:** B, strict is simpler.
";

    #[test]
    fn accepts_findings_with_extra_sections_and_reads_the_decision_points() {
        let findings = check_findings(WITH_POINTS).unwrap();
        assert_eq!(findings.outcome(), Outcome::DecisionPoints);
        assert_eq!(
            findings.decision_points,
            vec![
                DecisionPoint {
                    id: "B1".into(),
                    question: "Which branch?".into(),
                    options: vec!['A', 'B', 'C'],
                    advice: 'A',
                },
                DecisionPoint {
                    id: "B2".into(),
                    question: "Extra sections?".into(),
                    options: vec!['A', 'B'],
                    advice: 'B',
                },
            ]
        );
    }

    #[test]
    fn findings_without_decision_points_are_plain_findings() {
        let text = "## Cause\n\nX.\n\n## Approach\n\nY.\n";
        assert_eq!(check_findings(text).unwrap().outcome(), Outcome::Findings);
        let empty_points = format!("{text}\n## Decision points\n\nNone.\n");
        assert_eq!(check_findings(&empty_points).unwrap().outcome(), Outcome::Findings);
    }

    #[test]
    fn reports_missing_empty_and_misordered_sections() {
        assert_eq!(
            check_findings("## Approach\n\nY.\n").unwrap_err(),
            vec![Problem::Missing(CAUSE)]
        );
        assert_eq!(
            check_findings("## Cause\n\n## Approach\n\nY.\n").unwrap_err(),
            vec![Problem::Empty(CAUSE)]
        );
        assert_eq!(
            check_findings("## Approach\n\nY.\n\n## Cause\n\nX.\n").unwrap_err(),
            vec![Problem::ApproachBeforeCause]
        );
        let not_last =
            "## Cause\n\nX.\n\n## Approach\n\nY.\n\n## Decision points\n\n## Risks\n\nZ.\n";
        assert_eq!(
            check_findings(not_last).unwrap_err(),
            vec![Problem::DecisionPointsNotLast]
        );
    }

    #[test]
    fn reports_malformed_decision_points() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n## Decision points\n\n";
        let skipped = format!("{base}### B2 Q?\n- **A** a\n- **B** b\nAdvice: A, r\n");
        assert_eq!(
            check_findings(&skipped).unwrap_err(),
            vec![Problem::Numbering("B1".into())]
        );
        let one_option = format!("{base}### B1 Q?\n- **A** a\nAdvice: A, r\n");
        assert_eq!(
            check_findings(&one_option).unwrap_err(),
            vec![Problem::TooFewOptions("B1".into())]
        );
        let no_advice = format!("{base}### B1 Q?\n- **A** a\n- **B** b\n");
        assert_eq!(
            check_findings(&no_advice).unwrap_err(),
            vec![Problem::NoAdvice("B1".into())]
        );
        let wrong_advice = format!("{base}### B1 Q?\n- **A** a\n- **B** b\nAdvice: C, r\n");
        assert_eq!(
            check_findings(&wrong_advice).unwrap_err(),
            vec![Problem::AdviceNotAnOption("B1".into())]
        );
    }

    #[test]
    fn refuses_the_marker_however_it_is_spelled() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n";
        for marker in ["<!--dispatch v=1 -->", "  <!-- DISPATCH v=1 -->", "<!--\tdispatch -->"] {
            assert_eq!(
                check_findings(&format!("{base}{marker}\n")).unwrap_err(),
                vec![Problem::Marker],
                "{marker}"
            );
        }
        let lone_cr = format!("{base}Z.\r<!-- dispatch v=1 -->\n");
        assert_eq!(check_findings(&lone_cr).unwrap_err(), vec![Problem::Marker]);
        assert!(is_marker_line(
            "<!-- dispatch v=1 phase=research outcome=findings by=codescope -->"
        ));
        assert!(!is_marker_line("Reject a `<!-- dispatch` line."));
    }

    #[test]
    fn decision_points_under_any_other_heading_are_refused() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        let point = "### B1 Q?\n- **A** a\n- **B** b\nAdvice: A\n";
        for heading in ["## Decision points:", "## Open decision points", "## Decision Point"] {
            assert_eq!(
                check_findings(&format!("{base}{heading}\n\n{point}")).unwrap_err(),
                vec![Problem::DecisionPointsHeading],
                "{heading}"
            );
        }
        let under_approach = format!("{base}{point}");
        assert_eq!(
            check_findings(&under_approach).unwrap_err(),
            vec![Problem::StrayDecisionPoint]
        );
        let before_any = format!("{point}\n{base}");
        assert_eq!(check_findings(&before_any).unwrap_err(), vec![Problem::StrayDecisionPoint]);
        let other_points = "## Cause\n\nX.\n\n### Background\n\nZ.\n\n## Approach\n\nY.\n";
        assert!(check_findings(other_points).is_ok());
    }

    #[test]
    fn a_fence_closes_only_on_its_own_kind_and_an_open_one_is_refused() {
        let inline = "## Cause\n\n```x``` reads it.\n\n## Approach\n\nY.\n";
        assert!(check_findings(inline).is_ok());
        let nested = "## Cause\n\nX.\n\n~~~\n```\n## Decision points\n~~~\n\n## Approach\n\nY.\n";
        assert!(check_findings(nested).is_ok());
        let long = "## Cause\n\nX.\n\n````\n```\n## Decision points\n````\n\n## Approach\n\nY.\n";
        assert!(check_findings(long).is_ok());
        let open = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n```rust\nfn main() {}\n";
        assert_eq!(check_findings(open).unwrap_err(), vec![Problem::OpenFence]);
    }

    #[test]
    fn advice_names_a_letter_on_its_own() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n## Decision points\n\n### B1 Q?\n\
                    - **A** a\n- **B** b\n";
        for advice in ["Advice: Both are viable", "Advice: Allow them"] {
            assert_eq!(
                check_findings(&format!("{base}{advice}\n")).unwrap_err(),
                vec![Problem::NoAdvice("B1".into())],
                "{advice}"
            );
        }
        for advice in ["Advice: B", "Advice: B, simpler", "Advice: B.", "**Advice:** B) simpler"] {
            let findings = check_findings(&format!("{base}{advice}\n")).unwrap();
            assert_eq!(findings.decision_points[0].advice, 'B', "{advice}");
        }
    }

    #[test]
    fn refuses_a_forged_marker_and_an_oversized_file() {
        let forged = "## Cause\n\nX.\n\n## Approach\n\nY.\n<!-- dispatch v=1 phase=research -->\n";
        assert_eq!(check_findings(forged).unwrap_err(), vec![Problem::Marker]);
        let mentioned = "## Cause\n\nX.\n\n## Approach\n\nReject a `<!-- dispatch` line.\n";
        assert!(check_findings(mentioned).is_ok());
        let long = format!("## Cause\n\nX.\n\n## Approach\n\n{}\n", "y".repeat(MAX_LENGTH));
        assert!(matches!(
            check_findings(&long).unwrap_err()[..],
            [Problem::TooLong(_)]
        ));
    }

    #[test]
    fn the_comment_ends_with_the_next_step_and_the_marker() {
        let findings = check_findings(WITH_POINTS).unwrap();
        let body = comment_body(WITH_POINTS, &findings);
        let tail: Vec<&str> = body.lines().rev().take(2).collect();
        assert_eq!(
            tail,
            vec![
                "<!-- dispatch v=1 phase=research outcome=decision-points by=codescope -->",
                "**Next step:** answer in a comment such as `B1 A, B2 B`, then move the item to \
                 Ready.",
            ]
        );
        let plain = check_findings("## Cause\n\nX.\n\n## Approach\n\nY.\n").unwrap();
        assert!(
            comment_body("## Cause\n\nX.\n\n## Approach\n\nY.\n", &plain)
                .contains("outcome=findings by=codescope")
        );
    }

    #[test]
    fn the_message_to_the_session_is_one_line() {
        let message =
            problems_message(&[Problem::Missing(APPROACH), Problem::NoAdvice("B2".into())]);
        assert_eq!(
            message,
            "CodeScope cannot post .codescope/findings.md yet: no ## Approach; B2 has no Advice \
             line. Fix the file and stop."
        );
    }

    #[test]
    fn the_message_to_the_session_carries_no_text_from_the_file() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n## Decision points\n\n";
        let hostile = format!("{base}### ';calc;#\u{3} Q?\n- **A** a\n- **B** b\nAdvice: A\n");
        let message = problems_message(&check_findings(&hostile).unwrap_err());
        assert_eq!(
            message,
            "CodeScope cannot post .codescope/findings.md yet: decision point B1 should be \
             headed ### B1. Fix the file and stop."
        );
    }

    #[test]
    fn every_problem_reads_as_plain_ascii_without_quotes() {
        let all = [
            Problem::Missing(CAUSE),
            Problem::Empty(APPROACH),
            Problem::ApproachBeforeCause,
            Problem::DecisionPointsNotLast,
            Problem::DecisionPointsHeading,
            Problem::StrayDecisionPoint,
            Problem::Numbering("B2".into()),
            Problem::TooFewOptions("B1".into()),
            Problem::NoAdvice("B1".into()),
            Problem::AdviceNotAnOption("B1".into()),
            Problem::Marker,
            Problem::TooLong(70_000),
            Problem::OpenFence,
        ];
        let message = problems_message(&all);
        let plain = |c: char| (c.is_ascii_graphic() || c == ' ') && !matches!(c, '\'' | '"' | '`');
        assert!(message.chars().all(plain), "{message}");
    }

    #[test]
    fn the_message_names_a_few_problems_and_counts_the_rest() {
        let problems: Vec<Problem> = (1..=7).map(|n| Problem::NoAdvice(format!("B{n}"))).collect();
        assert_eq!(
            problems_message(&problems),
            "CodeScope cannot post .codescope/findings.md yet: B1 has no Advice line; B2 has no \
             Advice line; B3 has no Advice line; B4 has no Advice line; and 3 more. Fix the file \
             and stop."
        );
    }
}
