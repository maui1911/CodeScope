//! Research findings as the board protocol shapes them: checked before CodeScope posts them,
//! and turned into the comment that carries the marker.

use std::fmt;
use std::io::Read as _;
use std::path::Path;
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::dispatch::{FINDINGS_FILE, linked_part};

pub(super) const MAX_LENGTH: usize = 60_000;
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
    /// The text between the heading and the first option, joined into one paragraph.
    pub context: String,
    /// The lettered options in the order listed, `A` first.
    pub options: Vec<DecisionOption>,
    /// The letter of one of `options`.
    pub advice: char,
}

impl DecisionPoint {
    /// Whether one of its options has this letter.
    pub fn has_option(&self, letter: char) -> bool {
        self.options.iter().any(|o| o.letter == letter)
    }
}

/// One lettered option of a decision point.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionOption {
    /// `A`, `B`, …
    pub letter: char,
    /// The Markdown after the letter, and its indented lines below it.
    pub text: String,
}

impl DecisionOption {
    /// The first sentence of the first line, and the rest on one line, as plain text without
    /// emphasis or code marks.
    pub fn label_and_detail(&self) -> (String, String) {
        let (first, below) = self.text.split_once('\n').unwrap_or((&self.text, ""));
        let (label, rest) = split_first_sentence(first);
        let detail: Vec<String> = std::iter::once(rest)
            .chain(below.lines())
            .map(plain_text)
            .filter(|part| !part.is_empty())
            .collect();
        (plain_text(label), detail.join(" "))
    }
}

fn split_first_sentence(line: &str) -> (&str, &str) {
    let mut in_code = false;
    let mut chars = line.char_indices().peekable();
    while let Some((index, c)) = chars.next() {
        match c {
            '`' => in_code = !in_code,
            '.' if !in_code && chars.peek().is_some_and(|(_, next)| *next == ' ') => {
                return line.split_at(index + 1);
            }
            _ => {}
        }
    }
    (line, "")
}

/// Markdown without `**` and backticks, trimmed.
pub fn plain_text(markdown: &str) -> String {
    markdown.replace("**", "").replace('`', "").trim().to_string()
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
    /// A line in a code block indented less than its opening fence, which ends the block on
    /// GitHub when the code sits in a list item.
    UnevenFence,
    /// A fence on a list item's line after text, where GitHub may read the item as more text.
    ListFence,
    /// A fence four or more columns into a list, which GitHub may or may not take for one.
    DeepFence,
    /// A heading naming a decision point that GitHub may show but the check does not read: in a
    /// list, quote or footnote, four or more columns in, or underlined.
    HiddenPoint,
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
                write!(f, "a code block has a line indented less than its opening fence")
            }
            Problem::ListFence => write!(
                f,
                "a code block opens on the line of a list item that may read as text: put a \
                 blank line before that item"
            ),
            Problem::DeepFence => write!(
                f,
                "a code block in a list has a fence four or more columns in: keep its fences \
                 at most three columns in"
            ),
            Problem::HiddenPoint => write!(
                f,
                "a heading that names a decision point sits in a list, quote or footnote, is \
                 indented, or is text underlined with = or -: start ## Decision points and \
                 each ### B heading at the beginning of a line, and put a blank line above a rule"
            ),
            Problem::Link => write!(f, "it or its folder is a link"),
            Problem::NotAFile => write!(f, "it is not a plain file"),
            Problem::Unreadable => write!(f, "it cannot be read"),
            Problem::NotText => write!(f, "it is not UTF-8 text"),
        }
    }
}

/// Modified time and length, which tell one version of a file from the next.
pub type FileStamp = (Option<SystemTime>, u64);

/// A file an agent leaves for CodeScope to post, as one read found it.
#[derive(Debug)]
pub struct HandoverFile {
    /// Taken as it was read, so the session hears about each version once.
    pub stamp: FileStamp,
    pub(super) read: Result<String, Problem>,
}

impl HandoverFile {
    /// The text to post with its findings, or what to fix first.
    pub fn check(&self) -> Result<(&str, Findings), Vec<Problem>> {
        let text = self.read.as_ref().map_err(|problem| vec![problem.clone()])?;
        check_findings(text).map(|findings| (text.as_str(), findings))
    }
}

/// The findings in `worktree`, `None` without a file; a link, an odd file, too much or non-UTF-8
/// text reads as that problem. Line ends come back as `\n`, without byte order marks.
pub fn read_findings(worktree: &Path) -> Option<HandoverFile> {
    read_handover(worktree, FINDINGS_FILE)
}

pub(super) fn read_handover(worktree: &Path, relative: &str) -> Option<HandoverFile> {
    let path = worktree.join(relative);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => {
            return Some(HandoverFile {
                stamp: (None, 0),
                read: Err(Problem::Unreadable),
            });
        }
    };
    let read = if linked_part(worktree, relative).is_some() {
        Err(Problem::Link)
    } else if !metadata.is_file() {
        Err(Problem::NotAFile)
    } else {
        read_text(&path)
    };
    Some(HandoverFile {
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
    lines: Vec<Line<'a>>,
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
    let (lines, fence_problems) = read_lines(text);
    let top = sections(&lines, 2);
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

    if let Some(problem) = misplaced_point_heading(&lines) {
        problems.push(problem);
    }

    let mut decision_points = Vec::new();
    if let Some(index) = position(DECISION_POINTS) {
        if index + 1 != top.len() {
            problems.push(Problem::DecisionPointsNotLast);
        }
        let body = &top[index].lines;
        let points = sections(body, 3);
        for (number, point) in points.iter().enumerate() {
            match decision_point(point, number + 1) {
                Ok(point) => decision_points.push(point),
                Err(problem) => problems.push(problem),
            }
        }
        let names_point =
            |line: &Line<'_>| line.text.split(|c: char| !c.is_alphanumeric()).any(is_point_id);
        let unheaded = points.is_empty() && body.iter().any(names_point);
        if unheaded && !problems.contains(&Problem::UnheadedDecisionPoints) {
            problems.push(Problem::UnheadedDecisionPoints);
        }
    }

    problems.extend(fence_problems);

    if problems.is_empty() {
        Ok(Findings { decision_points })
    } else {
        Err(problems)
    }
}

/// GitHub stops nesting list items about a hundred deep, so the check follows far fewer.
const MAX_MARKERS: usize = 10;
const POINT_LETTERS: &str = "decisionpoint";

#[derive(Clone, Copy)]
struct Fence {
    mark: char,
    run: usize,
    column: usize,
    item_column: usize,
    listed: bool,
}

impl Fence {
    fn at(text: &str, column: usize, item_column: usize, listed: bool) -> Option<Fence> {
        let mark = text.chars().next().filter(|c| matches!(c, '`' | '~'))?;
        let info = text.trim_start_matches(mark);
        let run = text.len() - info.len();
        let fence = Fence { mark, run, column, item_column, listed };
        (run >= 3 && !(mark == '`' && info.contains('`'))).then_some(fence)
    }

    fn closer_at(&self, line: &str) -> Option<usize> {
        let rest = line.trim_start_matches([' ', '\t']);
        let after = rest.trim_start_matches(self.mark);
        let closes = rest.len() - after.len() >= self.run
            && after.chars().all(|c| matches!(c, ' ' | '\t'));
        closes.then(|| indent_width(line))
    }
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum LastLine {
    #[default]
    NoParagraph,
    Paragraph,
    Unknown,
}

#[derive(Clone, Copy)]
struct Line<'a> {
    text: &'a str,
    code: bool,
}

#[derive(Default)]
struct Fences {
    open: Option<Fence>,
    items: Vec<usize>,
    listed: bool,
    blank_before: bool,
    last: LastLine,
    less_indented: bool,
    deep: bool,
    unsure_item: bool,
    hidden_point: bool,
    text_names_point: bool,
    text_tail: String,
}

impl Fences {
    fn is_code(&mut self, line: &str) -> bool {
        let start = line.trim_start_matches([' ', '\t']);
        let indent = indent_width(line);
        if let Some(fence) = self.open {
            self.end_paragraph();
            self.less_indented |= !start.is_empty() && indent < fence.column;
            match fence.closer_at(line) {
                Some(at) if (fence.item_column..=fence.item_column + 3).contains(&at) => {
                    self.open = None;
                    self.last = LastLine::NoParagraph;
                }
                Some(at) => {
                    let unsure = fence.item_column + 3 < at && at <= fence.column + 3;
                    self.deep |= fence.listed && unsure;
                }
                None => {}
            }
            return true;
        }
        if start.is_empty() {
            self.last = LastLine::NoParagraph;
            self.blank_before = true;
            self.end_paragraph();
            return false;
        }
        let after_blank = std::mem::take(&mut self.blank_before);
        self.note_point(line, start);
        let markers = after_markers(line);
        let fenced = markers.map_or_else(
            || line.contains("```") || line.contains("~~~"),
            |(text, _)| Fence::at(text, 0, 0, false).is_some(),
        );
        let marker_line = indent <= 3 && item_content(start).is_some() && !is_thematic_break(start);
        self.deep |= indent > 3 && self.listed && fenced;
        self.open = if marker_line {
            self.item(start, indent, markers, fenced)
        } else {
            self.other(line, indent, after_blank)
        };
        self.open.is_some()
    }

    fn note_point(&mut self, line: &str, start: &str) {
        let behind = behind_containers(start);
        let read = is_heading(line);
        let shown = heading_title(behind).is_some_and(mentions_point);
        let underline = is_underline_in_container(start);
        self.hidden_point |= !read && shown || underline && self.text_names_point;
        if read || underline {
            self.end_paragraph();
            return;
        }
        let text = shown_text(behind);
        let joined = format!("{}{}", self.text_tail, letters(&text));
        self.text_names_point |= starts_with_point_id(&text) || joined.contains(POINT_LETTERS);
        let tail = joined.char_indices().rev().nth(POINT_LETTERS.len() - 2);
        self.text_tail = joined[tail.map_or(0, |(at, _)| at)..].to_string();
    }

    fn end_paragraph(&mut self) {
        self.text_names_point = false;
        self.text_tail.clear();
    }

    fn item(
        &mut self,
        start: &str,
        indent: usize,
        markers: Option<(&str, usize)>,
        fenced: bool,
    ) -> Option<Fence> {
        let beside = self.items.last().is_some_and(|&open| indent < open);
        self.listed = true;
        if !(self.last == LastLine::NoParagraph || beside || may_interrupt_text(start)) {
            self.unsure_item |= fenced;
            self.last = LastLine::Unknown;
            return None;
        }
        self.items.retain(|&open| open <= indent);
        let Some((text, column)) = markers else {
            self.deep |= fenced;
            self.last = LastLine::Unknown;
            return None;
        };
        if !text.is_empty() && !may_define_link(text) {
            self.items.push(column);
        }
        self.last = if text.is_empty() || is_heading(text) {
            LastLine::NoParagraph
        } else if is_prose(text) {
            LastLine::Paragraph
        } else {
            LastLine::Unknown
        };
        Fence::at(text, column, column, true)
    }

    fn other(&mut self, line: &str, indent: usize, after_blank: bool) -> Option<Fence> {
        let start = line.trim_start_matches([' ', '\t']);
        let fence = if indent <= 3 { Fence::at(start, indent, 0, self.listed) } else { None };
        let heading = is_heading(line);
        let continues = self.last == LastLine::Paragraph && is_prose(start);
        if !continues {
            self.items.retain(|&open| open <= indent);
        }
        if indent == 0 && (after_blank || fence.is_some() || heading) {
            self.listed = false;
        }
        let container = self.items.last().copied().unwrap_or(0);
        self.last = if continues {
            LastLine::Paragraph
        } else if heading {
            LastLine::NoParagraph
        } else if self.last == LastLine::NoParagraph && is_prose(start) && indent < container + 4 {
            LastLine::Paragraph
        } else {
            LastLine::Unknown
        };
        fence
    }

    fn problems(&self) -> Vec<Problem> {
        [
            (self.unsure_item, Problem::ListFence),
            (self.deep, Problem::DeepFence),
            (self.hidden_point, Problem::HiddenPoint),
            (self.less_indented, Problem::UnevenFence),
            (self.open.is_some(), Problem::OpenFence),
        ]
        .into_iter()
        .filter_map(|(found, problem)| found.then_some(problem))
        .collect()
    }
}

pub(super) fn fence_problems(text: &str) -> Vec<Problem> {
    let (_, problems) = read_lines(text);
    problems
        .into_iter()
        .filter(|problem| {
            matches!(
                problem,
                Problem::OpenFence | Problem::UnevenFence | Problem::ListFence | Problem::DeepFence
            )
        })
        .collect()
}

fn read_lines(text: &str) -> (Vec<Line<'_>>, Vec<Problem>) {
    let mut fences = Fences::default();
    let lines = text.lines().map(|text| Line { code: fences.is_code(text), text }).collect();
    (lines, fences.problems())
}

/// CommonMark lets a list start inside a paragraph only with a bullet or 1, and not empty.
fn may_interrupt_text(item: &str) -> bool {
    let digits = item.len() - item.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let filled = item_content(item).is_some_and(|c| !c.trim_matches([' ', '\t']).is_empty());
    (digits == 0 || &item[..digits] == "1") && filled
}

/// CommonMark starts an item's content one to four columns past its marker, else one past it.
fn after_markers(line: &str) -> Option<(&str, usize)> {
    let mut rest = line.trim_start_matches([' ', '\t']);
    let mut column = indent_width(line);
    for _ in 0..=MAX_MARKERS {
        let Some(content) = item_content(rest).filter(|_| !is_thematic_break(rest)) else {
            return Some((rest, column));
        };
        let marker_end = advance(column, &rest[..rest.len() - content.len()]);
        let text = content.trim_start_matches([' ', '\t']);
        let text_column = advance(marker_end, &content[..content.len() - text.len()]);
        if text.is_empty() || text_column - marker_end > 4 {
            return Some(("", marker_end + 1));
        }
        (rest, column) = (text, text_column);
    }
    None
}

/// cmark-gfm drops a paragraph of only link definitions, so its list item may end up empty.
fn may_define_link(text: &str) -> bool {
    let Some(label) = text.strip_prefix('[') else {
        return false;
    };
    let mut chars = label.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                chars.next();
            }
            '[' => return false,
            ']' => return chars.as_str().starts_with(':'),
            _ => {}
        }
    }
    true
}

fn is_prose(text: &str) -> bool {
    text.starts_with(char::is_alphanumeric) && !text.contains('|') && item_content(text).is_none()
}

/// CommonMark: three or more of one of `-`, `*` and `_`, with only spaces and tabs between.
fn is_thematic_break(text: &str) -> bool {
    let mut marks = text.chars().filter(|c| !matches!(c, ' ' | '\t'));
    let Some(mark) = marks.next().filter(|c| matches!(c, '-' | '*' | '_')) else {
        return false;
    };
    let mut count = 1;
    for c in marks {
        if c != mark {
            return false;
        }
        count += 1;
    }
    count >= 3
}

fn is_heading(text: &str) -> bool {
    heading_title(text).is_some()
}

fn heading_title(text: &str) -> Option<&str> {
    heading_with_level(text).map(|(_, title)| title)
}

fn heading_with_level(text: &str) -> Option<(usize, &str)> {
    (1..=6).find_map(|level| heading(text, level).map(|title| (level, title)))
}

/// CommonMark: a run of `=` or of `-` under a paragraph makes that paragraph a heading.
fn is_underline(text: &str) -> bool {
    let run = text.trim_end_matches([' ', '\t']);
    !run.is_empty() && (run.bytes().all(|b| b == b'=') || run.bytes().all(|b| b == b'-'))
}

fn mentions_point(text: &str) -> bool {
    let text = shown_text(text);
    starts_with_point_id(&text) || letters(&text).contains(POINT_LETTERS)
}

fn names_decision_points(text: &str) -> bool {
    letters(&shown_text(text)).contains(POINT_LETTERS)
}

fn starts_with_point_id(text: &str) -> bool {
    let mut words = text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty());
    words.next().is_some_and(is_point_id)
}

fn letters(text: &str) -> String {
    text.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// CommonMark shows no emphasis, code or escape marks or link targets, and decodes references.
fn shown_text(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(c) = rest.chars().next() {
        if let Some((decoded, length)) = character_reference(rest) {
            shown.push(decoded);
            rest = &rest[length..];
        } else if let Some(target) = rest.strip_prefix("](") {
            shown.push(' ');
            rest = target.find(')').map_or("", |end| &target[end + 1..]);
        } else {
            if !matches!(c, '*' | '_' | '~' | '`' | '\\') {
                shown.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    shown
}

/// HTML: `&#` and decimal digits, `&#x` and hex digits, or `&` and a name, then `;`.
fn character_reference(text: &str) -> Option<(char, usize)> {
    let body = text.strip_prefix('&')?;
    let (end, _) = body.char_indices().take(33).find(|&(_, c)| c == ';')?;
    let name = &body[..end];
    let decoded = match name.strip_prefix('#') {
        Some(number) => {
            let (digits, radix) = match number.strip_prefix(['x', 'X']) {
                Some(hex) => (hex, 16),
                None => (number, 10),
            };
            if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
                return None;
            }
            let code = u32::from_str_radix(digits, radix).unwrap_or(u32::MAX);
            char::from_u32(code).unwrap_or(char::REPLACEMENT_CHARACTER)
        }
        None if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric()) => ' ',
        None => return None,
    };
    Some((decoded, end + 2))
}

fn is_underline_in_container(line: &str) -> bool {
    let mut rest = line.trim_start_matches([' ', '\t']);
    loop {
        if is_underline(rest) {
            return true;
        }
        let Some(inner) = container_content(rest) else {
            return false;
        };
        rest = inner.trim_start_matches([' ', '\t']);
    }
}

fn behind_containers(line: &str) -> &str {
    let mut rest = line.trim_start_matches([' ', '\t']);
    while let Some(inner) = container_content(rest) {
        rest = inner.trim_start_matches([' ', '\t']);
    }
    rest
}

fn indent_width(line: &str) -> usize {
    advance(0, &line[..line.len() - line.trim_start_matches([' ', '\t']).len()])
}

fn advance(column: usize, text: &str) -> usize {
    text.chars().fold(column, |width, c| if c == '\t' { width + 4 - width % 4 } else { width + 1 })
}

fn sections<'a>(lines: &[Line<'a>], level: usize) -> Vec<Section<'a>> {
    let mut sections: Vec<Section> = Vec::new();
    for &line in lines {
        if !line.code && let Some(title) = heading(line.text, level) {
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

fn misplaced_point_heading(lines: &[Line<'_>]) -> Option<Problem> {
    let mut in_points = false;
    let mut found = None;
    for line in lines.iter().filter(|line| !line.code) {
        let Some((level, title)) = heading_with_level(line.text) else {
            continue;
        };
        let section = level == 2 && format!("## {title}").eq_ignore_ascii_case(DECISION_POINTS);
        if level == 2 {
            in_points = section;
        }
        let point = level == 3 && in_points && is_point_id(split_point_title(title).0);
        if section || point || !mentions_point(title) {
            continue;
        }
        if names_decision_points(title) {
            return Some(Problem::DecisionPointsHeading);
        }
        found = if in_points {
            found.or(Some(Problem::UnheadedDecisionPoints))
        } else {
            Some(Problem::StrayDecisionPoint)
        };
    }
    found
}

fn point_title<'a>(section: &'a Section) -> (&'a str, &'a str) {
    split_point_title(section.heading.trim_start_matches('#').trim())
}

fn split_point_title(title: &str) -> (&str, &str) {
    let (id, question) = title.split_once(char::is_whitespace).unwrap_or((title, ""));
    (id.trim_end_matches([':', '.', ')']), question.trim())
}

fn is_point_id(word: &str) -> bool {
    let mut chars = word.chars();
    matches!(chars.next(), Some('B' | 'b'))
        && !chars.as_str().is_empty()
        && chars.all(|c| c.is_ascii_digit())
}

fn is_blank(lines: &[Line<'_>]) -> bool {
    lines.iter().all(|line| line.text.trim().is_empty())
}

fn decision_point(section: &Section, number: usize) -> Result<DecisionPoint, Problem> {
    let expected = format!("B{number}");
    let (id, question) = point_title(section);
    if !id.eq_ignore_ascii_case(&expected) {
        return Err(Problem::Numbering(expected));
    }
    let mut context: Vec<&str> = Vec::new();
    let mut options: Vec<DecisionOption> = Vec::new();
    let mut in_option = false;
    for line in section.lines.iter().map(|line| line.text) {
        if let Some((letter, text)) = option_start(line) {
            options.push(DecisionOption {
                letter,
                text: text.to_string(),
            });
            in_option = true;
        } else if line.trim().is_empty() || advice_letter(line).is_some() {
            in_option = false;
        } else if in_option && line.starts_with(char::is_whitespace) {
            if let Some(option) = options.last_mut() {
                option.text = format!("{}\n{}", option.text, line.trim());
            }
        } else {
            in_option = false;
            if options.is_empty() {
                context.push(line.trim());
            }
        }
    }
    if options.len() < 2 {
        return Err(Problem::TooFewOptions(expected));
    }
    let advice = section
        .lines
        .iter()
        .find_map(|l| advice_letter(l.text))
        .ok_or_else(|| Problem::NoAdvice(expected.clone()))?;
    if !options.iter().any(|o| o.letter == advice) {
        return Err(Problem::AdviceNotAnOption(expected));
    }
    Ok(DecisionPoint {
        id: expected,
        question: question.to_string(),
        context: context.join(" "),
        options,
        advice,
    })
}

fn option_start(line: &str) -> Option<(char, &str)> {
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
    let text = rest.strip_prefix("**")?;
    let text = text.trim_start_matches(|c: char| c.is_whitespace() || matches!(c, ':' | '.'));
    Some((letter, text.trim_end()))
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

pub(super) fn normalised(text: &str) -> String {
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
pub(super) fn quiet(text: &str) -> String {
    let lines: Vec<String> = at_signs(text).lines().map(quiet_line).collect();
    lines.join("\n")
}

fn quiet_line(line: &str) -> String {
    let linked = at_signs_in_web_paths(line);
    let mut quiet = String::with_capacity(line.len() + 8);
    let mut chars = line.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        let next = chars.peek().map(|(_, next)| *next);
        match c {
            '@' if linked.binary_search(&at).is_ok() => {
                quiet.push_str("%40");
            }
            '@' => {
                quiet.push('@');
                quiet.push(ZERO_WIDTH_SPACE);
            }
            '/' if starts_command(line, at) => {
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
    disarm_info_strings(&quiet)
}

fn starts_command(line: &str, slash: usize) -> bool {
    let before = &line[..slash];
    let word = before.trim_end_matches(|c: char| "([{\"'`*".contains(c));
    let starts_word =
        !before.ends_with("](") && word.chars().next_back().is_none_or(char::is_whitespace);
    starts_word && line[slash + 1..].starts_with(|c: char| c.is_ascii_alphanumeric())
}

/// GFM links a web address up to whitespace or `<`, so an `@` in its path, query or fragment
/// is part of the link.
fn at_signs_in_web_paths(line: &str) -> Vec<usize> {
    let mut linked = Vec::new();
    let mut host = false;
    let mut path = false;
    for (at, c) in line.char_indices() {
        if c.is_whitespace() || "<\"'`)".contains(c) {
            host = false;
            path = false;
            continue;
        }
        host |= ["https://", "http://", "www."].iter().any(|prefix| {
            at.checked_sub(prefix.len()).is_some_and(|start| ascii_at(line, start, prefix))
        });
        path |= host && matches!(c, '/' | '?' | '#');
        if path && c == '@' {
            linked.push(at);
        }
    }
    linked
}

fn ascii_at(text: &str, at: usize, ascii: &str) -> bool {
    text.as_bytes()
        .get(at..at + ascii.len())
        .is_some_and(|bytes| bytes.eq_ignore_ascii_case(ascii.as_bytes()))
}

fn starts_web_address(text: &str) -> bool {
    ["http://", "https://"].iter().any(|scheme| ascii_at(text, 0, scheme))
}

/// GitHub draws a fenced block whose info string, references decoded, names a diagram or map.
fn disarm_info_strings(line: &str) -> String {
    let Some(info) = info_string_at(line) else {
        return line.to_string();
    };
    let rest = &line[info..];
    let word = rest.len() - rest.trim_start_matches([' ', '\t']).len();
    let diagram = DIAGRAMS.iter().any(|diagram| ascii_at(rest, word, diagram));
    let mut disarmed = String::with_capacity(line.len() + 8);
    disarmed.push_str(&line[..info]);
    for (at, c) in rest.char_indices() {
        if diagram && at == word {
            disarmed.push(ZERO_WIDTH_SPACE);
        }
        disarmed.push(c);
        if c == '&' {
            disarmed.push(ZERO_WIDTH_SPACE);
        }
    }
    disarmed
}

/// GFM opens a fence after any run of block quote, list item and footnote definition markers.
fn info_string_at(line: &str) -> Option<usize> {
    let rest = behind_containers(line);
    let mark = rest.chars().next().filter(|c| matches!(c, '`' | '~'))?;
    let info = rest.trim_start_matches(mark);
    let run = rest.len() - info.len();
    (run >= 3 && !(mark == '`' && info.contains('`'))).then(|| line.len() - info.len())
}

fn container_content(line: &str) -> Option<&str> {
    line.strip_prefix('>').or_else(|| footnote_content(line)).or_else(|| item_content(line))
}

fn footnote_content(line: &str) -> Option<&str> {
    let label = line.strip_prefix("[^")?;
    let end = label.find([']', ' ', '\t'])?;
    label[end..].strip_prefix("]:").filter(|_| end > 0)
}

fn item_content(line: &str) -> Option<&str> {
    let digits = line.len() - line.trim_start_matches(|c: char| c.is_ascii_digit()).len();
    let marker = match digits {
        0 => line.strip_prefix(['-', '*', '+']),
        1..=9 => line[digits..].strip_prefix(['.', ')']),
        _ => None,
    };
    marker.filter(|content| content.is_empty() || content.starts_with([' ', '\t']))
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

/// The findings in a comment [`comment_body`] made, read back without its next step, its marker
/// and the zero-width spaces `quiet` put in.
pub fn posted_findings(body: &str) -> Option<Findings> {
    let lines: Vec<&str> = body.lines().collect();
    let marker =
        lines.iter().rposition(|line| is_marker_line(line) && line.contains("phase=research"))?;
    let end = lines[..marker]
        .iter()
        .rposition(|line| line.starts_with("**Next step:**"))
        .unwrap_or(marker);
    check_findings(&lines[..end].join("\n").replace(ZERO_WIDTH_SPACE, "")).ok()
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

    fn option(letter: char, text: &str) -> DecisionOption {
        DecisionOption {
            letter,
            text: text.into(),
        }
    }

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
                    context: "Context.".into(),
                    options: vec![
                        option('A', "Stack it."),
                        option('B', "Wait."),
                        option('C', "Fold it in."),
                    ],
                    advice: 'A',
                },
                DecisionPoint {
                    id: "B2".into(),
                    question: "Extra sections?".into(),
                    context: String::new(),
                    options: vec![option('A', "Allow them."), option('B', "Reject them.")],
                    advice: 'B',
                },
            ]
        );
    }

    #[test]
    fn an_option_takes_its_indented_lines_and_splits_into_label_and_detail() {
        let text = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n## Decision points\n\n\
                    ### B1 Which session?\n\nThe user can open a shell.\n\
                    It has no id.\n\n\
                    - **A** Prefer agent sessions, in this order:\n\
                    \x20 1. an open tab with an `agent_id`;\n\
                    \x20 2. else a fresh one.\n\
                    - **B**: Use `cfg.first()`. It is what the sidebar does.\n\n\
                    Advice: A. It needs no schema change.\n";
        let point = &check_findings(text).unwrap().decision_points[0];
        assert_eq!(point.context, "The user can open a shell. It has no id.");
        assert_eq!(
            point.options,
            vec![
                option(
                    'A',
                    "Prefer agent sessions, in this order:\n1. an open tab with an `agent_id`;\n\
                     2. else a fresh one."
                ),
                option('B', "Use `cfg.first()`. It is what the sidebar does."),
            ]
        );
        assert_eq!(
            point.options[0].label_and_detail(),
            (
                "Prefer agent sessions, in this order:".to_string(),
                "1. an open tab with an agent_id; 2. else a fresh one.".to_string()
            )
        );
        assert_eq!(
            point.options[1].label_and_detail(),
            ("Use cfg.first().".to_string(), "It is what the sidebar does.".to_string())
        );
    }

    #[test]
    fn reads_the_findings_back_from_the_comment_that_posted_them() {
        let findings = check_findings(WITH_POINTS).unwrap();
        let body = comment_body(WITH_POINTS, &findings);
        assert_eq!(posted_findings(&body), Some(findings));
        assert_eq!(posted_findings(WITH_POINTS), None);
        assert_eq!(posted_findings("B1 A, B2 B"), None);

        let quieted = WITH_POINTS.replace("Stack it.", "Ask @maui1911 to run /deploy <now>.");
        let findings = check_findings(&quieted).unwrap();
        let body = comment_body(&quieted, &findings);
        assert!(body.contains(ZERO_WIDTH_SPACE));
        assert_eq!(posted_findings(&body), Some(findings));
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
                "<{z}!-- note\n<{z}details open>\n<{z}/details><{z}img src=x>\n\
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
        assert_eq!(
            quiet("> - ```mermaid\n1) ~~~ m&#x65;rmaid\n- > ``` &#109;ermaid"),
            format!("> - ```{z}mermaid\n1) ~~~ m&{z}#x65;rmaid\n- > ``` &{z}#109;ermaid")
        );
        assert_eq!(
            quiet("[^d]: ```mermaid\n> [^e]: ~~~ m&#x65;rmaid"),
            format!("[^d]: ```{z}mermaid\n> [^e]: ~~~ m&{z}#x65;rmaid")
        );
        for code in ["Run ```npm ci && npm test``` first", "echo '```' >> n.md && git add n.md"] {
            assert_eq!(quiet(code), code, "no fence opens on this line");
        }
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
    fn a_long_line_is_quieted_in_one_pass() {
        let z = '\u{200b}';
        let started = std::time::Instant::now();
        let fence = format!("```{}", "&".repeat(READ_LIMIT));
        assert_eq!(quiet(&fence), format!("```{}", format!("&{z}").repeat(READ_LIMIT)));
        let hosts = "www.".repeat(READ_LIMIT / 4);
        assert_eq!(quiet(&hosts), hosts);
        let paths = "www.x/@".repeat(READ_LIMIT / 7);
        assert_eq!(quiet(&paths), "www.x/%40".repeat(READ_LIMIT / 7));
        let tags = "<a\u{e9}\u{e9}\u{e9}\u{e9}".repeat(READ_LIMIT / 10);
        assert_eq!(quiet(&tags), format!("<{z}a\u{e9}\u{e9}\u{e9}\u{e9}").repeat(READ_LIMIT / 10));
        let elapsed = started.elapsed();
        assert!(elapsed < std::time::Duration::from_secs(5), "took {elapsed:?}");
    }

    #[test]
    fn a_fence_that_closes_less_indented_than_it_opens_is_refused() {
        let base = "## Cause\n\nX.\n\n## Approach\n\n";
        let listed = format!("{base}1. Run it:\n   ```\n   cargo test\n```\n");
        assert_eq!(check_findings(&listed).unwrap_err(), vec![Problem::UnevenFence]);
        let ended = format!("{base}1. Run it:\n   ```\n   cargo test\nmore\n   ```\n");
        assert_eq!(check_findings(&ended).unwrap_err(), vec![Problem::UnevenFence]);
        let aligned = format!("{base}1. Run it:\n   ```\n   cargo test\n\n\tdeeper\n   ```\n");
        assert!(check_findings(&aligned).is_ok());
        let deeper = format!("{base}```\ncargo test\n  ```\n");
        assert!(check_findings(&deeper).is_ok());
    }

    #[test]
    fn a_closing_fence_may_be_followed_only_by_spaces_and_tabs() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        let spaced = format!("{base}```\ncode\n``` \t\n");
        assert!(check_findings(&spaced).is_ok());
        let no_break = format!("{base}```\ncode\n```\u{a0}\n\nZ.\n");
        assert_eq!(check_findings(&no_break).unwrap_err(), vec![Problem::OpenFence]);
    }

    #[test]
    fn a_slash_that_starts_a_word_is_broken_and_one_inside_a_word_is_not() {
        let z = '\u{200b}';
        assert_eq!(
            quiet("run /deploy, `/deploy`, (/deploy), \"/deploy\" or **/deploy**, then /x-y."),
            format!(
                "run /{z}deploy, `/{z}deploy`, (/{z}deploy), \"/{z}deploy\" or **/{z}deploy**, \
                 then /{z}x-y."
            )
        );
        assert_eq!(
            quiet("see /claude.md, cat /etc/hosts\n/deploy/\n  /close/x\n\u{a0}/deploy.prod"),
            format!(
                "see /{z}claude.md, cat /{z}etc/hosts\n/{z}deploy/\n  /{z}close/x\n\
                 \u{a0}/{z}deploy.prod"
            ),
            "a bot may take a command after any whitespace, whatever follows its name"
        );
        let paths = "src/app.rs ./x ../y ~/z C:/w https://e.example/a [docs](/docs/a.md) a/b \
                     src/**/mod.rs docs/caf\u{e9}/menu.md https://e.example/wiki/Foo_(bar)/baz \
                     $(pwd)/x packages/*/src src/**/tests \"$ROOT\"/build '$HOME'/bin";
        assert_eq!(quiet(paths), paths);
        assert_eq!(quiet("[d]: /docs/a.md"), format!("[d]: /{z}docs/a.md"));
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
    fn a_fence_may_open_on_the_line_of_its_list_item() {
        let base = "## Cause\n\nX.\n\n## Approach\n\n";
        let items = format!("{base}- ```sh\n  a\n  ```\n- ```sh\n  b\n  ```\n");
        assert!(check_findings(&items).is_ok());
        let numbered = format!("{base}1. ```sh\n   a\n   ```\n10. ```\n    b\n       ```\n");
        assert!(check_findings(&numbered).is_ok());
        let hidden = format!("{base}- ```md\n  ## Decision points\n  ```\n");
        assert!(check_findings(&hidden).is_ok(), "the heading is code in the item");
        let outside = format!("{base}- ```sh\n  a\n```\n");
        assert_eq!(
            check_findings(&outside).unwrap_err(),
            vec![Problem::UnevenFence, Problem::OpenFence],
            "the closing fence ends the item and opens a block that runs to the end"
        );
        let no_item = format!("{base}Steps:\n2. ```sh\n   a\n   ```\n");
        assert_eq!(
            check_findings(&no_item).unwrap_err(),
            vec![Problem::ListFence, Problem::OpenFence],
            "a list numbered from 2 can't interrupt a paragraph, so its last line opens a block"
        );
        for empty in ["* ", "1. "] {
            let text = format!("{base}Steps\n{empty}\n2. ```\n   ~~~\n   ```\n");
            assert_eq!(
                check_findings(&text).unwrap_err(),
                vec![Problem::ListFence, Problem::OpenFence],
                "an empty item can't interrupt a paragraph either: {empty:?}"
            );
        }
    }

    #[test]
    fn a_fence_on_an_item_s_line_is_refused_where_github_may_read_the_item_as_text() {
        let base = "## Cause\n\nX.\n\n## Approach\n\n";
        let step = "2. ```sh\n   a\n   ```\n";
        for before in [
            "Then run:\n    # build first\n",
            "1. Run the build:\n\n       make all\nThen test it\n",
            "- Build:\n\n      make all\nThen test\n",
            "- ---\n  Steps\n",
            "* * *\n    code\nlazy\n",
            "1. \n\n   foo\n",
            "- a\n===\n",
            "> note\n",
            "| a |\n|---|\n",
        ] {
            let text = format!("{base}{before}{step}");
            assert_eq!(check_findings(&text).unwrap_err()[0], Problem::ListFence, "{before:?}");
        }
        let hidden = "- a\n===\n2. ```\n   x\n   ```\n   ## Decision points\n   ```\n";
        let hidden = format!("{base}{hidden}");
        assert_eq!(check_findings(&hidden).unwrap_err(), vec![Problem::ListFence], "a lazy ===");
        let blank = format!("{base}> note\n\n{step}");
        assert!(check_findings(&blank).is_ok(), "after a blank line it is surely an item");
    }

    #[test]
    fn a_fence_four_or_more_columns_into_a_list_is_refused() {
        let base = "## Cause\n\nX.\n\n## Approach\n\n";
        for deep in [
            "-   a\n    ```\n    code\n    ```\n",
            "1. Build:\n   - debug:\n     ```sh\n     x\n     ```\n",
            "10. Run:\n    ```sh\n    x\n    ```\n",
            "1. a\n   - ```sh\n     x\n     ```\n     - ```sh\n       y\n       ```\n",
            "- a\n  ```\n     ```\n  ## B9\n  ```\n",
        ] {
            let text = format!("{base}{deep}");
            assert_eq!(check_findings(&text).unwrap_err(), vec![Problem::DeepFence], "{deep:?}");
        }
        for code in ["Y.\n\n    ```\n", "- a\n\n## Notes\n\n    ```\n", "- a\n\nB.\n\n    ```\n"] {
            let text = format!("{base}{code}");
            assert!(check_findings(&text).is_ok(), "no list holds it, so it is code: {code:?}");
        }
    }

    #[test]
    fn a_later_step_may_open_its_fence_on_its_line_after_any_text_of_the_list() {
        let base = "## Cause\n\nX.\n\n## Approach\n\n";
        let step = "2. ```sh\n   make test\n   ```\n";
        for before in [
            "1. ```sh\n   make\n   ```\n   This takes a minute.\n",
            "1. Stop the service\n   and wait for it to drain.\n",
            "1. Stop the service\nand wait for it to drain.\n",
            "1. Build:\n    - debug\n",
            "1. Add the flag:\n   - in settings\n   - in the CLI\n\n   Restart afterwards.\n",
        ] {
            let text = format!("{base}{before}{step}");
            assert!(check_findings(&text).is_ok(), "{before:?}");
        }
        let heading = format!("## Cause\n\nX.\n\n## Approach\n{step}");
        assert!(check_findings(&heading).is_ok(), "a heading is no paragraph");
    }

    #[test]
    fn a_marker_with_nothing_after_it_opens_an_item_and_starts_no_prose() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        let step = "2. ```\n   ```\n";
        for before in [
            "- 1.\nx\n",
            "- a\n1.\nx\n",
            "10. - a\n    1. b\n\n      x\n\n        y\nz\n",
        ] {
            let text = format!("{base}{before}{step}");
            assert_eq!(check_findings(&text).unwrap_err()[0], Problem::ListFence, "{before:?}");
        }
        let points = "-\n     ```\n  ```\n  ## Decision points\n\n  ### B1 Pick?\n  ```\n";
        for deep in [points, "1.\n      ```\n   ## Approach\n"] {
            let text = format!("{base}{deep}");
            assert!(check_findings(&text).unwrap_err().contains(&Problem::DeepFence), "{deep:?}");
        }
    }

    #[test]
    fn a_line_of_many_nested_markers_is_not_followed_deeper_than_github_nests() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        let nested = format!("{}a\n\n{}x\ny\n2. ```\n   ```\n", "- ".repeat(120), " ".repeat(240));
        assert_eq!(check_findings(&format!("{base}{nested}")).unwrap_err()[0], Problem::ListFence);
        let fenced = format!("{base}{}```\n", "- ".repeat(20));
        assert_eq!(check_findings(&fenced).unwrap_err(), vec![Problem::DeepFence]);
        assert!(check_findings(&format!("{base}- 1. - a\n\n      b\n")).is_ok());
    }

    #[test]
    fn a_rule_in_a_list_item_is_its_content_not_more_items() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        let step = "2. ```\n   ```\n";
        for rule in ["- * * *", "+ - - -"] {
            let text = format!("{base}{rule}\n\n      x\ny\n{step}");
            assert_eq!(check_findings(&text).unwrap_err()[0], Problem::ListFence, "{rule:?}");
        }
        let kept = format!("{base}- * * *\n  x\n2. ```sh\n   a\n   ```\n");
        assert!(check_findings(&kept).is_ok(), "the next step is beside the item");
    }

    #[test]
    fn a_decision_point_github_would_show_where_the_check_reads_none_is_refused() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        for hidden in [
            "1.  Do it.\n\n    ## Decision points\n\n    ### B1 Which?\n",
            "- ## Decision points\n",
            "- a\n\n    - ### B1 Which?\n",
            "- a\n\n    1. b\n    \t# B2 c\n",
            "1. The issue asks:\n   > ## Decision points\n   > ### B1 Stack or wait?\n",
            "> ## Decision points\n",
            "- > ## Decision points\n",
            "Y [^1].\n\n[^1]: x\n    ### B1 Q?\n",
            "Y [^1].\n\n[^1]: ## Decision points\n",
            "Decision points\n---\n",
            "- a\n\n  B1 Which?\n  ---\n",
            "- Decision points\n  -\n",
            "> Decision points\n> -\n",
            ">> Decision points\n>> -\n",
            "> B1 Which?\n> -\n",
            "Some text about decision points\n===\n",
        ] {
            let text = format!("{base}{hidden}");
            let problems = check_findings(&text).unwrap_err();
            assert_eq!(problems, vec![Problem::HiddenPoint], "{hidden:?}");
        }
        for shown in [
            "Y.\n\n    # a comment in code\n",
            "1. Run:\n   ```sh\n   # build\n   ```\n",
            "- a\n\nB.\n\n    # code again\n",
            "1. Run:\n\n       # build\n       make\n",
            "- ## Setup\n",
            "> ## Steps to reproduce\n",
            "Text\n---\n",
            "Decision points are below.\n\n---\n",
        ] {
            assert!(check_findings(&format!("{base}{shown}")).is_ok(), "{shown:?}");
        }
    }

    #[test]
    fn an_item_that_may_hold_only_a_link_definition_is_not_taken_for_open() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        let rest = "\n\n\n  foo\n2. ```sh\n   x\n   ```\n";
        let definitions = ["- [protocol]: docs/board-protocol.md", "- [a long\n  label]: /u"];
        let escaped = ["- [a\\]\n  b]: /u", "- [a\\] c\n  b]: /u", "- [^1]: x"];
        for item in definitions.into_iter().chain(escaped) {
            let text = format!("{base}{item}{rest}");
            assert_eq!(check_findings(&text).unwrap_err()[0], Problem::ListFence, "{item:?}");
        }
        let linked = format!("{base}- [docs](/docs/a.md) explain it{rest}");
        assert!(check_findings(&linked).is_ok());
    }

    #[test]
    fn decision_points_are_named_as_github_shows_them() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        for spelled in ["## Decision  points\n\nX.\n", "## Decision\tpoints\n\nX.\n"] {
            let problems = check_findings(&format!("{base}{spelled}")).unwrap_err();
            assert_eq!(problems, vec![Problem::DecisionPointsHeading], "{spelled:?}");
        }
        for hidden in [
            "> ## *Decision* points\n",
            "**Decision**  points\n---\n",
            "- Decision-points\n  ===\n",
            "> ### **B1** Which?\n",
            "> ## Deci*sion* points\n",
            "> ## Decision&#32;points\n",
            "> ## Decision&nbsp;points\n",
            "> ## [Decision](#x) points\n",
            "> ### &#66;1 Which?\n",
            "Decision\npoints\n---\n",
            "- a\n\n  Open decision\n  points for you\n  ===\n",
        ] {
            let problems = check_findings(&format!("{base}{hidden}")).unwrap_err();
            assert_eq!(problems, vec![Problem::HiddenPoint], "{hidden:?}");
        }
        for other in ["> ## Decisions and their points\n", "Decision\n\npoints\n---\n"] {
            assert!(check_findings(&format!("{base}{other}")).is_ok(), "{other:?}");
        }
    }

    #[test]
    fn every_heading_that_names_a_decision_point_is_one_the_check_reads() {
        let base = "## Cause\n\nX.\n\n## Approach\n\nY.\n\n";
        let options = "- **A** a\n- **B** b\nAdvice: A\n";
        for (heading, problem) in [
            ("# Decision points\n\n#### B1 Stack or wait?\n", Problem::DecisionPointsHeading),
            ("### Decision points\n", Problem::DecisionPointsHeading),
            ("## Decision&#32;points\n\n### &#66;1 Which?\n", Problem::DecisionPointsHeading),
            ("## B1 Stack or wait?\n", Problem::StrayDecisionPoint),
            ("### **B1** Which?\n", Problem::StrayDecisionPoint),
            ("## Decision points\n\n### B1 Q?\n", Problem::UnheadedDecisionPoints),
        ] {
            let unheaded = problem == Problem::UnheadedDecisionPoints;
            let extra = if unheaded { "#### B2 R?\n" } else { "" };
            let text = format!("{base}{heading}{options}{extra}{options}");
            assert_eq!(check_findings(&text).unwrap_err(), vec![problem], "{heading:?}");
        }
        assert!(check_findings(&format!("{base}### Notes on the decision\n")).is_ok());
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
            Problem::ListFence,
            Problem::DeepFence,
            Problem::HiddenPoint,
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
