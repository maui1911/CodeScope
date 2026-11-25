//! Research findings as the board protocol shapes them: checked before CodeScope posts them,
//! and turned into the comment that carries the marker.

use std::fmt;

use serde::{Deserialize, Serialize};

const MAX_LENGTH: usize = 60_000;
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
    /// A point's heading is not the next `B<n>`.
    Numbering { expected: String, found: String },
    TooFewOptions(String),
    NoAdvice(String),
    AdviceNotAnOption(String),
    Marker,
    TooLong(usize),
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
            Problem::Numbering { expected, found } => {
                write!(f, "decision point {found} should be ### {expected}")
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
                write!(f, "it is {length} characters, more than {MAX_LENGTH}")
            }
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
    if text.lines().any(|line| line.trim_start().starts_with("<!-- dispatch")) {
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

    if problems.is_empty() {
        Ok(Findings { decision_points })
    } else {
        Err(problems)
    }
}

/// The sections that start with a heading of `prefix`'s level, outside fenced code blocks.
/// Text before the first heading is left out.
fn sections<'a>(text: &'a str, prefix: &str) -> Vec<Section<'a>> {
    let mut sections: Vec<Section> = Vec::new();
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
        }
        if !fenced && let Some(rest) = line.strip_prefix(prefix) {
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

fn is_blank(lines: &[&str]) -> bool {
    lines.iter().all(|line| line.trim().is_empty())
}

fn decision_point(section: &Section, number: usize) -> Result<DecisionPoint, Problem> {
    let expected = format!("B{number}");
    let title = section.heading.trim_start_matches('#').trim();
    let (id, question) = title.split_once(char::is_whitespace).unwrap_or((title, ""));
    if !id.eq_ignore_ascii_case(&expected) {
        return Err(Problem::Numbering {
            expected,
            found: id.to_string(),
        });
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

/// `Advice: A, …`, also in bold → the letter.
fn advice_letter(line: &str) -> Option<char> {
    let plain = line.replace("**", "");
    let plain = plain.trim();
    plain.get(..7).filter(|p| p.eq_ignore_ascii_case("advice:"))?;
    plain[7..]
        .trim_start()
        .chars()
        .next()
        .filter(char::is_ascii_uppercase)
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

/// One line for the session that wrote the findings, saying what to fix.
pub fn problems_message(problems: &[Problem]) -> String {
    let list: Vec<String> = problems.iter().map(ToString::to_string).collect();
    format!(
        "CodeScope can't post {} yet: {}. Fix the file and stop.",
        super::dispatch::FINDINGS_FILE,
        list.join("; ")
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
            vec![Problem::Numbering {
                expected: "B1".into(),
                found: "B2".into()
            }]
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
            "CodeScope can't post .codescope/findings.md yet: no ## Approach; B2 has no Advice \
             line. Fix the file and stop."
        );
    }
}
