//! Restart Claude tabs onto a newly installed Claude Code build without
//! restarting CodeScope.
//!
//! Claude Code's installer replaces `claude.exe` while tabs still run
//! it, so the update itself lands; each running `claude` just keeps the
//! old build until it restarts ("Update installed · Restart to
//! update"). Every few minutes this compares the installed build with
//! the one each Claude tab's process reports, marks the tabs that are
//! behind, and offers to restart them: an idle tab's `claude` is ended
//! with Ctrl+C and `claude --resume <session id>` is typed at the shell
//! it leaves behind, so the conversation carries on in the new build
//! (see `docs/DECISIONS.md`, ADR-0026).

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use codescope_core::agents::claude::live_session::{self, LiveProcess};
use codescope_core::agents::claude::restart::{self, StopStep};
use codescope_core::agents::claude::version::{self, ClaudeVersion};
use gpui::{Context, SharedString};

use super::{AppShell, ToastAction, ToastActionKind, ToastKind};

/// First check shortly after launch (past startup's own disk and
/// process work), then every few minutes — an update is a rare event,
/// and `claude --version` starts a few-hundred-MB binary.
const FIRST_CHECK: Duration = Duration::from_secs(30);
const CHECK_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// How often a restart looks whether the tab's `claude` has exited.
const EXIT_POLL: Duration = Duration::from_millis(50);

#[derive(Default)]
pub(super) struct ClaudeUpdates {
    /// What `claude --version` said at the last check.
    installed: Option<ClaudeVersion>,
    /// Claude tabs whose `claude` runs an older build, by tab id.
    outdated: HashMap<u64, LiveProcess>,
    /// Tabs with a restart in flight.
    restarting: HashSet<u64>,
    /// The build the toast last announced, so it shows once per update.
    announced: Option<ClaudeVersion>,
}

impl ClaudeUpdates {
    /// Whether the tab should show the "older Claude build" marker.
    pub(super) fn is_outdated(&self, tab_id: u64) -> bool {
        self.outdated.contains_key(&tab_id) && !self.restarting.contains(&tab_id)
    }

    /// The tab menu row: `Some(idle)` for an outdated tab.
    pub(super) fn restart_offer(&self, tab_id: u64) -> Option<bool> {
        self.is_outdated(tab_id).then(|| self.outdated[&tab_id].is_idle())
    }
}

/// Whether `pid` is still running.
fn process_alive(pid: u32) -> bool {
    codescope_core::process_tree::process_table().iter().any(|&(p, _)| p == pid)
}

impl AppShell {
    /// Start the periodic installed-build check. Called once from `new`.
    pub(super) fn start_claude_update_watch(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut wait = FIRST_CHECK;
            loop {
                cx.background_executor().timer(wait).await;
                wait = CHECK_INTERVAL;
                if this.upgrade().is_none() {
                    break;
                }
                let installed =
                    cx.background_executor().spawn(async { version::installed_version() }).await;
                if this.update(cx, |this, cx| this.apply_claude_check(installed, cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn apply_claude_check(&mut self, installed: Option<ClaudeVersion>, cx: &mut Context<Self>) {
        // A failed `claude --version` (PATH hiccup, install mid-swap)
        // keeps the last known build rather than clearing every marker.
        if installed.is_some() {
            self.claude_updates.installed = installed;
        }
        self.rescan_outdated_claude_tabs(cx);
        let Some(build) = self.claude_updates.installed.clone() else { return };
        let count = self.claude_updates.outdated.len();
        if count == 0 || self.claude_updates.announced.as_ref() == Some(&build) {
            return;
        }
        self.claude_updates.announced = Some(build.clone());
        let tabs = if count == 1 { "1 tab still runs".to_owned() } else { format!("{count} tabs still run") };
        self.push_action_toast(
            ToastKind::Info,
            format!("Claude Code {build} is installed"),
            Some(SharedString::from(format!(
                "{tabs} an older build. Restarting picks it up and keeps the conversation."
            ))),
            ToastAction {
                label: "Restart idle tabs".into(),
                kind: ToastActionKind::RestartOutdatedClaudeTabs,
            },
            cx,
        );
    }

    /// Re-read which Claude tabs run an older build than the installed
    /// one, and whether each is idle. Walks the process tree once —
    /// milliseconds — so it runs on a check or a click, not per frame.
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
        let changed = outdated.len() != self.claude_updates.outdated.len()
            || outdated.keys().any(|id| !self.claude_updates.outdated.contains_key(id));
        self.claude_updates.outdated = outdated;
        if changed {
            cx.notify();
        }
    }

    /// The toast action: restart every outdated tab that is idle right
    /// now. Busy ones are left alone and counted in a follow-up toast.
    pub(super) fn restart_outdated_claude_tabs(&mut self, cx: &mut Context<Self>) {
        self.rescan_outdated_claude_tabs(cx);
        let mut busy = 0;
        let mut idle = Vec::new();
        for (&tab_id, live) in &self.claude_updates.outdated {
            if self.claude_updates.restarting.contains(&tab_id) {
                continue;
            }
            if live.is_idle() {
                idle.push(tab_id);
            } else {
                busy += 1;
            }
        }
        for tab_id in idle {
            self.restart_claude_tab(tab_id, cx);
        }
        if busy > 0 {
            let title = if busy == 1 {
                "1 busy tab stays on the older build".to_owned()
            } else {
                format!("{busy} busy tabs stay on the older build")
            };
            self.push_toast(
                ToastKind::Info,
                title,
                Some("Restart them from the tab menu once they are idle.".into()),
                cx,
            );
        }
    }

    /// Restart one tab's `claude` onto the installed build, resuming
    /// its conversation. Only an idle `claude` is interrupted — the
    /// state is re-read here, not trusted from the last scan.
    pub(super) fn restart_claude_tab(&mut self, tab_id: u64, cx: &mut Context<Self>) {
        if self.claude_updates.restarting.contains(&tab_id) {
            return;
        }
        let Some(tab) = self.groups.iter().flat_map(|g| g.tabs.iter()).find(|t| t.id == tab_id)
        else {
            return;
        };
        let terminal = tab.terminal.clone();
        let title = tab.title.clone();
        let wd = tab
            .working_directory
            .as_ref()
            .map(|wd| wd.to_string_lossy().into_owned())
            .unwrap_or_default();
        let live = live_session::default_sessions_dir().zip(terminal.read(cx).child_pid()).and_then(
            |(dir, shell)| {
                let table = codescope_core::process_tree::process_table();
                let pids = codescope_core::process_tree::descendants(shell, &table);
                live_session::process_for_pids(&dir, &pids, &wd)
            },
        );
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
        let has_transcript = codescope_core::agents::claude::telemetry::default_projects_root()
            .map(|root| {
                codescope_core::agents::claude::telemetry::transcript_path(
                    &root,
                    &live.cwd,
                    &live.session_id,
                )
            })
            .is_some_and(|path| path.is_file());
        let Some(resume) = self
            .agent_registry
            .get_by_id(codescope_core::AgentId::Claude.as_str())
            .and_then(|profile| {
                restart::relaunch_command(profile, &live.session_id, has_transcript)
            })
        else {
            return;
        };
        self.claude_updates.restarting.insert(tab_id);
        cx.notify();
        let pid = live.pid;
        cx.spawn(async move |this, cx| {
            // `Some(true)` resumed, `Some(false)` gave up, `None` the
            // tab closed under us (nothing to report).
            let mut sent = 0;
            let outcome: Option<bool> = loop {
                let running =
                    cx.background_executor().spawn(async move { process_alive(pid) }).await;
                match restart::next_step(sent, running) {
                    StopStep::Resume => {
                        let typed = terminal.update(cx, |term, _| {
                            let mut bytes = resume.into_bytes();
                            bytes.push(b'\r');
                            term.write_input(bytes);
                        });
                        break typed.ok().map(|()| true);
                    }
                    StopStep::GiveUp => break Some(false),
                    StopStep::Interrupt { wait } => {
                        if terminal
                            .update(cx, |term, _| term.write_input(restart::INTERRUPT))
                            .is_err()
                        {
                            break None;
                        }
                        sent += 1;
                        let deadline = Instant::now() + wait;
                        while Instant::now() < deadline {
                            cx.background_executor().timer(EXIT_POLL).await;
                            let alive = cx
                                .background_executor()
                                .spawn(async move { process_alive(pid) })
                                .await;
                            if !alive {
                                break;
                            }
                        }
                    }
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.claude_updates.restarting.remove(&tab_id);
                if outcome != Some(false) {
                    this.claude_updates.outdated.remove(&tab_id);
                } else {
                    this.push_toast(
                        ToastKind::Err,
                        "Could not restart Claude",
                        Some(SharedString::from(format!(
                            "{title}: claude did not exit after Ctrl+C. Restart it by hand."
                        ))),
                        cx,
                    );
                }
                cx.notify();
            });
        })
        .detach();
    }
}
