//! Restart Claude tabs onto a newly installed Claude Code build without
//! restarting CodeScope.
//!
//! Claude Code's installer replaces `claude.exe` while tabs still run
//! it, so the update itself lands; each running `claude` just keeps the
//! old build until it restarts ("Update installed · Restart to
//! update"). Every few minutes this compares the installed build with
//! the one each Claude tab's process reports and marks the tabs that
//! are behind. Those tabs are then restarted on their own as soon as
//! that is safe (see [`restart::may_auto_restart`]): the idle `claude`
//! is ended from its own prompt and `claude --resume <session id>` is
//! typed at the shell it leaves behind, so the conversation carries on
//! in the new build (see `docs/DECISIONS.md`, ADR-0026). The tab menu
//! offers the same restart by hand for a tab that keeps waiting.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use codescope_core::agents::claude::live_session::{self, LiveProcess};
use codescope_core::agents::claude::restart::{self, ClaudeState, StopStep};
use codescope_core::agents::claude::version::{self, ClaudeVersion};
use gpui::{Context, SharedString};

use super::{AppShell, ToastKind};

/// One tick of the watch. While a tab is outdated every tick re-reads
/// the tabs and restarts the ones that became safe to restart (a
/// process-table walk plus a small file per Claude tab); otherwise a
/// tick does nothing but count towards the next version check.
const TICK: Duration = Duration::from_secs(15);
/// First version check shortly after launch (past startup's own disk
/// and process work), then every few minutes — an update is a rare
/// event, and `claude --version` starts a few-hundred-MB binary.
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How often a restart looks whether the tab's `claude` has exited.
/// Each look is a process snapshot (a `ps` spawn on Unix).
const EXIT_POLL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(super) struct ClaudeUpdates {
    /// What `claude --version` said at the last check.
    installed: Option<ClaudeVersion>,
    /// Claude tabs whose `claude` runs an older build, by tab id.
    outdated: HashMap<u64, LiveProcess>,
    /// Tabs with a restart in flight.
    restarting: HashSet<u64>,
    /// The build each tab was last restarted towards. A tab is
    /// restarted on its own once per installed build: if it comes back
    /// still outdated (its shell resolves `claude` to another install
    /// than CodeScope does) or its `claude` wouldn't exit, retrying
    /// every tick would only repeat that. The tab menu still can.
    attempted: HashMap<u64, ClaudeVersion>,
}

impl ClaudeUpdates {
    /// Whether the tab should show the "older Claude build" marker and
    /// the tab menu's restart row.
    pub(super) fn is_outdated(&self, tab_id: u64) -> bool {
        self.outdated.contains_key(&tab_id) && !self.restarting.contains(&tab_id)
    }

    /// Whether a restart is driving the tab's `claude` now.
    pub(super) fn is_restarting(&self, tab_id: u64) -> bool {
        self.restarting.contains(&tab_id)
    }
}

/// How a restart ended.
enum Outcome {
    /// The relaunch command was typed.
    Relaunched,
    /// `claude` didn't exit.
    GaveUp,
    /// The tab went busy, its state couldn't be read, or the user
    /// started typing; nothing more was typed. Eligible again later.
    Aborted,
}

/// Where the tab's `claude` (`pid`, under the tab's `shell`) stands.
/// Blocking: takes a process snapshot and reads one small file.
fn claude_state(shell: u32, pid: u32, sessions_dir: &std::path::Path, cwd: &str) -> ClaudeState {
    let table = codescope_core::process_tree::process_table();
    // An empty snapshot (the platform query failed) has no shell in it
    // either: unknown, never "exited".
    if !table.iter().any(|&(p, _)| p == shell) {
        return ClaudeState::Unknown;
    }
    // Under the tab's shell, not merely alive: a recycled pid elsewhere
    // doesn't count as our `claude` still running.
    if !codescope_core::process_tree::descendants(shell, &table).contains(&pid) {
        return ClaudeState::Gone;
    }
    match live_session::process_for_pids(sessions_dir, &[pid], cwd) {
        Some(live) if !live.is_idle() => ClaudeState::Busy,
        // Idle, or the pid file is already gone mid-shutdown.
        _ => ClaudeState::Idle,
    }
}

impl AppShell {
    /// Start the watch. Called once from `new`.
    pub(super) fn start_claude_update_watch(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(FIRST_CHECK).await;
            let mut last_check: Option<Instant> = None;
            loop {
                if this.upgrade().is_none() {
                    break;
                }
                let due = last_check.is_none_or(|at| at.elapsed() >= CHECK_INTERVAL);
                let installed = if due {
                    last_check = Some(Instant::now());
                    cx.background_executor().spawn(async { version::installed_version() }).await
                } else {
                    None
                };
                if this.update(cx, |this, cx| this.claude_update_tick(due, installed, cx)).is_err() {
                    break;
                }
                cx.background_executor().timer(TICK).await;
            }
        })
        .detach();
    }

    fn claude_update_tick(
        &mut self,
        checked: bool,
        installed: Option<ClaudeVersion>,
        cx: &mut Context<Self>,
    ) {
        // A failed `claude --version` (PATH hiccup, install mid-swap)
        // keeps the last known build rather than clearing every marker.
        if installed.is_some() {
            self.claude_updates.installed = installed;
        }
        if !checked && self.claude_updates.outdated.is_empty() {
            return;
        }
        self.rescan_outdated_claude_tabs(cx);
        let in_view: HashSet<u64> = if self.window_active_cached {
            self.groups
                .iter()
                .filter_map(|g| g.tabs.get(g.active_tab))
                .map(|t| t.id)
                .collect()
        } else {
            HashSet::new()
        };
        let updates = &self.claude_updates;
        let ready: Vec<u64> = updates
            .outdated
            .iter()
            .filter(|&(tab_id, live)| {
                !updates.restarting.contains(tab_id)
                    && updates.attempted.get(tab_id) != updates.installed.as_ref()
                    && restart::may_auto_restart(
                        live.is_idle(),
                        self.tab_may_have_draft(*tab_id, cx),
                        in_view.contains(tab_id),
                    )
            })
            .map(|(&tab_id, _)| tab_id)
            .collect();
        for tab_id in ready {
            self.restart_claude_tab(tab_id, cx);
        }
    }

    fn tab_may_have_draft(&self, tab_id: u64, cx: &Context<Self>) -> bool {
        self.groups
            .iter()
            .flat_map(|g| g.tabs.iter())
            .find(|t| t.id == tab_id)
            .is_none_or(|t| t.terminal.read(cx).typed_since_submit())
    }

    /// Re-read which Claude tabs run an older build than the installed
    /// one, and whether each is idle. Walks the process tree once —
    /// milliseconds — so it runs on a tick, not per frame.
    fn rescan_outdated_claude_tabs(&mut self, cx: &mut Context<Self>) {
        let mut outdated = HashMap::new();
        if let Some(installed) = self.claude_updates.installed.clone()
            && let Some(dir) = live_session::default_sessions_dir()
        {
            let table = codescope_core::process_tree::process_table();
            for tab in self.groups.iter().flat_map(|g| g.tabs.iter()) {
                // Remote tabs run `claude` on another machine.
                if tab.agent_id != Some(codescope_core::AgentId::Claude)
                    || tab.remote_project_id.is_some()
                {
                    continue;
                }
                let Some(shell) = tab.terminal.read(cx).child_pid() else { continue };
                let wd = tab
                    .working_directory
                    .as_ref()
                    .map(|wd| wd.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let pids = codescope_core::process_tree::descendants(shell, &table);
                let Some(live) = live_session::process_for_pids(&dir, &pids, &wd) else {
                    continue;
                };
                if version::is_outdated(live.version.as_deref(), &installed) {
                    outdated.insert(tab.id, live);
                }
            }
        }
        // Closed, restarted or exited tabs are done with.
        self.claude_updates.attempted.retain(|id, _| outdated.contains_key(id));
        let changed = outdated.len() != self.claude_updates.outdated.len()
            || outdated.keys().any(|id| !self.claude_updates.outdated.contains_key(id));
        self.claude_updates.outdated = outdated;
        if changed {
            cx.notify();
        }
    }

    /// Restart one tab's `claude` onto the installed build, resuming
    /// its conversation. Only an idle `claude` is interrupted — the
    /// state is re-read here and before every keystroke, not trusted
    /// from the last scan.
    pub(super) fn restart_claude_tab(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        if self.claude_updates.restarting.contains(&tab_id) {
            return;
        }
        let Some(tab) = self.groups.iter().flat_map(|g| g.tabs.iter()).find(|t| t.id == tab_id)
        else {
            return;
        };
        let Some(sessions_dir) = live_session::default_sessions_dir() else { return };
        let Some(shell) = tab.terminal.read(cx).child_pid() else { return };
        let title = tab.title.clone();
        let wd = tab
            .working_directory
            .as_ref()
            .map(|wd| wd.to_string_lossy().into_owned())
            .unwrap_or_default();
        let live = {
            let table = codescope_core::process_tree::process_table();
            let pids = codescope_core::process_tree::descendants(shell, &table);
            live_session::process_for_pids(&sessions_dir, &pids, &wd)
        };
        let Some(live) = live else {
            // `claude` already exited; nothing left to restart.
            self.claude_updates.outdated.remove(&tab_id);
            cx.notify();
            return;
        };
        if !live.is_idle() {
            self.push_toast(
                ToastKind::Info,
                "Claude is busy in this tab",
                Some("Restart it once the current turn is done.".into()),
                cx,
            );
            return;
        }
        // Resume when the conversation exists: on disk, or known to the
        // adoption tick (which only adopts transcripts it saw) — so a
        // missed path lookup can't swap a conversation for a blank one.
        let has_transcript = tab.adopted_session_id.as_deref() == Some(live.session_id.as_str())
            || tab.fired_session_ids.contains(&live.session_id)
            || codescope_core::agents::claude::telemetry::default_projects_root()
                .map(|root| {
                    codescope_core::agents::claude::telemetry::transcript_path(
                        &root,
                        &live.cwd,
                        &live.session_id,
                    )
                })
                .is_some_and(|path: PathBuf| path.is_file());
        let Some(relaunch) = self
            .agent_registry
            .get_by_id(codescope_core::AgentId::Claude.as_str())
            .and_then(|profile| {
                restart::relaunch_command(profile, &live.session_id, has_transcript)
            })
        else {
            return;
        };
        // Weak: closing the tab mid-restart drops the terminal and ends
        // the sequence, instead of keeping a tab-less pty alive for it.
        let terminal = tab.terminal.downgrade();
        let draft_at_start = tab.terminal.read(cx).typed_since_submit();
        self.claude_updates.restarting.insert(tab_id);
        if let Some(build) = self.claude_updates.installed.clone() {
            self.claude_updates.attempted.insert(tab_id, build);
        }
        cx.notify();
        let pid = live.pid;
        let cwd = live.cwd.clone();
        cx.spawn(async move |this, cx| {
            let state = |cx: &mut gpui::AsyncApp| {
                let dir = sessions_dir.clone();
                let cwd = cwd.clone();
                cx.background_executor().spawn(async move { claude_state(shell, pid, &dir, &cwd) })
            };
            let mut sent = 0;
            let outcome = loop {
                let now = state(cx).await;
                match restart::next_step(sent, now) {
                    StopStep::Resume => {
                        let typed = terminal.update(cx, |term, _| {
                            let mut bytes = relaunch.into_bytes();
                            bytes.push(b'\r');
                            term.write_input(bytes);
                        });
                        break if typed.is_ok() { Outcome::Relaunched } else { Outcome::Aborted };
                    }
                    StopStep::GiveUp => break Outcome::GaveUp,
                    StopStep::Abort => break Outcome::Aborted,
                    StopStep::Send { bytes, wait } => {
                        // The user started typing into the tab: it's
                        // theirs again (an error reading it means the
                        // tab closed).
                        let typing = terminal
                            .read_with(cx, |term, _| term.typed_since_submit())
                            .unwrap_or(true);
                        if typing && !draft_at_start {
                            break Outcome::Aborted;
                        }
                        if terminal.update(cx, |term, _| term.write_input(bytes)).is_err() {
                            break Outcome::Aborted;
                        }
                        sent += 1;
                        let deadline = Instant::now() + wait;
                        while Instant::now() < deadline {
                            cx.background_executor().timer(EXIT_POLL).await;
                            if state(cx).await == ClaudeState::Gone {
                                break;
                            }
                        }
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                let updates = &mut this.claude_updates;
                updates.restarting.remove(&tab_id);
                match outcome {
                    Outcome::Relaunched => {
                        updates.outdated.remove(&tab_id);
                    }
                    Outcome::Aborted => {
                        updates.attempted.remove(&tab_id);
                    }
                    Outcome::GaveUp => {
                        this.push_toast(
                            ToastKind::Err,
                            "Could not restart Claude",
                            Some(SharedString::from(format!(
                                "{title}: claude did not exit. Restart it by hand."
                            ))),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}
