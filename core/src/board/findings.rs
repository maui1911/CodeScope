//! Research findings as the board protocol shapes them: checked before CodeScope posts them,
//! and turned into the comment that carries the marker.

use std::fmt;
use std::io::Read as _;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::dispatch::{FINDINGS_FILE, linked_part};

const MAX_LENGTH: usize = 60_000;
const READ_LIMIT: usize = 4 * MAX_LENGTH;
const MAX_LISTED: usize = 4;
const CAUSE: &str = "## Cause";
const APPROACH: &str = "## Approach";
const DECISION_POINTS: &str = "## Decision points";
const ZERO_WIDTH_SPACE: char = '\u{200b}';
const DIAGRAMS: [&str; 4] = ["mermaid", "geojson", "topojson", "stl"];

/// What a research round produced, as the marker names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    /// Cause and approach only: the item can go to Ready as it is.
    Findings,
    /// At least one `### B<n>` point waits for an answer on the issue.
    DecisionPoints,
}

impl Outcome {
    /// The `outcome=` value in the marker, the same as the serialized form.
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
    /// The heading's text after the id, trimmed; may be empty.
    pub question: String,
    /// The option letters in the order listed, `A` first.
    pub options: Vec<char>,
    /// One of `options`.
    pub advice: char,
}

/// Findings that passed [`check_findings`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Findings {
    /// In order, numbered from `B1`; empty for plain findings.
    pub decision_points: Vec<DecisionPoint>,
}

impl Findings {
    /// [`Outcome::DecisionPoints`] when there is at least one point.
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
    /// The required `##` heading isn't there.
    Missing(&'static str),
    /// The required `##` heading has only blank lines under it.
    Empty(&'static str),
    /// `## Approach` comes before `## Cause`.
    ApproachBeforeCause,
    /// Another `##` section follows `## Decision points`.
    DecisionPointsNotLast,
    /// A `## ` heading about decision points that is not exactly `## Decision points`.
    DecisionPointsHeading,
    /// A `### B<n>` heading outside `## Decision points`.
    StrayDecisionPoint,
    /// `B<n>` named under `## Decision points` without a single `### B<n>` heading there.
    UnheadedDecisionPoints,
    /// A point's heading is not the next `B<n>`, which it holds.
    Numbering(String),
    /// The point, by id, lists fewer than two `**A**` options.
    TooFewOptions(String),
    /// The point, by id, has no `Advice:` line that names a letter.
    NoAdvice(String),
    /// The point, by id, advises a letter none of its options has.
    AdviceNotAnOption(String),
    /// A dispatch marker line, which only CodeScope writes.
    Marker,
    /// The length as posted, over the most a comment holds with the next step added.
    TooLong(usize),
    /// The file's size in bytes, over what CodeScope reads.
    TooLarge(u64),
    /// A fenced code block runs to the end of the file.
    OpenFence,
    /// A closing fence indented less than its opening one, which GitHub reads as a new block
    /// when the code sits in a list item.
    UnevenFence,
    /// The file, or the folder it is in, is a link.
    Link,
    /// A folder, a pipe or a device where the file should be.
    NotAFile,
    /// Reading it failed, for want of permission or otherwise.
    Unreadable,
    /// Its bytes are not UTF-8, as a UTF-16 redirection writes them.
    NotText,
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
            Problem::UnheadedDecisionPoints => {
                write!(f, "each decision point needs a heading ### B1, ### B2 and so on")
            }
            Problem::Numbering(expected) => {
                write!(f, "decision point {expected} should be headed ### {expected}")
            }
            Problem::TooFewOptions(id) => {
                write!(f, "{id} needs at least two options starting with **A**, **B**")
            }
            Problem::NoAdvice(id) => write!(f, "{id} has no line Advice: <letter>, <reason>"),
            Problem::AdviceNotAnOption(id) => {
                write!(f, "{id} advises a letter it has no option for")
            }
            Problem::Marker => {
                write!(f, "it contains a dispatch marker, which only CodeScope writes")
            }
            Problem::TooLong(length) => {
                write!(f, "it is too long to post: {length} characters, at most {MAX_LENGTH}")
            }
            Problem::TooLarge(size) => {
                write!(f, "it is {size} bytes, more than the {READ_LIMIT} CodeScope reads")
            }
            Problem::OpenFence => write!(f, "a code block is not closed"),
            Problem::UnevenFence => {
                write!(f, "a code block closes less indented than it opens")
            }
            Problem::Link => write!(f, "it or its folder is a link"),
            Problem::NotAFile => write!(f, "it is not a plain file"),
            Problem::Unreadable => write!(f, "it cannot be read"),
            Problem::NotText => write!(f, "it is not UTF-8 text"),
        }
    }
}

/// Modified time and length, which tell one version of a file from the next.
pub type FileStamp = (Option<SystemTime>, u64);

/// [`FINDINGS_FILE`] as one read found it.
#[derive(Debug)]
pub struct FindingsFile {
    /// Taken as it was read, so the session hears about each version once.
    pub stamp: FileStamp,
    read: Result<String, Problem>,
}

impl FindingsFile {
    /// The text to post with its findings, or what to fix first.
    pub fn check(&self) -> Result<(&str, Findings), Vec<Problem>> {
        let text = self.read.as_ref().map_err(|problem| vec![problem.clone()])?;
        check_findings(text).map(|findings| (text.as_str(), findings))
    }
}

/// The findings in `worktree`, `None` without a file; a link, an odd file, too much or non-UTF-8
/// text reads as that problem. Line ends come back as `\n`, without byte order marks.
pub fn read_findings(worktree: &Path) -> Option<FindingsFile> {
    let path = worktree.join(FINDINGS_FILE);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => {
            return Some(FindingsFile {
                stamp: (None, 0),
                read: Err(Problem::Unreadable),
            });
        }
    };
    let read = if linked_part(worktree, FINDINGS_FILE).is_some() {
        Err(Problem::Link)
    } else if !metadata.is_file() {
        Err(Problem::NotAFile)
    } else {
        read_text(&path)
    };
    Some(FindingsFile {
        stamp: (metadata.modified().ok(), metadata.len()),
        read,
    })
}

fn read_text(path: &Path) -> Result<String, Problem> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(READ_LIMIT as u64 + 1).read_to_end(&mut bytes))
        .map_err(|_| Problem::Unreadable)?;
    if bytes.len() > READ_LIMIT {
        let size = std::fs::metadata(path).map_or(bytes.len() as u64, |m| m.len());
        return Err(Problem::TooLarge(size));
    }
    let text = String::from_utf8(bytes).map_err(|_| Problem::NotText)?;
    Ok(text
        .replace('\u{feff}', "")
        .replace("\r\n", "\n")
        .replace(['\r', '\u{2028}', '\u{2029}'], "\n"))
}

struct Section<'a> {
    heading: String,
    lines: Vec<&'a str>,
}

/// Check findings against the board protocol: `## Cause` before `## Approach`, both with text;
/// other sections anywhere before `## Decision points`, which comes last when there is one.
pub fn check_findings(text: &str) -> Result<Findings, Vec<Problem>> {
    let text = &normalised(text);
    let mut problems = Vec::new();
    let posted = quiet(text).chars().count();
    if posted > MAX_LENGTH {
        problems.push(Problem::TooLong(posted));
    }
    if text.split(['\n', '\r']).any(is_marker_line) {
        problems.push(Problem::Marker);
    }
    let top = sections(text, 2);
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
        let points = sections(&body, 3);
        for (number, point) in points.iter().enumerate() {
            match decision_point(point, number + 1) {
                Ok(point) => decision_points.push(point),
                Err(problem) => problems.push(problem),
            }
        }
        if points.is_empty() && body.split(|c: char| !c.is_alphanumeric()).any(is_point_id) {
            problems.push(Problem::UnheadedDecisionPoints);
        }
    }

    problems.extend(fence_problem(text));

    if problems.is_empty() {
        Ok(Findings { decision_points })
    } else {
        Err(problems)
    }
}

#[derive(Default)]
struct Fences {
    open: Option<(char, usize, usize)>,
    closed_less_indented: bool,
}

impl Fences {
    fn is_code(&mut self, line: &str) -> bool {
        let trimmed = line.trim_start_matches(' ');
        let indent = line.len() - trimmed.len();
        let fenced = indent <= 3;
        let mark = trimmed.chars().next().filter(|c| fenced && matches!(c, '`' | '~'));
        let Some(mark) = mark else {
            return self.open.is_some();
        };
        let run = trimmed.chars().take_while(|c| *c == mark).count();
        let rest = &trimmed[run..];
        match self.open {
            Some((open, length, opened_at)) => {
                if mark == open && run >= length && rest.trim().is_empty() {
                    self.open = None;
                    self.closed_less_indented |= indent < opened_at;
                }
                true
            }
            None if run >= 3 && !(mark == '`' && rest.contains('`')) => {
                self.open = Some((mark, run, indent));
                true
            }
            None => false,
        }
    }
}

fn fence_problem(text: &str) -> Option<Problem> {
    let mut fences = Fences::default();
    for line in text.lines() {
        fences.is_code(line);
    }
    if fences.open.is_some() {
        Some(Problem::OpenFence)
    } else {
        fences.closed_less_indented.then_some(Problem::UnevenFence)
    }
}

fn sections(text: &str, level: usize) -> Vec<Section<'_>> {
    let mut sections: Vec<Section> = Vec::new();
    let mut fences = Fences::default();
    for line in text.lines() {
        if !fences.is_code(line) && let Some(title) = heading(line, level) {
            sections.push(Section {
                heading: format!("{} {title}", "#".repeat(level)),
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

/// CommonMark: up to three spaces, the `#`s, then a space or tab; a closing run of `#`s drops.
fn heading(line: &str, level: usize) -> Option<&str> {
    let rest = line.trim_start_matches(' ');
    if line.len() - rest.len() > 3 {
        return None;
    }
    let title = rest.strip_prefix(&"#".repeat(level))?;
    if !(title.is_empty() || title.starts_with([' ', '\t'])) {
        return None;
    }
    let title = title.trim();
    let unclosed = title.trim_end_matches('#');
    if unclosed.len() < title.len() && (unclosed.is_empty() || unclosed.ends_with([' ', '\t'])) {
        return Some(unclosed.trim_end());
    }
    Some(title)
}

fn point_headings(text: &str) -> usize {
    sections(text, 3).iter().filter(|s| is_point_id(point_title(s).0)).count()
}

fn point_title<'a>(section: &'a Section) -> (&'a str, &'a str) {
    let title = section.heading.trim_start_matches('#').trim();
    let (id, question) = title.split_once(char::is_whitespace).unwrap_or((title, ""));
    (id.trim_end_matches([':', '.', ')']), question.trim())
}

fn is_point_id(word: &str) -> bool {
    let mut chars = word.chars();
    matches!(chars.next(), Some('B' | 'b'))
        && !chars.as_str().is_empty()
        && chars.all(|c| c.is_ascii_digit())
}

fn is_blank(lines: &[&str]) -> bool {
    lines.iter().all(|line| line.trim().is_empty())
}

fn decision_point(section: &Section, number: usize) -> Result<DecisionPoint, Problem> {
    let expected = format!("B{number}");
    let (id, question) = point_title(section);
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
        question: question.to_string(),
        options,
        advice,
    })
}

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

fn advice_letter(line: &str) -> Option<char> {
    let plain = line.replace("**", "");
    let plain = plain.trim();
    plain.get(..7).filter(|p| p.eq_ignore_ascii_case("advice:"))?;
    let mut rest = plain[7..].trim_start().chars();
    let letter = rest.next().filter(char::is_ascii_uppercase)?;
    let after = rest.as_str();
    let aside = after.strip_prefix(' ').is_some_and(|aside| {
        aside.trim_start().starts_with(['(', '-', '\u{2013}', '\u{2014}'])
    });
    (after.is_empty() || after.starts_with([',', '.', ':', ';', ')']) || aside).then_some(letter)
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
    format!("{}\n\n**Next step:** {next_step}\n{marker}\n", quiet(&normalised(text)))
}

fn normalised(text: &str) -> String {
    let plain: String = text
        .replace("\r\n", "\n")
        .replace(['\r', '\u{2028}', '\u{2029}'], "\n")
        .chars()
        .filter(|c| *c != '\u{feff}' && (!c.is_control() || matches!(c, '\n' | '\t')))
        .collect();
    let blank_lead: usize = plain
        .split_inclusive('\n')
        .take_while(|line| line.trim_matches([' ', '\t', '\n']).is_empty())
        .map(str::len)
        .sum();
    plain[blank_lead..].trim_end().to_string()
}

/// GitHub and bots act on `@name`, a `/command`, HTML, images and diagrams, in code too.
fn quiet(text: &str) -> String {
    let lines: Vec<String> = at_signs(text).lines().map(quiet_line).collect();
    lines.join("\n")
}

fn quiet_line(line: &str) -> String {
    let addresses = web_addresses(line);
    let mut quiet = String::with_capacity(line.len() + 8);
    let mut chars = line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let next = chars.peek().map(|(_, next)| *next);
        match c {
            '@' if addresses.iter().any(|address| address.contains(&at)) => {
                quiet.push_str("%40");
            }
            '@' => {
                quiet.push('@');
                quiet.push(ZERO_WIDTH_SPACE);
            }
            '/' if next.is_some_and(|n| n.is_ascii_alphanumeric()) && starts_word(line, at) => {
                quiet.push('/');
                quiet.push(ZERO_WIDTH_SPACE);
            }
            '<' if next.is_some_and(|n| n.is_ascii_alphabetic() || matches!(n, '/' | '!' | '?'))
                && !starts_web_address(&line[at + 1..]) =>
            {
                quiet.push('<');
                quiet.push(ZERO_WIDTH_SPACE);
            }
            '!' if next == Some('[') => {
                quiet.push('!');
                quiet.push(ZERO_WIDTH_SPACE);
            }
            _ => quiet.push(c),
        }
    }
    disarm_info_strings(&mut quiet);
    quiet
}

fn starts_word(line: &str, at: usize) -> bool {
    let before = &line[..at];
    let link_destination = before.ends_with("](");
    let continues = |c: char| c.is_ascii_alphanumeric() || "/.-_~%:+=&#?@".contains(c);
    !link_destination && !before.chars().next_back().is_some_and(continues)
}

/// The path, query and fragment of each web address; GFM links one up to whitespace or `<`.
fn web_addresses(line: &str) -> Vec<std::ops::Range<usize>> {
    let lower = line.to_ascii_lowercase();
    ["https://", "http://", "www."]
        .iter()
        .flat_map(|prefix| lower.match_indices(prefix).map(|(at, prefix)| at + prefix.len()))
        .map(|host| {
            let end = line[host..]
                .find(|c: char| c.is_whitespace() || "<\"'`)".contains(c))
                .map_or(line.len(), |length| host + length);
            let path = line[host..end].find(['/', '?', '#']).map_or(end, |length| host + length);
            path..end
        })
        .collect()
}

fn starts_web_address(text: &str) -> bool {
    let start = text.get(..8).unwrap_or(text).to_ascii_lowercase();
    start.starts_with("http://") || start.starts_with("https://")
}

/// GitHub draws a fenced block whose info string, references decoded, names a diagram or map.
fn disarm_info_strings(line: &mut String) {
    let lower = line.to_ascii_lowercase();
    let Some(fence) = ["```", "~~~"].iter().filter_map(|run| lower.find(run)).min() else {
        return;
    };
    let mut spaces: Vec<usize> =
        lower[fence..].match_indices('&').map(|(at, _)| fence + at + 1).collect();
    for diagram in DIAGRAMS {
        spaces.extend(lower.match_indices(diagram).map(|(at, _)| at).filter(|&at| {
            let before = lower[..at].trim_end_matches([' ', '\t']);
            before.ends_with("```") || before.ends_with("~~~")
        }));
    }
    spaces.sort_unstable();
    spaces.dedup();
    for at in spaces.into_iter().rev() {
        line.insert(at, ZERO_WIDTH_SPACE);
    }
}

fn at_signs(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        plain.push_str(&rest[..at]);
        let reference = &rest[at..];
        match at_reference(reference) {
            Some(length) => {
                plain.push('@');
                rest = &reference[length..];
            }
            None => {
                plain.push('&');
                rest = &reference[1..];
            }
        }
    }
    plain.push_str(rest);
    plain
}

/// HTML reads `&commat;`, and `&#64` or `&#x40` with any leading zeros and an optional `;`, as `@`.
fn at_reference(text: &str) -> Option<usize> {
    if text.starts_with("&commat;") {
        return Some("&commat;".len());
    }
    let number = text.strip_prefix("&#")?;
    let (radix, digits) = match number.strip_prefix(['x', 'X']) {
        Some(hex) => (16, hex),
        None => (10, number),
    };
    let length = digits.find(|c: char| !c.is_digit(radix)).unwrap_or(digits.len());
    let value = u32::from_str_radix(&digits[..length], radix).ok()?;
    let end = text.len() - digits.len() + length;
    (value == u32::from('@')).then(|| end + usize::from(text[end..].starts_with(';')))
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

    fn worktree_with(content: &[u8]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".codescope")).unwrap();
        std::fs::write(dir.path().join(FINDINGS_FILE), content).unwrap();
        dir
    }

    #[test]
    fn reads_the_findings_with_plain_line_ends_and_no_byte_order_mark() {
        let written =
            "\u{feff}## Cause\r\n\r\nX.\u{2028}\u{feff}/Z.\r\n\r\n## Approach\rY.\u{2029}\r\n";
        let dir = worktree_with(written.as_bytes());
        let file = read_findings(dir.path()).unwrap();
        assert_eq!(file.stamp.1, written.len() as u64);
        let (text, findings) = file.check().unwrap();
        assert_eq!(text, "## Cause\n\nX.\n/Z.\n\n## Approach\nY.\n\n");
        assert_eq!(findings.outcome(), Outcome::Findings);
    }

    #[test]
    fn no_file_is_no_findings_and_an_odd_one_is_a_problem() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_findings(dir.path()).is_none());
        std::fs::create_dir_all(dir.path().join(FINDINGS_FILE)).unwrap();
        let folder = read_findings(dir.path()).unwrap();
        assert_eq!(folder.check().unwrap_err(), vec![Problem::NotAFile]);

        let utf16 = worktree_with(&[0xff, 0xfe, b'#', 0, b'#', 0]);
        let utf16 = read_findings(utf16.path()).unwrap();
        assert_eq!(utf16.check().unwrap_err(), vec![Problem::NotText]);

        let huge = worktree_with(&vec![b'y'; READ_LIMIT + 1]);
        let huge = read_findings(huge.path()).unwrap();
        assert_eq!(huge.check().unwrap_err(), vec![Problem::TooLarge(READ_LIMIT as u64 + 1)]);
    }

    #[test]
    fn the_length_limit_counts_characters_as_posted_not_bytes_read() {
        let base = "## Cause\n\nX.\n\n## Approach\n\n";
        let arrows = format!("{base}{}\n", "\u{2192}".repeat(MAX_LENGTH / 2));
        assert!(arrows.len() > MAX_LENGTH);
        let dir = worktree_with(arrows.as_bytes());
        assert!(read_findings(dir.path()).unwrap().check().is_ok());
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
    fn no_raw_html_reaches_the_comment() {
        let z = '\u{200b}';
        let html = "<!-- note\n<details open>\n</details><img src=x>\n<?x <![CDATA[ <IMG\tsrc=x>";
        assert_eq!(
            quiet(html),
            format!(
                "<{z}!-- note\n<{z}details open>\n<{z}/{z}details><{z}img src=x>\n\
                 <{z}?x <{z}!{z}[CDATA[ <{z}IMG\tsrc=x>"
            )
        );
        assert_eq!(quiet("a < b, a<=b, Vec<T>"), format!("a < b, a<=b, Vec<{z}T>"));
        assert_eq!(quiet("<https://example.com/a>"), "<https://example.com/a>");
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        for open in ["<!-- note", "<details>\n\nZ.", "<script>"] {
            assert!(check_findings(&format!("{base}{open}\n")).is_ok(), "{open}");
        }
    }

    #[test]
    fn no_image_or_diagram_reaches_the_comment() {
        let z = '\u{200b}';
        assert_eq!(quiet("![x](https://e.example/?d)"), format!("!{z}[x](https://e.example/?d)"));
        assert_eq!(quiet("![a `]` b](u) and ![x][r]"), format!("!{z}[a `]` b](u) and !{z}[x][r]"));
        assert_eq!(quiet("rows![0][1] and vec![1]"), format!("rows!{z}[0][1] and vec!{z}[1]"));
        assert_eq!(quiet("```mermaid\n~~~ Mermaid"), format!("```{z}mermaid\n~~~ {z}Mermaid"));
        assert_eq!(
            quiet("```geojson\n~~~TopoJSON\n``` stl"),
            format!("```{z}geojson\n~~~{z}TopoJSON\n``` {z}stl")
        );
        assert_eq!(
            quiet("```&#109;ermaid\n~~~ m&#x65;rmaid\n```&#32;mermaid"),
            format!("```&{z}#109;ermaid\n~~~ m&{z}#x65;rmaid\n```&{z}#32;mermaid")
        );
        assert_eq!(quiet("R&D: a &amp; b"), "R&D: a &amp; b");
        assert_eq!(quiet("```rust\nlet a = b!(c);"), "```rust\nlet a = b!(c);");
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        for image in ["![x](u)", "![x]\n\n[x]: u", "rows![0][1]", "<img src=x>"] {
            assert!(check_findings(&format!("{base}{image}\n")).is_ok(), "{image}");
        }
    }

    #[test]
    fn an_at_sign_in_a_web_address_path_is_escaped_so_the_link_still_works() {
        let z = '\u{200b}';
        assert_eq!(
            quiet("[types](https://www.npmjs.com/package/@types/node) and see @octocat"),
            format!("[types](https://www.npmjs.com/package/%40types/node) and see @{z}octocat")
        );
        assert_eq!(
            quiet("HTTPS://cdn.example/npm/react@18 www.example.com/@x <https://e.example/@y>"),
            "HTTPS://cdn.example/npm/react%4018 www.example.com/%40x <https://e.example/%40y>"
        );
        assert_eq!(quiet("https://@octocat"), format!("https://@{z}octocat"));
        assert_eq!(
            quiet("git clone https://deploy@git.example.com/r.git"),
            format!("git clone https://deploy@{z}git.example.com/r.git")
        );
        assert_eq!(
            quiet(r#"{"url":"https://api.example/a","owner":"@octocat"}"#),
            format!(r#"{{"url":"https://api.example/a","owner":"@{z}octocat"}}"#)
        );
    }

    #[test]
    fn a_fence_that_closes_less_indented_than_it_opens_is_refused() {
        let base = "## Cause\n\nX.\n\n## Approach\n\n";
        let listed = format!("{base}1. Run it:\n   ```\n   cargo test\n```\n");
        assert_eq!(check_findings(&listed).unwrap_err(), vec![Problem::UnevenFence]);
        let aligned = format!("{base}1. Run it:\n   ```\n   cargo test\n   ```\n");
        assert!(check_findings(&aligned).is_ok());
        let deeper = format!("{base}```\ncargo test\n  ```\n");
        assert!(check_findings(&deeper).is_ok());
    }

    #[test]
    fn a_slash_that_starts_a_word_starts_no_command_and_paths_stay_as_they_are() {
        let z = '\u{200b}';
        assert_eq!(
            quiet("run /deploy, `/deploy`, (/deploy) or \"/deploy\""),
            format!("run /{z}deploy, `/{z}deploy`, (/{z}deploy) or \"/{z}deploy\"")
        );
        let paths = "src/app.rs ./x ../y ~/z C:/w https://e.example/a [docs](/docs/a.md) a/b";
        assert_eq!(quiet(paths), paths);
        assert_eq!(quiet("// a comment and 1 / 2"), "// a comment and 1 / 2");
    }

    #[test]
    fn what_is_checked_is_what_is_posted() {
        let z = '\u{200b}';
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        let bell_fence = format!("{base}\u{7}```\n");
        assert_eq!(check_findings(&bell_fence).unwrap_err(), vec![Problem::OpenFence]);
        let bell_point = format!("{base}#\u{7}## B1 Which?\n");
        assert_eq!(check_findings(&bell_point).unwrap_err(), vec![Problem::StrayDecisionPoint]);
        let indented = format!("\n    ```\n{base}");
        let findings = check_findings(&indented).unwrap();
        assert!(comment_body(&indented, &findings).starts_with("    ```\n## Cause"));
        let split = format!("{base}Ask &#\u{1}64;x and &com\u{7f}mat;y.\n");
        let findings = check_findings(&split).unwrap();
        assert!(comment_body(&split, &findings).contains(&format!("Ask @{z}x and @{z}y.")));
    }

    #[test]
    fn the_comment_drops_control_characters_and_outer_blank_lines() {
        let text = "\n\n## Cause\n\nX\u{7}\u{1b}[31m.\tTab.\n\n## Approach\n\nY.\n\n";
        let findings = check_findings(text).unwrap();
        let body = comment_body(text, &findings);
        assert!(body.starts_with("## Cause\n\nX[31m.\tTab.\n"), "{body:?}");
        assert!(body.contains("Y.\n\n**Next step:**"), "{body:?}");
    }

    #[test]
    fn what_is_posted_must_fit_in_a_comment() {
        let base = "## Cause\n\nX.\n\n## Approach\n\n";
        let at_signs = format!("{base}{}\n", "@".repeat(MAX_LENGTH / 2));
        assert!(at_signs.len() < MAX_LENGTH);
        assert!(matches!(
            check_findings(&at_signs).unwrap_err()[..],
            [Problem::TooLong(length)] if length > MAX_LENGTH
        ));
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
    fn a_point_id_may_end_in_punctuation_and_every_point_needs_its_heading() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n## Decision points\n\n";
        for heading in ["### B1: Q?", "### B1. Q?", "### B1) Q?"] {
            let text = format!("{base}{heading}\n- **A** a\n- **B** b\nAdvice: A\n");
            let findings = check_findings(&text).unwrap();
            assert_eq!(findings.decision_points[0].id, "B1", "{heading}");
            assert_eq!(findings.decision_points[0].question, "Q?", "{heading}");
        }
        for unheaded in ["#### B1 Q?", "**B1** Q?", "B1. Q?"] {
            let text = format!("{base}{unheaded}\n- **A** a\n- **B** b\nAdvice: A\n");
            assert_eq!(
                check_findings(&text).unwrap_err(),
                vec![Problem::UnheadedDecisionPoints],
                "{unheaded}"
            );
        }
    }

    #[test]
    fn advice_names_a_letter_on_its_own() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n## Decision points\n\n### B1 Q?\n\
                    - **A** a\n- **B** b\n";
        for advice in ["Advice: Both are viable", "Advice: Allow them", "Advice: A better one is B"]
        {
            assert_eq!(
                check_findings(&format!("{base}{advice}\n")).unwrap_err(),
                vec![Problem::NoAdvice("B1".into())],
                "{advice}"
            );
        }
        let named = [
            "Advice: B",
            "Advice: B, simpler",
            "Advice: B.",
            "**Advice:** B) simpler",
            "Advice: B (simpler)",
            "Advice: B \u{2014} simpler",
        ];
        for advice in named {
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
    fn the_comment_mentions_nobody_and_starts_no_command_in_text_or_code() {
        let text = "## Cause\n\nAsk @octocat, _@org/team_ and \u{8c22}@octocat; mail a@b.example \
                    &#64;x &#x40;y &commat;z @&#111;ctocat.\n\n## Approach\n\n\
                    \u{20}\u{20}/deploy production\n// a comment\n\n```\n@claude run\n/close\n\
                    ```\n";
        let findings = check_findings(text).unwrap();
        let body = comment_body(text, &findings);
        let z = '\u{200b}';
        let mentions = format!(
            "Ask @{z}octocat, _@{z}org/team_ and \u{8c22}@{z}octocat; mail a@{z}b.example \
             @{z}x @{z}y @{z}z @{z}&#111;ctocat."
        );
        assert!(body.contains(&mentions), "{body}");
        assert!(body.contains(&format!("\n  /{z}deploy production\n// a comment\n")), "{body}");
        assert!(body.contains(&format!("\n@{z}claude run\n/{z}close\n")), "{body}");
        assert_eq!(
            quiet("&#0064a &#X040;b &#65;c &amp;#64;d &commat e"),
            format!("@{z}a @{z}b &#65;c &amp;#64;d &commat e")
        );
    }

    #[test]
    fn headings_and_fences_are_read_the_way_github_reads_them() {
        let spaced = "  ## Cause\n\nX.\n\n##\tApproach ##\n\nY.\n";
        assert!(check_findings(spaced).is_ok());
        let indented = "## Cause\n\nX.\n\n    ```\n\n## Approach\n\nY.\n\n    ```\n";
        assert!(check_findings(indented).is_ok(), "four spaces make code, not a fence");
        let code = "## Cause\n\nX.\n\n    ## Approach\n\nY.\n";
        assert_eq!(check_findings(code).unwrap_err(), vec![Problem::Missing(APPROACH)]);
    }

    #[test]
    fn the_message_to_the_session_is_one_line() {
        let message =
            problems_message(&[Problem::Missing(APPROACH), Problem::NoAdvice("B2".into())]);
        assert_eq!(
            message,
            "CodeScope cannot post .codescope/findings.md yet: no ## Approach; B2 has no line \
             Advice: <letter>, <reason>. Fix the file and stop."
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
            Problem::UnheadedDecisionPoints,
            Problem::Numbering("B2".into()),
            Problem::TooFewOptions("B1".into()),
            Problem::NoAdvice("B1".into()),
            Problem::AdviceNotAnOption("B1".into()),
            Problem::Marker,
            Problem::TooLong(70_000),
            Problem::TooLarge(300_000),
            Problem::OpenFence,
            Problem::UnevenFence,
            Problem::Link,
            Problem::NotAFile,
            Problem::Unreadable,
            Problem::NotText,
        ];
        let message = problems_message(&all);
        let plain = |c: char| (c.is_ascii_graphic() || c == ' ') && !matches!(c, '\'' | '"' | '`');
        assert!(message.chars().all(plain), "{message}");
    }

    #[test]
    fn the_message_names_a_few_problems_and_counts_the_rest() {
        let problems: Vec<Problem> =
            (1..=7).map(|n| Problem::AdviceNotAnOption(format!("B{n}"))).collect();
        assert_eq!(
            problems_message(&problems),
            "CodeScope cannot post .codescope/findings.md yet: B1 advises a letter it has no \
             option for; B2 advises a letter it has no option for; B3 advises a letter it has no \
             option for; B4 advises a letter it has no option for; and 3 more. Fix the file and \
             stop."
        );
    }
}
