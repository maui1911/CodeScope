//! Whose turn each card is (ADR-0027), which cards are the user's own for *Mine* and which need
//! them, and the count of the user's dispatches in the board header and the status bar.

use std::collections::{HashMap, HashSet};

use codescope_core::board::dispatch::Hold;
use codescope_core::board::turn::{Turn, TurnSince, turn};
use codescope_core::board::{BoardItem, effective_mapping};
use codescope_core::time::{secs_since, span};
use gpui::{App, SharedString};

use super::{AppShell, CardNote, NoteTone};

/// A card's turn, as its top line and the drawer show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TurnLabel {
    /// `You · 6 min`, `@sanne's agent · 18 min`, `CI · 2 min`.
    pub text: SharedString,
    /// Set when it is the turn of the user, so the item needs them.
    pub yours: bool,
}

/// The user's dispatches, as the board header counts them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DispatchCount {
    pub running: usize,
    pub limit: usize,
    /// Items on this board that need the user.
    pub need_you: usize,
    /// Items on this board that wait until fewer dispatches run.
    pub waiting: usize,
}

impl DispatchCount {
    /// `2 of 3 running · 1 needs you · 1 waiting for a slot`, leaving out what is zero.
    pub(crate) fn text(&self) -> String {
        let mut parts = vec![format!("{} of {} running", self.running, self.limit)];
        parts.extend(self.need_you_text());
        if self.waiting > 0 {
            parts.push(format!("{} waiting for a slot", self.waiting));
        }
        parts.join(" · ")
    }

    /// `board: 2/3 running`, as the status bar starts its count.
    pub(crate) fn running_text(&self) -> String {
        format!("board: {}/{} running", self.running, self.limit)
    }

    /// `1 needs you` or `4 need you`; `None` while nothing does.
    pub(crate) fn need_you_text(&self) -> Option<String> {
        match self.need_you {
            0 => None,
            1 => Some("1 needs you".to_string()),
            n => Some(format!("{n} need you")),
        }
    }
}

impl AppShell {
    /// The turn of each dispatched item on the board at `board_url`, by item id.
    pub(crate) fn turn_labels(&self, board_url: &str) -> HashMap<String, TurnLabel> {
        let Some(read) = self.board.reads_by_url.get(board_url) else {
            return HashMap::new();
        };
        let (Some(board), Some(settings)) = (
            read.last_good.as_ref(),
            self.settings.boards.iter().find(|b| b.url == board_url),
        ) else {
            return HashMap::new();
        };
        let mapping = effective_mapping(board, &settings.columns);
        let viewer = self.board.dispatch.viewer.as_ref().map(|v| v.login.as_str());
        let now = codescope_core::time::parse_iso8601_secs(&codescope_core::time::now_iso8601())
            .unwrap_or(0.0);
        board
            .items
            .iter()
            .filter_map(|item| {
                let pull_request = item
                    .content_id
                    .as_ref()
                    .and_then(|id| read.turns.as_ref()?.get(id)?.as_ref());
                let turn = turn(item, board.role_of(item, &mapping), pull_request)?;
                Some((item.id.clone(), turn_label(&turn, &item.assignees, viewer, now)))
            })
            .collect()
    }

    /// The items on the board at `board_url` that *Mine* shows.
    pub(crate) fn my_items(&self, board_url: &str, cx: &App) -> HashSet<String> {
        let Some(read) = self.board.reads_by_url.get(board_url) else {
            return HashSet::new();
        };
        let Some(board) = read.last_good.as_ref() else {
            return HashSet::new();
        };
        let viewer = self.board.dispatch.viewer.as_ref().map(|v| v.login.as_str());
        let dispatched: HashSet<String> = self
            .dispatch_records(cx)
            .into_iter()
            .filter(|(_, _, _, r)| r.board_url == board_url)
            .map(|(_, _, _, r)| r.item_id)
            .collect();
        board
            .items
            .iter()
            .filter(|item| {
                let mover = item
                    .content_id
                    .as_ref()
                    .and_then(|id| read.triggers.as_ref()?.changes.get(id)?.actor.as_deref());
                is_mine(item, viewer, dispatched.contains(&item.id), mover)
            })
            .map(|item| item.id.clone())
            .collect()
    }

    /// The items on the board at `board_url` that need the user, out of its cards' `notes` and
    /// `turns`.
    pub(crate) fn need_you_items(
        &self,
        board_url: &str,
        notes: &HashMap<String, CardNote>,
        turns: &HashMap<String, TurnLabel>,
    ) -> HashSet<String> {
        needing_you(&self.items_on(board_url), notes, turns)
    }

    fn items_on(&self, board_url: &str) -> HashSet<&str> {
        self.board
            .reads_by_url
            .get(board_url)
            .and_then(|read| read.last_good.as_ref())
            .map(|board| board.items.iter().map(|item| item.id.as_str()).collect())
            .unwrap_or_default()
    }

    /// The user's dispatches, `need_you` of the items on the board at `board_url` that need them,
    /// and how many wait for a slot there.
    pub(crate) fn dispatch_count(
        &self,
        board_url: &str,
        need_you: usize,
        cx: &App,
    ) -> DispatchCount {
        DispatchCount {
            running: self.running_dispatches(cx),
            limit: self.settings.dispatch_limit,
            need_you,
            waiting: self.waiting_for_a_slot(board_url),
        }
    }

    fn waiting_for_a_slot(&self, board_url: &str) -> usize {
        let on_board = self.items_on(board_url);
        let holds = &self.board.dispatch.holds;
        holds
            .iter()
            .filter(|(id, hold)| {
                on_board.contains(id.as_str()) && matches!(hold, Hold::LimitReached(_))
            })
            .count()
    }

    /// The user's dispatches, and what needs them or waits on every board, as the status bar and
    /// the sidebar's *Board* button count them; `None` while no board is added.
    pub(crate) fn boards_count(&self, cx: &App) -> Option<DispatchCount> {
        let boards = &self.settings.boards;
        if boards.is_empty() {
            return None;
        }
        let need_you = boards.iter().map(|board| {
            let notes = self.card_notes(&board.url, cx);
            let turns = self.turn_labels(&board.url);
            self.need_you_items(&board.url, &notes, &turns).len()
        });
        Some(DispatchCount {
            running: self.running_dispatches(cx),
            limit: self.settings.dispatch_limit,
            need_you: need_you.sum(),
            waiting: boards.iter().map(|board| self.waiting_for_a_slot(&board.url)).sum(),
        })
    }
}

/// The items of `on_board` whose card says they need the user, or whose turn is the user's.
fn needing_you(
    on_board: &HashSet<&str>,
    notes: &HashMap<String, CardNote>,
    turns: &HashMap<String, TurnLabel>,
) -> HashSet<String> {
    notes
        .iter()
        .filter(|(_, note)| note.tone == NoteTone::NeedsYou)
        .map(|(id, _)| id)
        .chain(turns.iter().filter(|(_, turn)| turn.yours).map(|(id, _)| id))
        .filter(|id| on_board.contains(id.as_str()))
        .cloned()
        .collect()
}

/// Whether *Mine* shows `item`: it is assigned to the user, it is one of their dispatches, or
/// nobody has it yet and the user moved it into its trigger column, the last move `mover` made.
fn is_mine(
    item: &BoardItem,
    viewer: Option<&str>,
    dispatched: bool,
    mover: Option<&str>,
) -> bool {
    let Some(viewer) = viewer else {
        return true;
    };
    let me = |login: &str| login.eq_ignore_ascii_case(viewer);
    dispatched
        || item.assignees.iter().any(|login| me(login))
        || (item.assignees.is_empty() && mover.is_some_and(me))
}

fn turn_label(
    turn: &TurnSince,
    assignees: &[String],
    viewer: Option<&str>,
    now: f64,
) -> TurnLabel {
    let mine = |login: &str| viewer.is_some_and(|v| v.eq_ignore_ascii_case(login));
    let (who, yours) = match &turn.turn {
        Turn::Agent => match assignees {
            [login] if mine(login) => ("Your agent".to_string(), false),
            [login] => (format!("@{login}'s agent"), false),
            _ => ("An agent".to_string(), false),
        },
        Turn::Ci => ("CI".to_string(), false),
        Turn::Assignees if assignees.is_empty() => ("Unassigned".to_string(), false),
        Turn::Assignees => (people(assignees, viewer), assignees.iter().any(|l| mine(l))),
        Turn::Reviewers(names) if names.is_empty() => ("Reviewers".to_string(), false),
        Turn::Reviewers(names) => (people(names, viewer), names.iter().any(|l| mine(l))),
        Turn::Nobody => ("Nobody".to_string(), false),
    };
    let text = match turn.since.as_deref().and_then(|at| secs_since(at, now)) {
        Some(secs) => format!("{who} · {}", span(secs)),
        None => who,
    };
    TurnLabel {
        text: text.into(),
        yours,
    }
}

/// `You, @sanne +1`: the user first, at most two names.
fn people(logins: &[String], viewer: Option<&str>) -> String {
    let mine = |login: &str| viewer.is_some_and(|v| v.eq_ignore_ascii_case(login));
    let mut names: Vec<String> = logins
        .iter()
        .filter(|login| mine(login))
        .map(|_| "You".to_string())
        .take(1)
        .chain(logins.iter().filter(|login| !mine(login)).map(|login| format!("@{login}")))
        .collect();
    let more = names.len().saturating_sub(2);
    names.truncate(2);
    let mut text = names.join(", ");
    if more > 0 {
        text.push_str(&format!(" +{more}"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use codescope_core::board::{ItemKind, ItemState};

    fn now() -> f64 {
        codescope_core::time::parse_iso8601_secs("2026-11-27T22:00:00Z").unwrap()
    }

    fn label(turn: Turn, assignees: &[&str]) -> TurnLabel {
        let assignees: Vec<String> = assignees.iter().map(|s| s.to_string()).collect();
        let turn = TurnSince {
            turn,
            since: Some("2026-11-27T21:54:00Z".into()),
        };
        turn_label(&turn, &assignees, Some("Maui1911"), now())
    }

    fn text(turn: Turn, assignees: &[&str]) -> String {
        label(turn, assignees).text.to_string()
    }

    #[test]
    fn a_turn_names_whose_it_is_and_since_when() {
        assert_eq!(text(Turn::Agent, &["maui1911"]), "Your agent · 6 min");
        assert_eq!(text(Turn::Agent, &["sanne"]), "@sanne's agent · 6 min");
        assert_eq!(text(Turn::Agent, &["sanne", "maui1911"]), "An agent · 6 min");
        assert_eq!(text(Turn::Ci, &["maui1911"]), "CI · 6 min");
        assert_eq!(text(Turn::Assignees, &["sanne"]), "@sanne · 6 min");
        assert_eq!(text(Turn::Assignees, &[]), "Unassigned · 6 min");
        assert_eq!(text(Turn::Reviewers(Vec::new()), &["maui1911"]), "Reviewers · 6 min");
        let nobody = TurnSince {
            turn: Turn::Nobody,
            since: None,
        };
        assert_eq!(turn_label(&nobody, &[], None, now()).text.as_ref(), "Nobody");
    }

    #[test]
    fn it_is_yours_when_you_are_among_the_people_it_waits_on() {
        let yours = label(Turn::Assignees, &["sanne", "maui1911"]);
        assert_eq!(yours.text.as_ref(), "You, @sanne · 6 min");
        assert!(yours.yours);
        let asked = vec!["lead".to_string(), "maui1911".into(), "acme/web".into(), "ana".into()];
        let review = label(Turn::Reviewers(asked), &["sanne"]);
        assert_eq!(review.text.as_ref(), "You, @lead +2 · 6 min");
        assert!(review.yours);
        assert!(!label(Turn::Agent, &["maui1911"]).yours, "your agent works, not you");
    }

    #[test]
    fn mine_is_assigned_to_me_dispatched_by_me_or_moved_by_me_while_nobody_has_it() {
        let item = |assignees: &[&str]| BoardItem {
            id: "PVTI_1".into(),
            kind: ItemKind::Issue,
            number: Some(7),
            title: "Fix it".into(),
            url: None,
            repository: Some("o/r".into()),
            content_id: Some("I_7".into()),
            assignees: assignees.iter().map(|s| s.to_string()).collect(),
            state: ItemState::Open,
            column_id: None,
            column_set_at: None,
            updated_at: String::new(),
        };
        let me = Some("maui1911");
        assert!(is_mine(&item(&["Maui1911"]), me, false, None));
        assert!(is_mine(&item(&["sanne"]), me, true, None));
        assert!(is_mine(&item(&[]), me, false, Some("maui1911")));
        assert!(!is_mine(&item(&["sanne"]), me, false, Some("maui1911")));
        assert!(!is_mine(&item(&[]), me, false, Some("sanne")));
        assert!(!is_mine(&item(&[]), me, false, None));
        assert!(is_mine(&item(&["sanne"]), None, false, None), "all, until gh says who you are");
    }

    #[test]
    fn the_count_leaves_out_what_is_zero() {
        let count = |need_you, waiting| DispatchCount {
            running: 2,
            limit: 3,
            need_you,
            waiting,
        };
        assert_eq!(count(0, 0).text(), "2 of 3 running");
        assert_eq!(count(1, 0).text(), "2 of 3 running · 1 needs you");
        assert_eq!(count(3, 1).text(), "2 of 3 running · 3 need you · 1 waiting for a slot");
    }

    #[test]
    fn the_status_bar_says_board_and_what_runs_and_then_what_needs_you() {
        let count = |need_you| DispatchCount {
            running: 3,
            limit: 3,
            need_you,
            waiting: 2,
        };
        assert_eq!(count(0).running_text(), "board: 3/3 running");
        assert_eq!(count(0).need_you_text(), None);
        assert_eq!(count(1).need_you_text().as_deref(), Some("1 needs you"));
        assert_eq!(count(4).need_you_text().as_deref(), Some("4 need you"));
    }

    #[test]
    fn an_item_needs_you_when_its_card_says_so_or_it_is_your_turn() {
        let note = |tone| CardNote::new("", tone);
        let notes = HashMap::from([
            ("PVTI_choice".to_string(), note(NoteTone::NeedsYou)),
            ("PVTI_working".to_string(), note(NoteTone::Working)),
            ("PVTI_elsewhere".to_string(), note(NoteTone::NeedsYou)),
        ]);
        let turn = |yours| TurnLabel {
            text: "".into(),
            yours,
        };
        let turns = HashMap::from([
            ("PVTI_review".to_string(), turn(true)),
            ("PVTI_working".to_string(), turn(false)),
        ]);
        let on_board = HashSet::from(["PVTI_choice", "PVTI_working", "PVTI_review"]);
        let mut needing = Vec::from_iter(needing_you(&on_board, &notes, &turns));
        needing.sort();
        assert_eq!(needing, ["PVTI_choice", "PVTI_review"]);
    }
}
