//! Application shell — the chrome that wraps one or more
//! `TerminalView` entities.
//!
//! Layout (top to bottom):
//!
//! ```text
//! ┌────────────────────────────────────────────────────────┐
//! │ brand · «  · drag region · [+] · ▭ ▢ ✕                 │  ← 32 px caption row
//! ├────────────────────────────────────────────────────────┤
//! │ sidebar pad │ [strip g0] │ [strip g1] · ...            │  ← 40 px strip row
//! ├────────────────────────────────────────────────────────┤
//! │ side │ ║ │  pane g0      │ ║ │  pane g1                │
//! │ bar  │   │  (active term)│   │  (active term)          │
//! └────────────────────────────────────────────────────────┘
//! ```
//!
//! The caption row holds chrome (brand mark, sidebar-toggle, drag,
//! split, and min/max/close). The strip row holds *only* the
//! sidebar-mirror padding plus the per-group strip sections — that
//! way both the strip row and the work row below it share the same
//! horizontal extent, so the dividers between groups in the strip
//! line up *exactly* with the splitters between panes below.
//!
//! Each top-level "group" is a tab strip + active terminal. Multiple
//! groups sit side-by-side with the same weight per column on the
//! tab-strip row and the body row, so clicking a strip section focuses
//! the matching pane below. Groups mirror the C# build's
//! `EditorGroupViewModel` flat list — no recursive split tree.
//!
//! Visuals follow `src/CodeScope.App/Styles/DesignTokens.xaml`:
//! pure-black canvas, pure-white ink, single Framer Blue accent,
//! frosted-glass surfaces. See [`crate::theme`] for the tokens.
//!
//! Each tab owns its own [`Backend`] + [`TerminalView`]. Closing a
//! tab drops the entity, which drops the backend, which sends
//! `Msg::Shutdown` to the alacritty event loop and joins the worker
//! thread. When the last tab in a non-only group is closed the group
//! itself collapses (mirroring `MainViewModel.CloseGroup`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use codescope_core::{
    AppPaths, FontSettings, LayoutState, ProjectsConfig, Session, SessionManager, Settings, Theme,
    WindowState, now_iso8601,
};
use codescope_terminal::{
    Backend, ColorPalette, CursorStylePreset, FontConfig, Shell, SpawnConfig, TerminalSize,
    TerminalView,
};
use gpui::StatefulInteractiveElement;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    AppContext, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Styled, Window, WindowBounds,
    WindowControlArea, div, px,
};
use parking_lot::Mutex;

use crate::sidebar::{
    SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH, Sidebar, SidebarEvent,
    ToastSeverity,
};
use crate::theme;

/// Pixel height of the bottom status bar. Shared between
/// `render_status_bar` (the actual bar's `h(...)`) and
/// `render_notifications_popover` (first-frame fallback bottom inset
/// when the bell button's bounds haven't been recorded yet).
const STATUS_BAR_HEIGHT_PX: f32 = 32.0;

/// How often the window-state debounce loop wakes up to check whether
/// the latest pending save has been stable long enough.
const WINDOW_SAVE_POLL: Duration = Duration::from_millis(150);
/// How long the pending save must sit untouched before we actually
/// hit disk. Long enough that a drag-resize doesn't write on every
/// pixel; short enough that a normal resize-and-let-go feels instant.
const WINDOW_SAVE_DEBOUNCE: Duration = Duration::from_millis(500);
/// How often the settings-watch loop checks `settings.json`'s mtime.
/// 1 s is plenty — settings edits are user-driven and never on a hot
/// path; the latency is "save-to-see" and the user won't notice the
/// gap. Using mtime polling instead of a real fswatch keeps us off
/// the `notify` crate dependency for a single-file watch.
const SETTINGS_POLL: Duration = Duration::from_millis(1000);

struct PendingWindowSave {
    state: WindowState,
    set_at: Instant,
}

/// One tab = one terminal session.
struct Tab {
    id: u64,
    /// The CodeScope session id — the stable identifier persisted in
    /// `projects.json` under the owning project's `sessions[]`.
    /// Allocated at spawn via [`SessionManager::open`] (or restored
    /// from disk for a launch-time rehydrate). Distinct from
    /// [`Tab::adopted_session_id`] which is the *Claude Code* UUID
    /// discovered later from a tail of the agent's `.jsonl` transcript.
    /// Mirrors C# `SessionTabViewModel.Descriptor.Id` /
    /// `Models.Session.Id`.
    session_id: String,
    title: SharedString,
    terminal: Entity<TerminalView>,
    /// Working directory the pty was spawned in. Captured so a
    /// session-restore round-trip can re-spawn the tab in the same
    /// folder without going through the sidebar's active-project
    /// fallback (which may have shifted since save).
    working_directory: Option<std::path::PathBuf>,
    /// Auto-typed command at spawn (None for plain shells, Some for
    /// agent-launch tabs). Persisted so "New Claude session" comes
    /// back as claude on restore.
    auto_type: Option<SharedString>,
    /// Wall-clock spawn time, captured at `spawn_tab_in`. Used as the
    /// `since` filter for Claude session adoption — only `.jsonl`
    /// transcripts created/modified at or after this point qualify
    /// as "this tab's session". Mirrors the `since` arg of
    /// `ClaudeSessionDiscovery.Watch`.
    spawned_at: SystemTime,
    /// Adopted agent session id, set once the agent-specific discovery
    /// scan returns a fresh transcript / message dir / session dir for
    /// this tab. `None` until the agent has written its first
    /// recognisable file — and stays `None` for plain shell tabs
    /// (no agent `auto_type`). On adoption we register the telemetry
    /// tail via the per-`agent_id` dispatch in `register_telemetry`;
    /// on close we unregister.
    ///
    /// For Claude this id rotates on `/clear` (a new `.jsonl` lands
    /// under the same encoded-cwd directory). For Pi the discovery
    /// loop also keeps re-firing as the user re-invokes `pi`. In both
    /// cases the loop unregisters the previous tail before swapping
    /// in the new id, mirroring C# `WatchHandle._fired` +
    /// `MainViewModel.ApplyAdoption`.
    adopted_session_id: Option<String>,
    /// Every agent session id we've already registered telemetry for
    /// on this tab — i.e. the historical adoption set.  Kept so the
    /// discovery loop can dedupe when re-scanning after `/clear`
    /// rotations (or successive `pi` invocations) and avoid registering
    /// the same tail twice.  Mirrors the C# `WatchHandle._fired`
    /// HashSet.
    fired_session_ids: std::collections::HashSet<String>,
    /// Detected agent backend, derived from `auto_type` at spawn time
    /// via [`codescope_core::agent_id_from_auto_type`]. `None` for
    /// plain shell tabs (no auto-type) and tabs whose first token
    /// doesn't match a known agent. Drives discovery + telemetry
    /// dispatch — mirrors the `agentId` string the C#
    /// `MainViewModel.RegisterAgentTelemetry` branches on.
    agent_id: Option<codescope_core::AgentId>,
}

/// One column in the work area: a tab strip + the currently-selected
/// tab's terminal. Mirrors `EditorGroupViewModel` from the C# build —
/// flat list, no recursion. The group's identity is its `id` so we can
/// stamp gpui element ids that survive across renders without going
/// through the index (which shifts when a sibling group collapses).
struct Group {
    id: u64,
    tabs: Vec<Tab>,
    /// Local tab index inside this group. Each group has its own
    /// active tab — focusing one group doesn't change another's
    /// selection.
    active_tab: usize,
}

/// Live state for an in-flight splitter drag. Captured at mouse-down
/// on a divider so each subsequent `on_mouse_move` can recompute the
/// new weights from the *original* numbers — re-deriving from the
/// live weights every frame would compound rounding error across the
/// many small moves a real drag generates.
struct SplitterDrag {
    /// Index of the gap being dragged. Resolves to splitting between
    /// `groups[split_idx]` (left) and `groups[split_idx + 1]` (right).
    split_idx: usize,
    /// Cursor X at drag start, in window coords.
    start_x: gpui::Pixels,
    /// Snapshot of weights at drag start, indexed parallel to
    /// `groups`. We only ever modify entries `split_idx` and
    /// `split_idx + 1`; the rest stay at their snapshot values so
    /// neighbouring groups don't shift while we resize.
    start_weights: Vec<f32>,
    /// Pixels of work-area width per unit of weight at drag start.
    /// Set from `(viewport_width - sidebar_width) / total_weight`.
    /// Used to translate cursor delta-x into a weight delta.
    px_per_unit: f32,
}

/// Filesystem path equality after stripping any trailing separator.
/// Case-insensitive on Windows (NTFS / ReFS are case-preserving but
/// case-insensitive by default, so "C:\\Repo" and "c:\\repo" must
/// compare equal); case-sensitive everywhere else (Linux ext4 / APFS
/// in its default case-sensitive mode treat them as distinct paths,
/// and a case-insensitive compare here would mis-route a tab to the
/// wrong project / worktree).
fn path_eq_ci(a: &str, b: &str) -> bool {
    fn norm(s: &str) -> &str {
        s.trim_end_matches(|c| c == '\\' || c == '/')
    }
    let a = norm(a);
    let b = norm(b);
    if cfg!(windows) {
        a.len() == b.len()
            && a.chars()
                .zip(b.chars())
                .all(|(x, y)| x.eq_ignore_ascii_case(&y))
    } else {
        a == b
    }
}

/// `mtime` for the watcher loop's "did the file change?" check.
/// Returns `None` when the file is missing — that lets the loop
/// detect reappearance (e.g. user re-creates `settings.json` after
/// deleting it) the same way it detects an edit.
fn settings_file_mtime(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// Read the saved group weights out of a `LayoutState` snapshot.
/// Wrapped as a helper so the constructor can also fall back to a
/// single 1.0 weight when nothing's saved (first launch, fresh
/// install, deleted layout.json).
fn saved_group_weights(layout: &LayoutState) -> &[f32] {
    layout.group_weights.as_slice()
}

/// Smallest weight we let either side of a drag go to. Below this the
/// pane visibly disappears and the user can't get focus back to it
/// without a Ctrl+Shift+W to remove the empty group. Mirrors C#'s
/// `MinWidth = 200` on the GridSplitter columns at the conceptual
/// level; ours is in weight units rather than pixels because the
/// total-width depends on the viewport.
const MIN_GROUP_WEIGHT: f32 = 0.15;
/// Width of the actual splitter hit-target. Wider than the painted
/// 1px divider so the user can actually grab it without pixel-perfect
/// aiming.
/// Width of the splitter / sidebar-handle / strip-divider hit zone
/// in pixels. The hit zone is wider than the visible line so the
/// user can grab the divider without pixel-perfect aiming. Layout
/// itself uses `DIVIDER_VISUAL_WIDTH`; the extra width comes from
/// an absolute-positioned overlay centered on the visible line that
/// extends `(HIT - VISUAL) / 2` pixels into each adjacent pane.
const SPLITTER_HIT_WIDTH: f32 = 6.0;

/// Visible width of the splitter / sidebar-handle / strip-divider
/// line. Kept at 1 px for a clean, single-pixel rule between panes;
/// the hit area is enlarged to `SPLITTER_HIT_WIDTH` via an absolute-
/// positioned overlay div nested inside the splitter / sidebar-handle
/// element (search for `cursor_col_resize` in `render` /
/// `render_group` to find them — there are no named helpers, the
/// overlays are inlined where they're built). Layout math
/// (`strip_left_pad_w`, alignment between tab-strip dividers and
/// pane splitters, `begin_splitter_drag`'s sidebar-pixels subtraction)
/// uses *this* width so both rows divide the same horizontal extent
/// by the same `weight` factor.
const DIVIDER_VISUAL_WIDTH: f32 = 1.0;

/// Width of the right-side caption-controls cluster in the title
/// bar (split + min + max + close, each 46 px). The rightmost
/// group's tab strip reserves this much space at its right edge so
/// tabs don't slide *under* the absolute-positioned buttons when
/// the user drags a divider hard right.
const CAPTION_CTRLS_W: f32 = 4.0 * 46.0;

/// One on-screen toast — short status notification anchored bottom-
/// right of the window. Mirrors C# `ToastHost` shape (Ok / Err /
/// Info severities, auto-dismiss, top-newest stacking).
#[derive(Clone)]
struct Toast {
    id: u64,
    kind: ToastKind,
    title: SharedString,
    detail: Option<SharedString>,
    expires_at: Instant,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Ok,
    Err,
    #[allow(dead_code)]
    Info,
}

/// How long a toast stays visible before the auto-dismiss task
/// removes it. 4 s is long enough to read a one-liner; longer for
/// errors so the user can copy a path / message before it fades.
const TOAST_LIFETIME_OK: Duration = Duration::from_secs(4);
const TOAST_LIFETIME_INFO: Duration = Duration::from_secs(4);
const TOAST_LIFETIME_ERR: Duration = Duration::from_secs(8);
/// How often the auto-dismiss task wakes up while toasts are
/// visible. 250 ms gives a smooth-enough feel when several land in
/// a burst. When the stack is empty we sleep `TOAST_POLL_IDLE`
/// instead so an idle app doesn't wake at 4 Hz.
const TOAST_POLL: Duration = Duration::from_millis(250);
/// Idle interval when there are no live toasts. The longest
/// possible lifetime (`TOAST_LIFETIME_ERR = 8s`) bounds how soon a
/// freshly-pushed toast can need expiry attention, so polling at
/// the same rate is plenty.
const TOAST_POLL_IDLE: Duration = TOAST_LIFETIME_ERR;
/// Cap on simultaneously-visible toasts. Mirrors the C# build's
/// ToastService visible-cap. When the user fires a flurry of
/// actions (or hits a recurring error) we evict the oldest so the
/// stack doesn't grow without bound.
const TOAST_VISIBLE_CAP: usize = 5;

/// Open right-click menu state for a tab. Identifies the target
/// tab by id (not index) so the menu still hits the right tab if
/// the list mutates between right-click and click. Position is in
/// window coords for `anchored()`.
struct TabMenu {
    group_id: u64,
    tab_id: u64,
    position: gpui::Point<gpui::Pixels>,
}

/// Live state for an in-flight sidebar drag. Same shape as
/// `SplitterDrag` — captured at mouse-down on the sidebar's right
/// edge so we don't keep re-deriving from a moving target.
struct SidebarDrag {
    /// Cursor X at drag start.
    start_x: gpui::Pixels,
    /// Sidebar width at drag start. Each `mouse_move` recomputes
    /// from `start_width + (current_x - start_x)`.
    start_width: f32,
}

/// Payload for a tab drag-drop — the user is moving a tab from one
/// group to another. We carry stable ids (not indices) because
/// `groups` / `tabs` Vecs can mutate while the drag is in flight
/// (closing another tab in another group, splitting, …) and indices
/// would point at the wrong row by the time `on_drop` fires.
#[derive(Clone, Debug)]
struct TabDragData {
    source_group_id: u64,
    source_tab_id: u64,
    /// Snapshotted at drag start. Currently unused — the preview
    /// view captures its own copy in the `on_drag` constructor —
    /// but keeping it on the payload means future drop targets
    /// (e.g. status-bar history rows) can label the dropped tab
    /// without going back through `self.groups`.
    #[allow(dead_code)]
    title: SharedString,
}

/// The little floating "card" the user drags around — gpui needs a
/// `Render` entity to draw the drag image. We make it shape-light
/// so it follows the cursor without lag and matches the active-tab
/// styling so the user sees what they're moving.
struct DraggedTab {
    title: SharedString,
    theme: Arc<Theme>,
}

impl Render for DraggedTab {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .h(px(32.0))
            .min_w(px(140.0))
            .max_w(px(240.0))
            .flex()
            .flex_row()
            .items_center()
            .px_3()
            .gap_2()
            .bg(theme::canvas(&self.theme))
            .border_1()
            .border_color(theme::accent(&self.theme))
            .rounded_md()
            .shadow_lg()
            .text_size(px(13.0))
            .text_color(theme::ink(&self.theme))
            .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(theme::accent(&self.theme)))
            .child(div().flex_grow().truncate().child(self.title.clone()))
    }
}

/// One live telemetry tail for an adopted session. The variant is
/// chosen by the owning tab's [`Tab::agent_id`]; all variants expose
/// the same poll/snapshot surface so [`AppShell::telemetry_for`] can
/// return a uniform `TelemetrySnapshot` regardless of which parser
/// produced it.
///
/// Mirrors the four `*TelemetryService` instances the C#
/// `MainViewModel` keeps as separate fields — Rust collapses them
/// into one heterogeneous map so the discovery / poll plumbing stays
/// agent-agnostic.
enum AgentTail {
    Claude(codescope_core::TranscriptTail),
    Copilot(codescope_core::CopilotTranscriptTail),
    OpenCode(codescope_core::OpenCodeMessageTail),
    Pi(codescope_core::PiTranscriptTail),
}

impl AgentTail {
    fn poll(&mut self) -> bool {
        match self {
            AgentTail::Claude(t) => t.poll(),
            AgentTail::Copilot(t) => t.poll(),
            AgentTail::OpenCode(t) => t.poll(),
            AgentTail::Pi(t) => t.poll(),
        }
    }

    /// Cheap probe used by the busy/idle adaptive cadence in
    /// [`AppShell::start_telemetry_poll`]: returns the latest
    /// `SessionState` without cloning the surrounding snapshot.
    /// `SessionState` is `Copy`, so this is `O(1)` per tail. The
    /// owned-snapshot path goes through [`AgentTail::snapshot`] /
    /// [`AppShell::telemetry_for`].
    fn state(&self) -> Option<codescope_core::SessionState> {
        match self {
            AgentTail::Claude(t) => t.snapshot.as_ref().map(|s| s.state),
            AgentTail::Copilot(t) => t.snapshot.as_ref().map(|s| s.state),
            AgentTail::OpenCode(t) => t.snapshot().map(|s| s.state),
            AgentTail::Pi(t) => t.snapshot.as_ref().map(|s| s.state),
        }
    }

    fn snapshot(&self) -> Option<codescope_core::TelemetrySnapshot> {
        match self {
            AgentTail::Claude(t) => t.snapshot.clone(),
            AgentTail::Copilot(t) => t.snapshot.clone(),
            AgentTail::OpenCode(t) => t.snapshot().cloned(),
            AgentTail::Pi(t) => t.snapshot.clone(),
        }
    }
}

pub struct AppShell {
    /// Flat list of tab groups laid out left-to-right. Always at
    /// least one entry — `close_tab` collapses an emptied group only
    /// when there are siblings to fall back on, and `close_focused_group`
    /// quits the app when invoked on the only remaining group.
    groups: Vec<Group>,
    /// Index into `groups` of the currently-focused column. Keyboard
    /// shortcuts (Ctrl+T, Ctrl+\, Ctrl+W, Ctrl+1..9) target this
    /// group; click on any pane / tab strip section moves the focus.
    focused_group: usize,
    /// Per-group flex weights. Length always matches `groups.len()`.
    /// `split_right` pushes 1.0; `close_tab`'s collapse drops the
    /// matching entry. The render loop maps these to flex_grow values
    /// so a 1.5 / 1.0 split allocates 60% / 40% of the work area.
    ///
    /// Persisted to `layout.json` via `save_layout` after every
    /// drag / split / collapse. **Not yet rehydrated at cold-start**:
    /// the AppShell still spawns a single group on launch so a saved
    /// multi-group layout has nothing to map onto. Restoring the full
    /// shape lands together with session restore.
    group_weights: Vec<f32>,
    /// In-flight splitter drag, if any. `Some` between mouse-down on
    /// a divider and mouse-up. Tracks the gap index (which two
    /// adjacent groups the splitter sits between) plus the cursor
    /// origin and weight snapshot so each `mouse_move` can recompute
    /// from the original numbers — re-deriving from the live weights
    /// would compound rounding error across many small mouse moves.
    splitter_drag: Option<SplitterDrag>,
    /// Current sidebar width. Mirrors `LayoutState::sidebar_width`;
    /// updated live during a drag (clamped between
    /// `SIDEBAR_MIN_WIDTH` and `SIDEBAR_MAX_WIDTH`) and persisted on
    /// drag-end. Read by both `render` (to size the wrapper) and
    /// `begin_splitter_drag` (to compute pixels-per-unit for the
    /// group splitter math).
    sidebar_width: f32,
    /// Sidebar visibility flag. When `false` the sidebar is hidden
    /// and the work area takes the full width; a small expand
    /// caret stays in the titlebar so the user can bring it back.
    sidebar_visible: bool,
    /// In-flight sidebar drag, if any. Same shape as `splitter_drag`
    /// but specific to the sidebar's right edge.
    sidebar_drag: Option<SidebarDrag>,
    /// Open tab right-click menu, if any.
    tab_menu: Option<TabMenu>,
    /// Active toasts. Newest pushed at the front so the floating
    /// stack reads top-to-bottom from the recently-fired action.
    /// Auto-dismissed by the background poller spawned in
    /// `AppShell::new`.
    toasts: std::collections::VecDeque<Toast>,
    /// Monotonic id source for toasts so the auto-dismiss task can
    /// target the exact toast that just expired (otherwise
    /// concurrent pushes could shift indices under the timer).
    next_toast_id: u64,
    /// Threading the on-disk path bundle through so `save_layout` can
    /// reach `paths.layout_file()` without us having to pull it from
    /// the sidebar every time.
    paths: Arc<AppPaths>,
    /// In-memory copy of `layout.json` — kept in sync as group
    /// weights / focus / counts change so a save-on-change writes the
    /// full struct instead of the field we touched.
    layout: LayoutState,
    next_group_id: u64,
    next_tab_id: u64,
    focus_handle: FocusHandle,
    /// Loaded user settings. Cloned into each new tab spawn so a
    /// future "reload settings" can swap this without disturbing
    /// already-running tabs.
    settings: Arc<Settings>,
    /// Active theme — chrome reads from this on every render. Kept
    /// in an `Arc` so swapping themes is a single pointer write.
    theme: Arc<Theme>,
    /// Left rail. Lives behind a feature flag in the layout state —
    /// hidden when the user collapses the sidebar (later).
    sidebar: Entity<Sidebar>,
    /// Timestamp of the most recent left-mouse-down inside any of the
    /// title-bar drag regions. Used to detect titlebar double-clicks
    /// while filtering out the *synthetic* `WM_NCLBUTTONDOWN` event
    /// our own `start_drag` posts: when we `PostMessageW(WM_NCLBUTTONDOWN,
    /// HTCAPTION)` the OS turns it back into a non-client mouse-down
    /// that gpui re-dispatches through our listener stack with
    /// `click_count` already bumped to 2 (gpui's `ClickState` treats
    /// the synthetic press as a continuation of the real click).
    /// That would otherwise toggle maximize on every single click and
    /// no-op on every real double-click. We discriminate by
    /// time-delta from the previous press: synthetic events arrive
    /// in the same message-loop tick (sub-millisecond), real human
    /// double-clicks are 100 ms+ apart. Anything under 10 ms is
    /// treated as the synthetic echo and ignored.
    last_titlebar_press_at: Option<std::time::Instant>,
    /// Persistent notification ring buffer + popover visibility state.
    /// Mirrors `INotificationService` / `NotificationService` from the
    /// C# build.  The bell button (landing in the integrating PR) calls
    /// `notifications.toggle()` and the render calls
    /// `render_notifications_popover` alongside `render_toasts`.
    pub(crate) notifications: crate::notifications::Notifications,
    /// Live telemetry tails, keyed by adopted-agent session id.
    /// Entries are registered via `register_telemetry` (per-agent
    /// dispatch) and polled by the background task spawned in
    /// `AppShell::new`. The accessor `telemetry_for(session_id)`
    /// exposes the latest snapshot without taking a mutable reference.
    /// The map is heterogeneous via [`AgentTail`] so all four agent
    /// backends share one lookup surface — mirrors C#
    /// `MainViewModel.RegisterAgentTelemetry` / `UnregisterAgentTelemetry`
    /// dispatching to four distinct `*TelemetryService` instances.
    telemetry_tails: HashMap<String, AgentTail>,
    /// Window-coordinate bounds of the bell button as recorded by the
    /// `canvas` overlay child during the most recent layout pass.
    /// Used by `render_notifications_popover` to position the popover
    /// above the bell via `gpui::anchored().position(...)`, mirroring
    /// the C# `BellPopup` `PlacementTarget=BellButton`,
    /// `Placement=Top`, `VerticalOffset=-6` rule. `None` until the
    /// status bar has rendered at least once; the popover falls back
    /// to a window-corner anchor in that case so it never renders at
    /// (0, 0). Refreshed every frame, so window resizes / status-bar
    /// reflows track the popover automatically.
    bell_bounds: Option<gpui::Bounds<gpui::Pixels>>,
    /// The last release version we've already announced to the user
    /// this process lifetime. Mirrors `_stagedVersion` in C#
    /// `UpdateService` — keeps the 3-hourly re-poll from re-pushing the
    /// same notification entry on every tick. `None` until the first
    /// poll surfaces an `Available` result.
    last_announced_update: Option<String>,
    /// In-memory snapshot of the on-disk session catalog used by
    /// the session-lifecycle helpers. Initialised at construction
    /// from a [`SessionManager::load_with_sweep`] pass so the
    /// retention sweep runs once per launch (mirrors C#
    /// `SessionStore.LoadAsync`). Each session mutation (open /
    /// soft-close) re-reads from disk first so concurrent sidebar
    /// writes (project / worktree mutations also persist to
    /// `projects.json`) are not clobbered, then applies the change
    /// via [`SessionManager`] and writes back via
    /// [`session::save`]. The Sidebar still owns its own
    /// `ProjectsConfig` clone for project / worktree rendering —
    /// they converge through the shared file. This split mirrors
    /// the C# build where `SessionStore` is the orchestrator and
    /// `SidebarViewModel.StoreSync` projects from it.
    projects: ProjectsConfig,
    /// Registry of agent profiles built from `settings.agents`
    /// overrides (or the shipped built-in defaults when none are
    /// configured). Mirrors C# `AgentRegistry` — owned at the shell
    /// level so the future new-session menu can list agents, pick the
    /// user's preferred default, and look up by id on session restore.
    /// Not yet consumed by any view; held here so the registry is live
    /// from cold-start and the sidebar integration in a follow-up PR
    /// only has to wire the consumer side.
    #[allow(dead_code)]
    agent_registry: codescope_core::AgentRegistry,
    /// Overview-panel visibility. While `true`, the work area
    /// (group strip + terminal grid) is hidden and replaced by the
    /// full-pane [`crate::overview::AppShell::render_overview`]; the
    /// sidebar and status bar stay anchored. Toggled by the sidebar
    /// footer "Overview" button (PR #126), the `Ctrl+Shift+O`
    /// shortcut, and the in-panel "← Back to workspace" link.
    /// Mirrors C# `MainViewModel.IsOverviewVisible`.
    show_overview: bool,
}

impl AppShell {
    pub fn new(
        settings: Arc<Settings>,
        theme: Arc<Theme>,
        projects: ProjectsConfig,
        layout: LayoutState,
        paths: Arc<AppPaths>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        // The sidebar reads sidebar-* fields and selectedProjectId
        // out of the same `LayoutState`; we keep our own clone for
        // the group fields so save-on-change here doesn't trample
        // sidebar writes (and vice versa). Two writers to the same
        // file is acceptable — both go through the same atomic write
        // wrapper, last-writer-wins on order.
        // Hand the sidebar its own clone so session-lifecycle writes
        // on the AppShell side don't have to thread through the
        // sidebar entity. Our `projects` field below keeps a parallel
        // copy used purely for session bookkeeping; both converge
        // through the shared `projects.json`.
        let projects_for_sessions = projects.clone();
        let sidebar = cx.new(|cx| {
            let sidebar =
                Sidebar::new(projects, layout.clone(), theme.clone(), paths.clone());
            // Kick off the per-worktree dirty-state poll. Has to be
            // called from inside the `cx.new` callback because that's
            // where we have a `Context<Sidebar>` to register the
            // background task against. The git-status poll
            // (branch + numstat + ahead/behind) runs alongside it on
            // the same 5 s cadence; both feed the status bar's left
            // cluster and the sidebar dot.
            sidebar.start_dirty_poll(cx);
            sidebar.start_git_status_poll(cx);
            // PR poll runs on a slower (60 s) cadence — `gh pr list`
            // is the heaviest of the three pollers (network + auth)
            // and the data changes much less frequently than the
            // working tree.
            sidebar.start_pr_poll(cx);
            sidebar
        });

        // Spawn a tab whenever the sidebar asks us to — fired by a
        // worktree-row click in the project list and by a successful
        // `submit_new_worktree_dialog`. `subscribe_in` (vs
        // `subscribe`) is the variant that hands us `&mut Window`,
        // which we need so the freshly-spawned terminal can grab
        // focus inline.
        cx.subscribe_in(&sidebar, window, |this, _sidebar, event, window, cx| {
            match event {
                SidebarEvent::OpenSession {
                    working_directory,
                    title,
                    auto_type,
                    force_new,
                } => {
                    // Focus-or-open by default: walk every group's
                    // tabs for one whose `working_directory` matches
                    // and activate it. Only fall through to a fresh
                    // spawn when nothing matches *or* the caller
                    // explicitly asked for `force_new` (the
                    // "New session" / "New Claude session" menu
                    // rows). This is what the user means by
                    // "clicking a worktree shouldn't pile up new
                    // sessions every time".
                    if !*force_new {
                        let mut focus_target: Option<(usize, usize)> = None;
                        for (g_idx, group) in this.groups.iter().enumerate() {
                            for (t_idx, tab) in group.tabs.iter().enumerate() {
                                if tab.working_directory.as_deref() == Some(working_directory.as_path()) {
                                    focus_target = Some((g_idx, t_idx));
                                    break;
                                }
                            }
                            if focus_target.is_some() {
                                break;
                            }
                        }
                        if let Some((g_idx, t_idx)) = focus_target {
                            this.activate_tab(g_idx, t_idx, window, cx);
                            return;
                        }
                    }
                    this.spawn_tab_in(
                        Some(working_directory.clone()),
                        Some(title.clone()),
                        auto_type.clone(),
                        None,
                        window,
                        cx,
                    );
                }
                SidebarEvent::Toast { kind, title, detail } => {
                    let kind = match kind {
                        ToastSeverity::Ok => ToastKind::Ok,
                        ToastSeverity::Err => ToastKind::Err,
                        ToastSeverity::Info => ToastKind::Info,
                    };
                    this.push_toast(kind, title.clone(), detail.clone(), cx);
                }
                SidebarEvent::OpenOverview => {
                    // Toggle the Overview panel — clicking the
                    // sidebar footer button while the panel is open
                    // dismisses it, mirroring the C# build's
                    // `MainViewModel.ToggleOverview` command.
                    let next = !this.show_overview;
                    this.set_show_overview(next, cx);
                }
                SidebarEvent::ReopenSession { session_id } => {
                    this.reopen_session(session_id.clone(), window, cx);
                }
            }
        })
        .detach();
        let pending_window_save: Arc<Mutex<Option<PendingWindowSave>>> = Arc::new(Mutex::new(None));

        // Persist live window geometry. The observer fires for every
        // resize / move tick; we just stash the latest state and let
        // the background debounce task hit disk once the dust settles.
        cx.observe_window_bounds(window, {
            let pending = pending_window_save.clone();
            move |_, window, _| {
                let state = window_state_from_window(window);
                *pending.lock() = Some(PendingWindowSave {
                    state,
                    set_at: Instant::now(),
                });
            }
        })
        .detach();

        // Debounced disk write. The loop dies when AppShell drops
        // (`this.upgrade()` returns None), which is what we want — at
        // app shutdown anything still pending is genuinely stale.
        let pending_for_timer = pending_window_save.clone();
        let paths_for_timer = paths.clone();
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(WINDOW_SAVE_POLL).await;
                if this.upgrade().is_none() {
                    break;
                }
                let to_save = {
                    let mut guard = pending_for_timer.lock();
                    match guard.as_ref() {
                        Some(p) if p.set_at.elapsed() >= WINDOW_SAVE_DEBOUNCE => guard.take(),
                        _ => None,
                    }
                };
                if let Some(p) = to_save
                    && let Err(err) = p.state.save(&paths_for_timer)
                {
                    eprintln!("warning: failed to save window state: {err:#}");
                }
            }
        })
        .detach();

        // Live settings reload. Polls `settings.json`'s mtime every
        // `SETTINGS_POLL` and triggers `apply_settings` on a real
        // change. Mirrors C# `SettingsStore`'s file-watcher
        // behaviour without the `notify` crate dependency — a single
        // file at human typing speed doesn't need OS-level events.
        let settings_path = paths.settings_file();
        let initial_mtime = settings_file_mtime(&settings_path);
        cx.spawn(async move |this, cx| {
            let mut last_mtime = initial_mtime;
            loop {
                cx.background_executor().timer(SETTINGS_POLL).await;
                if this.upgrade().is_none() {
                    break;
                }
                let current_mtime = settings_file_mtime(&settings_path);
                if current_mtime == last_mtime {
                    continue;
                }
                let settings = match Settings::load_from(&settings_path) {
                    Ok(s) => s,
                    Err(err) => {
                        // A half-written save mid-edit is the common
                        // case — `Settings::load_from` returns an
                        // error, we log and try again on the next
                        // tick. **Do not** advance `last_mtime` here:
                        // if we did, a transient parse failure would
                        // leave us stuck on the old settings until
                        // the user touched the file again. Leaving
                        // it at the old value means the next tick
                        // sees the same "newer" mtime and retries
                        // until the editor finishes its atomic
                        // write.
                        eprintln!("warning: failed to reload settings: {err:#}");
                        continue;
                    }
                };
                last_mtime = current_mtime;
                let _ = this.update(cx, |this, cx| {
                    this.apply_settings(settings, cx);
                });
            }
        })
        .detach();

        // Toast auto-dismiss loop. While toasts are visible we wake
        // every `TOAST_POLL` to retire expired entries; when the
        // stack is empty we drop down to `TOAST_POLL_IDLE` (the
        // longest possible lifetime) so an idle app barely wakes
        // for this. Keeps the floating stack auto-clearing without
        // burning watts when there's nothing on screen.
        //
        // **Timer first, update second.** Calling `this.update(...)`
        // as the very first thing in a freshly-spawned foreground
        // task can race the in-flight startup borrow (window open
        // + first render path) and trip a `RefCell already
        // borrowed` panic. Sleeping for `TOAST_POLL_IDLE` up front
        // gets us safely past construction; subsequent ticks then
        // do `update → check → adjust interval → timer → update`
        // in that order so we never `update` immediately after
        // entering the loop body.
        cx.spawn(async move |this, cx| {
            let mut interval = TOAST_POLL_IDLE;
            loop {
                cx.background_executor().timer(interval).await;
                if this.upgrade().is_none() {
                    break;
                }
                let result = this.update(cx, |this, cx| {
                    let now = Instant::now();
                    let before = this.toasts.len();
                    this.toasts.retain(|t| t.expires_at > now);
                    if this.toasts.len() != before {
                        cx.notify();
                    }
                    if this.toasts.is_empty() {
                        TOAST_POLL_IDLE
                    } else {
                        TOAST_POLL
                    }
                });
                match result {
                    Ok(next) => interval = next,
                    Err(_) => break,
                }
            }
        })
        .detach();

        // Restore sidebar geometry from layout. Width is clamped on
        // load too — a corrupt or hand-edited layout.json could
        // otherwise pin the sidebar at 0 or eat the whole window.
        let sidebar_width = if layout.sidebar_width > 0.0 {
            layout
                .sidebar_width
                .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH)
        } else {
            SIDEBAR_DEFAULT_WIDTH
        };
        let sidebar_visible = layout.sidebar_visible;

        // Rehydrate group layout from `layout.json`. We can restore
        // the *shape* (column count + weights + focused index) even
        // though session restore — which would refill each group's
        // tab list — hasn't landed yet. So a saved 1.5/1.0 split
        // comes back as two empty columns and the cold-start tab
        // lands in whichever column was last focused. Empty siblings
        // show the "press Ctrl+Shift+T" placeholder; the user can
        // close them with Ctrl+Shift+W if they don't want them.
        //
        // Sanitise on the way in: drop any non-finite, non-positive,
        // wildly large, or invisibly small weights. A `<= 0.0` weight
        // would zero out its column with `flex-grow`, hiding the
        // pane with no way to recover via the UI; a tiny weight like
        // `1e-9` is the same problem (column collapses to a sub-
        // pixel slice that the user can't grab). Clamp to
        // `MIN_GROUP_WEIGHT` so any persisted weight maps to a
        // visible, draggable column.
        let mut sanitized_weights: Vec<f32> = saved_group_weights(&layout)
            .iter()
            .filter(|w| w.is_finite() && **w >= MIN_GROUP_WEIGHT && **w < 1000.0)
            .copied()
            .collect();
        if sanitized_weights.is_empty() {
            sanitized_weights.push(1.0);
        }
        let group_count = sanitized_weights.len();
        let groups: Vec<Group> = (0..group_count)
            .map(|idx| Group {
                id: idx as u64,
                tabs: Vec::new(),
                active_tab: 0,
            })
            .collect();
        // Saved focus may be stale — clamp it into range so a config
        // that lost groups since save still lands somewhere live.
        let focused_group = layout
            .focused_group_index
            .min(groups.len().saturating_sub(1));

        // Build the agent registry up-front from the loaded settings so
        // user overrides in `settings.agents` are honoured, otherwise
        // the shipped 5-agent defaults (claude/codex/opencode/copilot/pi)
        // are used. Cheap — just a `Vec<AgentProfile>` clone.
        let agent_registry = codescope_core::AgentRegistry::from_settings(&settings);

        let mut shell = Self {
            groups,
            focused_group,
            group_weights: sanitized_weights,
            splitter_drag: None,
            sidebar_width,
            sidebar_visible,
            sidebar_drag: None,
            tab_menu: None,
            toasts: std::collections::VecDeque::new(),
            next_toast_id: 0,
            paths: paths.clone(),
            layout,
            next_group_id: group_count as u64,
            next_tab_id: 0,
            focus_handle,
            settings,
            theme,
            sidebar,
            last_titlebar_press_at: None,
            notifications: crate::notifications::Notifications::new(),
            telemetry_tails: HashMap::new(),
            bell_bounds: None,
            last_announced_update: None,
            projects: projects_for_sessions,
            agent_registry,
            show_overview: false,
        };
        shell.start_telemetry_poll(cx);
        shell.start_agent_discovery_poll(cx);
        shell.start_update_check_poll(cx);
        shell.rehydrate_or_cold_start(window, cx);
        shell
    }

    /// Persist the current group layout (weights + focus index) to
    /// `layout.json`. Called after splitter-drag end, split-right, and
    /// group-collapse — anything that mutates either field. Never
    /// fails fatally; logs and moves on, the next save will retry.
    /// Persist this AppShell's layout fields to `layout.json`. After
    /// PR #75 the AppShell owns `group_weights`, `focused_group_index`,
    /// `sidebar_visible`, and `sidebar_width` (sidebar geometry
    /// moved here so the Sidebar entity doesn't have to know about
    /// its own chrome). The Sidebar still owns `selected_project_id`.
    ///
    /// Reads the on-disk state first and only overwrites the fields
    /// we own — a naive write of our cached `self.layout` would
    /// clobber any `selected_project_id` the user changed since the
    /// last reload, so we re-read on every save to avoid the
    /// last-writer-wins data loss the reviewer flagged in #73.
    fn save_layout(&mut self) {
        let mut on_disk = match LayoutState::load(self.paths.as_ref()) {
            Ok(state) => state,
            Err(err) => {
                eprintln!(
                    "warning: failed to read layout.json before save \
                     (using in-memory copy as base): {err:#}"
                );
                self.layout.clone()
            }
        };
        on_disk.group_weights = self.group_weights.clone();
        on_disk.focused_group_index = self.focused_group;
        on_disk.sidebar_visible = self.sidebar_visible;
        on_disk.sidebar_width = self.sidebar_width;
        on_disk.open_tabs = self.snapshot_open_tabs();
        if let Err(err) = on_disk.save(self.paths.as_ref()) {
            eprintln!("warning: failed to save layout.json: {err:#}");
            return;
        }
        self.layout = on_disk;
    }

    // -----------------------------------------------------------------------
    // Claude telemetry
    // -----------------------------------------------------------------------

    /// Register a transcript tail for `session_id` under
    /// `working_directory`, dispatching by `agent_id`. Safe to call
    /// multiple times with the same id — the old tail is replaced
    /// (e.g. after a Claude session resume that kept the same id).
    ///
    /// Mirrors `MainViewModel.RegisterAgentTelemetry` in the C#
    /// build — branches by agent id, no-ops cleanly for unknown
    /// backends or when the per-agent root can't be resolved.
    pub fn register_telemetry(
        &mut self,
        agent_id: codescope_core::AgentId,
        session_id: String,
        working_directory: &str,
    ) {
        let tail = match agent_id {
            codescope_core::AgentId::Claude => {
                let Some(root) = Self::claude_projects_root() else {
                    eprintln!(
                        "[telemetry] no USERPROFILE / HOME — skipping claude registration for {session_id}"
                    );
                    return;
                };
                AgentTail::Claude(codescope_core::TranscriptTail::for_session(
                    &root,
                    working_directory,
                    &session_id,
                ))
            }
            codescope_core::AgentId::Copilot => {
                let Some(root) = codescope_core::copilot_telemetry::default_session_state_root()
                else {
                    eprintln!(
                        "[telemetry] no USERPROFILE / HOME — skipping copilot registration for {session_id}"
                    );
                    return;
                };
                AgentTail::Copilot(codescope_core::CopilotTranscriptTail::for_session(
                    &root,
                    &session_id,
                ))
            }
            codescope_core::AgentId::OpenCode => {
                let Some(root) = codescope_core::opencode_telemetry::default_data_root() else {
                    eprintln!(
                        "[telemetry] no USERPROFILE / HOME — skipping opencode registration for {session_id}"
                    );
                    return;
                };
                AgentTail::OpenCode(codescope_core::OpenCodeMessageTail::new(
                    root,
                    session_id.clone(),
                ))
            }
            codescope_core::AgentId::Codex => {
                // Codex telemetry isn't wired in the Rust port yet (the
                // AgentRegistry entry exists ahead of the discovery
                // layer); skip registration so the rest of the agent
                // surface keeps working until the dedicated Codex
                // discovery / telemetry modules land.
                eprintln!(
                    "[telemetry] codex telemetry not yet wired — skipping registration for {session_id}"
                );
                return;
            }
            codescope_core::AgentId::Pi => {
                let Some(root) = codescope_core::pi_telemetry::default_sessions_root() else {
                    eprintln!(
                        "[telemetry] no USERPROFILE / HOME — skipping pi registration for {session_id}"
                    );
                    return;
                };
                // Pi locates the transcript by suffix-matching
                // `*_<sid>.jsonl`. The file may not exist yet on the
                // first call (the discovery loop racing the agent
                // starting up); skip the registration in that case
                // and let the next discovery tick retry.
                let Some(tail) = codescope_core::PiTranscriptTail::for_session(&root, &session_id)
                else {
                    eprintln!(
                        "[telemetry] pi transcript not yet on disk — skipping pi registration for {session_id}"
                    );
                    return;
                };
                AgentTail::Pi(tail)
            }
        };
        self.telemetry_tails.insert(session_id, tail);
    }

    /// Remove a tail and drop its snapshot. Mirrors `UnregisterAgentTelemetry`
    /// in the C# build — agent-agnostic (we drop by id, not by type).
    pub fn unregister_telemetry(&mut self, session_id: &str) {
        self.telemetry_tails.remove(session_id);
    }

    /// Return the latest telemetry snapshot for `session_id`, or `None`
    /// when the session has not been registered or has not yet produced
    /// any parseable entries.
    ///
    /// Agent-agnostic: the heterogeneous [`AgentTail`] enum normalises
    /// every parser onto the shared
    /// [`codescope_core::TelemetrySnapshot`] shape, so callers (e.g.
    /// `render_status_bar`) don't need to know which backend produced
    /// the data.
    pub fn telemetry_for(&self, session_id: &str) -> Option<codescope_core::TelemetrySnapshot> {
        self.telemetry_tails.get(session_id).and_then(|t| t.snapshot())
    }

    /// Spawn the background transcript-tail polling loop.
    ///
    /// Uses an adaptive interval: 250 ms while any session is busy /
    /// pending-tool-use; 2 s when all sessions are idle or unknown.
    /// Mirrors the C# `ClaudeTelemetryService` 250 ms poll (the C#
    /// build also uses FSWatcher; here we rely on polling only to avoid
    /// adding the `notify` crate dependency).
    ///
    /// Called from `AppShell::new` after the struct is constructed.
    fn start_telemetry_poll(&self, cx: &mut Context<Self>) {
        // Adaptive cadence — three rates so we don't burn CPU when
        // there's nothing to read:
        //
        // - 250 ms while any registered tail is in `Busy` /
        //   `PendingToolUse` (assistant streaming).
        // - 2 s while at least one tail is registered but every
        //   snapshot is idle.
        // - 30 s when there are no tails at all (the "armed-only-
        //   when-needed" pattern from the C# `RefreshTimerArmed`
        //   model — we don't fully tear down the task to keep the
        //   spawn site simple, but we stop hammering the executor).
        //
        // The first tick fires after construction is done (avoids the
        // borrow-at-construction race that `start_dirty_poll` also
        // guards against).
        cx.spawn(async move |this, cx| {
            let mut interval = Duration::from_secs(2);
            loop {
                cx.background_executor().timer(interval).await;
                if this.upgrade().is_none() {
                    break;
                }
                let result = this.update(cx, |this, cx| {
                    if this.telemetry_tails.is_empty() {
                        // Even with zero tails, the sidebar might
                        // still be holding stale busy/active sets
                        // from a session that just closed. Push
                        // empties so the dots fade back to `rest`.
                        this.push_sidebar_session_paths(cx);
                        return Duration::from_secs(30);
                    }
                    let mut any_busy = false;
                    for tail in this.telemetry_tails.values_mut() {
                        tail.poll();
                        if matches!(
                            tail.state(),
                            Some(codescope_core::SessionState::Busy)
                                | Some(codescope_core::SessionState::PendingToolUse)
                        ) {
                            any_busy = true;
                        }
                    }
                    // After every poll, recompute the per-path
                    // active/busy snapshot the sidebar uses to colour
                    // its worktree dots and propagate "agent busy" up
                    // to a collapsed project row. Cheap — one map
                    // lookup per tab; the sidebar `set_session_paths`
                    // call short-circuits with no notify when nothing
                    // changed, so a 250 ms busy cadence doesn't drive
                    // a redraw every tick unless a tab actually
                    // flipped state.
                    this.push_sidebar_session_paths(cx);
                    if any_busy {
                        Duration::from_millis(250)
                    } else {
                        Duration::from_secs(2)
                    }
                });
                match result {
                    Ok(next) => interval = next,
                    Err(_) => break,
                }
            }
        })
        .detach();
    }

    /// Resolve `~/.claude/projects/`. Returns `None` when neither
    /// USERPROFILE nor HOME is set — same gate `register_telemetry`
    /// uses, kept here so the discovery poll skips silently instead
    /// of repeatedly logging.
    fn claude_projects_root() -> Option<std::path::PathBuf> {
        let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
        Some(std::path::PathBuf::from(home).join(".claude").join("projects"))
    }

    /// Spawn the background per-tab agent session adoption loop.
    ///
    /// Mirrors C# `MainViewModel.BeginAgentAdoption`: every tick, scan
    /// each agent's filesystem layout for transcripts whose
    /// `created_at` or `last_modified` is at or after the tab's
    /// `spawned_at`, then dedupe per-tab via `fired_session_ids` and
    /// register the corresponding telemetry tail. Each tab's
    /// `agent_id` selects which discovery scan runs:
    ///
    /// * Claude — `~/.claude/projects/<encoded-cwd>/<sid>.jsonl`
    /// * Pi      — `~/.pi/agent/sessions/.../*_<sid>.jsonl` (recursive)
    /// * OpenCode — `~/.local/share/opencode/.../message/<sid>/msg_*.json`
    /// * Copilot  — `~/.copilot/session-state/<sid>/`
    ///
    /// All four scans match by canonicalised cwd (Claude uses an
    /// encoded-cwd directory; the others peek at a header field) so
    /// transcripts from a different workspace don't get adopted into
    /// the wrong tab.
    ///
    /// The watch stays armed for the tab's full lifetime so Claude's
    /// `/clear` rotations and Pi's per-invocation session ids both
    /// flow through cleanly. Tabs without a recognised `agent_id`
    /// (plain shell, unknown command) are skipped.
    ///
    /// Cadence: the fastest of the four agent-specific intervals
    /// (350 ms — Claude / Pi / Copilot all match) while any agent
    /// tab is alive, dropping to 5 s when none are.
    ///
    /// Called from `AppShell::new` after the struct is constructed.
    fn start_agent_discovery_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let active = Duration::from_millis(codescope_core::CLAUDE_DISCOVERY_POLL_MS);
            let idle = Duration::from_secs(5);
            let mut interval = active;
            loop {
                cx.background_executor().timer(interval).await;
                if this.upgrade().is_none() {
                    break;
                }
                let result = this.update(cx, |this, _cx| {
                    // Two-pass scan/mutate so we can't deadlock on
                    // `register_telemetry`'s `&mut self` borrow:
                    // collect candidates first, then perform the
                    // register / un-register dance.
                    struct Found {
                        group_idx: usize,
                        tab_idx: usize,
                        agent_id: codescope_core::AgentId,
                        previous_session_id: Option<String>,
                        new_session_id: String,
                        working_directory: String,
                    }
                    let mut found = Vec::new();
                    let mut any_active = false;
                    for (g_idx, group) in this.groups.iter().enumerate() {
                        for (t_idx, tab) in group.tabs.iter().enumerate() {
                            let Some(agent_id) = tab.agent_id else { continue };
                            let Some(ref wd) = tab.working_directory else { continue };
                            // Codex has no discovery wired in the Rust
                            // port yet — skip before flipping
                            // `any_active`, otherwise an all-Codex
                            // workspace would pin the poll at the
                            // 350 ms active rate while the loop does
                            // no work. Drop it through with the same
                            // "no agent" handling so the cadence
                            // relaxes to the 5 s idle interval.
                            if matches!(agent_id, codescope_core::AgentId::Codex) {
                                continue;
                            }
                            any_active = true;
                            let wd_str = wd.to_string_lossy().into_owned();
                            // Each agent scan returns
                            // `(session_id, mtime_source_path)` so we
                            // can pick the newest unseen candidate per
                            // tab — the "/clear rotation supplants the
                            // previous id" rule from PR #98 still
                            // applies for Claude, and the same logic
                            // covers Pi's per-invocation ids.
                            let hits: Vec<(String, std::path::PathBuf)> = match agent_id {
                                codescope_core::AgentId::Claude => {
                                    let Some(root) = Self::claude_projects_root() else {
                                        continue;
                                    };
                                    codescope_core::claude_discovery::scan(
                                        &root, &wd_str, tab.spawned_at,
                                    )
                                    .into_iter()
                                    .map(|c| (c.session_id, c.path))
                                    .collect()
                                }
                                codescope_core::AgentId::Pi => {
                                    let Some(root) =
                                        codescope_core::pi_telemetry::default_sessions_root()
                                    else {
                                        continue;
                                    };
                                    codescope_core::pi_discovery::scan(
                                        &root, &wd_str, tab.spawned_at,
                                    )
                                    .into_iter()
                                    .map(|c| (c.session_id, c.path))
                                    .collect()
                                }
                                codescope_core::AgentId::OpenCode => {
                                    let Some(root) =
                                        codescope_core::opencode_telemetry::default_data_root()
                                    else {
                                        continue;
                                    };
                                    codescope_core::opencode_discovery::scan(
                                        &root, &wd_str, tab.spawned_at,
                                    )
                                    .into_iter()
                                    .map(|c| (c.session_id, c.message_path))
                                    .collect()
                                }
                                codescope_core::AgentId::Copilot => {
                                    let Some(root) =
                                        codescope_core::copilot_telemetry::default_session_state_root()
                                    else {
                                        continue;
                                    };
                                    codescope_core::copilot_discovery::scan(
                                        &root, &wd_str, tab.spawned_at,
                                    )
                                    .into_iter()
                                    .map(|c| (c.session_id, c.session_dir))
                                    .collect()
                                }
                                codescope_core::AgentId::Codex => {
                                    // Unreachable: short-circuited
                                    // above so the active-poll rate
                                    // doesn't stay pinned for an
                                    // all-Codex workspace. Kept as a
                                    // belt-and-braces fallback in case
                                    // someone removes the early skip.
                                    continue;
                                }
                            };
                            let mut newest: Option<(SystemTime, String)> = None;
                            for (sid, path) in hits {
                                if tab.fired_session_ids.contains(&sid) {
                                    continue;
                                }
                                let mtime = std::fs::metadata(&path)
                                    .ok()
                                    .and_then(|m| m.modified().ok())
                                    .unwrap_or(SystemTime::UNIX_EPOCH);
                                let take = newest
                                    .as_ref()
                                    .map(|(prev, _)| mtime >= *prev)
                                    .unwrap_or(true);
                                if take {
                                    newest = Some((mtime, sid));
                                }
                            }
                            if let Some((_, sid)) = newest {
                                found.push(Found {
                                    group_idx: g_idx,
                                    tab_idx: t_idx,
                                    agent_id,
                                    previous_session_id: tab.adopted_session_id.clone(),
                                    new_session_id: sid,
                                    working_directory: wd_str,
                                });
                            }
                        }
                    }
                    for f in found {
                        if let Some(prev) = f.previous_session_id.as_deref() {
                            if prev != f.new_session_id {
                                this.unregister_telemetry(prev);
                            }
                        }
                        this.register_telemetry(
                            f.agent_id,
                            f.new_session_id.clone(),
                            &f.working_directory,
                        );
                        if let Some(group) = this.groups.get_mut(f.group_idx) {
                            if let Some(tab) = group.tabs.get_mut(f.tab_idx) {
                                tab.adopted_session_id = Some(f.new_session_id.clone());
                                tab.fired_session_ids.insert(f.new_session_id);
                            }
                        }
                    }
                    if any_active { active } else { idle }
                });
                match result {
                    Ok(next) => interval = next,
                    Err(_) => break,
                }
            }
        })
        .detach();
    }

    /// Spawn the GitHub release polling loop — Velopack parity for the
    /// Rust port.
    ///
    /// Runs once after `update_check::INITIAL_DELAY` (10 s) and every
    /// `update_check::POLL_INTERVAL` (3 h) thereafter — the same
    /// cadence the C# `App.xaml.cs` uses for `UpdateService.CheckAsync`.
    /// Network work is dispatched through `cx.background_executor()`
    /// so the UI thread never sees a blocking `ureq::call`. On
    /// `UpdateStatus::Available` we push a single `Generic`
    /// notification per unique version per process lifetime; the C#
    /// build's `_stagedVersion` field plays the same role.
    ///
    /// Skipped entirely under `CODESCOPE_DEV=1` — mirrors C#
    /// `UpdateService.CheckAsync`'s `IsDevMode` early return.
    fn start_update_check_poll(&self, cx: &mut Context<Self>) {
        let paths = self.paths.clone();
        if !codescope_core::update_check::should_poll(&paths) {
            return;
        }
        cx.spawn(async move |this, cx| {
            // Initial 10 s delay — keeps the first network call off
            // the startup-critical path.
            cx.background_executor()
                .timer(codescope_core::update_check::INITIAL_DELAY)
                .await;
            loop {
                if this.upgrade().is_none() {
                    break;
                }
                let status = cx
                    .background_executor()
                    .spawn(async move {
                        codescope_core::update_check::check_once(env!(
                            "CODESCOPE_VERSION_DISPLAY"
                        ))
                    })
                    .await;
                if this.upgrade().is_none() {
                    break;
                }
                if let codescope_core::update_check::UpdateStatus::Available {
                    version,
                    url,
                    body: _,
                } = status
                {
                    let _ = this.update(cx, |this, cx| {
                        // Suppress duplicate announcements for the
                        // same version this process lifetime, the
                        // way C# `_stagedVersion` does.
                        if this
                            .last_announced_update
                            .as_deref()
                            .map(|v| v == version)
                            .unwrap_or(false)
                        {
                            return;
                        }
                        this.last_announced_update = Some(version.clone());
                        let title: SharedString =
                            format!("CodeScope {version} available").into();
                        let detail: SharedString =
                            format!("A newer release is published. {url}").into();
                        this.push_notification(
                            crate::notifications::NotificationKind::Generic,
                            title,
                            detail,
                            None,
                            cx,
                        );
                    });
                }
                cx.background_executor()
                    .timer(codescope_core::update_check::POLL_INTERVAL)
                    .await;
            }
        })
        .detach();
    }

    // -----------------------------------------------------------------------
    // Layout persistence
    // -----------------------------------------------------------------------

    /// Build a `RestoreTab` for every currently-open tab. Tabs
    /// without a known working directory are skipped — we'd have
    /// nothing useful to spawn them with on rehydrate.
    fn snapshot_open_tabs(&self) -> Vec<codescope_core::RestoreTab> {
        let mut out = Vec::new();
        for (g_idx, group) in self.groups.iter().enumerate() {
            for (t_idx, tab) in group.tabs.iter().enumerate() {
                let Some(ref wd) = tab.working_directory else { continue };
                out.push(codescope_core::RestoreTab {
                    working_directory: wd.to_string_lossy().into_owned(),
                    title: tab.title.to_string(),
                    auto_type: tab.auto_type.as_ref().map(|s| s.to_string()),
                    group_index: g_idx,
                    active_in_group: t_idx == group.active_tab,
                });
            }
        }
        out
    }

    /// Restore tabs from `LayoutState::open_tabs`, or fall back to
    /// the cold-start spawn when nothing's saved. Builds on PR #74's
    /// group-shape rehydration: by the time this runs the AppShell
    /// already has N empty groups; we just put a tab back into each.
    /// Tabs whose working directory no longer exists are silently
    /// dropped; if everything is dropped (rare) we cold-start so the
    /// user isn't left staring at empty groups.
    fn rehydrate_or_cold_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let saved: Vec<codescope_core::RestoreTab> = self.layout.open_tabs.clone();
        if saved.is_empty() {
            self.spawn_tab(window, cx);
            return;
        }
        let group_count = self.groups.len();
        let mut active_by_group: Vec<Option<usize>> = vec![None; group_count];
        let mut spawned_any = false;
        for tab in saved.into_iter() {
            let path = std::path::PathBuf::from(&tab.working_directory);
            if !path.exists() {
                eprintln!(
                    "info: skipping restored tab — path no longer exists: {}",
                    tab.working_directory
                );
                continue;
            }
            let group_idx = tab.group_index.min(group_count.saturating_sub(1));
            // `spawn_tab_in` always lands in the focused group, so
            // we move focus first then restore the saved focus
            // index after the loop.
            self.focused_group = group_idx;
            let title = SharedString::from(tab.title);
            let auto = tab.auto_type.map(SharedString::from);
            // Rehydrate path lets `spawn_tab_in` mint a fresh session
            // id and append a new row through `SessionManager::open`,
            // even though the tab is logically "the same" tab the
            // user closed in the previous launch. This is a known
            // limitation: `LayoutState::open_tabs` does not yet carry
            // the persisted `Session.id`, so we can't map back to the
            // stored row. The follow-up that unifies the rehydrate
            // path with `SessionManager::live` will tighten this up;
            // until then, accept the duplicate row as the cost of
            // landing the lifecycle plumbing without a coordinated
            // schema change.
            self.spawn_tab_in(Some(path), Some(title), auto, None, window, cx);
            spawned_any = true;
            if tab.active_in_group {
                let new_idx = self.groups[group_idx].tabs.len() - 1;
                active_by_group[group_idx] = Some(new_idx);
            }
        }
        if !spawned_any {
            // Every saved path was missing — fall back to cold-start
            // so the user gets *something*.
            self.spawn_tab(window, cx);
            return;
        }
        // Apply per-group active-tab selections.
        for (g_idx, active) in active_by_group.iter().enumerate() {
            if let Some(t_idx) = active
                && let Some(group) = self.groups.get_mut(g_idx)
                && *t_idx < group.tabs.len()
            {
                group.active_tab = *t_idx;
            }
        }
        // Re-focus the saved focused group's active tab.
        let focused = self
            .layout
            .focused_group_index
            .min(self.groups.len().saturating_sub(1));
        if !self.groups[focused].tabs.is_empty() {
            let active = self.groups[focused].active_tab;
            self.activate_tab(focused, active, window, cx);
        }
    }

    // -----------------------------------------------------------------------
    // Session lifecycle (Big-Step-2 — Tab ↔ SessionManager plumbing)
    //
    // These helpers funnel every Tab create / close through
    // [`SessionManager`] so the on-disk session list stays in sync with
    // what the user actually has open. The Sidebar still owns project
    // and worktree mutations on its side; both writers go through the
    // shared `projects.json`. Mirrors the C# split between
    // `MainViewModel.NewSessionAsync` / `CloseTabAsync` and
    // `SidebarViewModel.StoreSync`.
    // -----------------------------------------------------------------------

    /// Find the project that owns `working_directory`, if any.
    /// Match order mirrors C# `MainViewModel`: a worktree path match
    /// wins over a project-root path match, and case-insensitive
    /// comparison handles Windows path quirks. Returns `(project_id,
    /// worktree_id)` so the caller can stamp both fields on the
    /// `Session` row exactly the way the C# build does.
    fn locate_project_for_path(&self, working_directory: &std::path::Path) -> Option<(String, Option<String>)> {
        let target = working_directory.to_string_lossy();
        for project in &self.projects.projects {
            for wt in &project.worktrees {
                if path_eq_ci(&wt.path, &target) {
                    return Some((project.id.clone(), Some(wt.id.clone())));
                }
            }
            if path_eq_ci(&project.path, &target) {
                // Project root that has a primary worktree row will
                // already have hit the loop above; the fall-through
                // here covers legacy / partially-migrated configs.
                let primary_id = project
                    .worktrees
                    .iter()
                    .find(|w| w.is_primary)
                    .map(|w| w.id.clone());
                return Some((project.id.clone(), primary_id));
            }
        }
        None
    }

    /// Allocate a session id for a freshly-spawned tab.
    ///
    /// Three modes:
    /// 1. `restore_session_id = Some` → caller is rehydrating a
    ///    persisted live session row; use that id verbatim. The row
    ///    already exists on disk so we don't append a new one.
    /// 2. The `working_directory` maps to a known project → mint a
    ///    fresh uuid, build a [`Session`], reload from disk so any
    ///    sidebar writes since construction aren't clobbered, append
    ///    the row via [`SessionManager::open`], persist.
    /// 3. No project owns this path → return a free-floating uuid
    ///    without persisting. The Tab still works as a terminal; the
    ///    session simply isn't tracked in `projects.json`. Matches C#
    ///    where `MainViewModel.NewSessionAsync` returns early when
    ///    `project is null`, except we don't want to refuse the spawn —
    ///    cold-start before any project is added still gets a Tab.
    fn allocate_session_id(
        &mut self,
        working_directory: Option<&std::path::Path>,
        restore_session_id: Option<String>,
    ) -> String {
        if let Some(id) = restore_session_id {
            return id;
        }
        let new_id = uuid::Uuid::new_v4().to_string();
        let Some(wd) = working_directory else {
            return new_id;
        };
        // Reload from disk *before* the project lookup — the sidebar
        // may have added or removed a project / worktree since our
        // last snapshot. Looking up against a stale in-memory copy
        // would miss a freshly-added worktree (no persistence) or
        // stamp the session with a stale `worktree_id` that the
        // sidebar has since deleted. Mirrors C# `SessionStore`'s
        // read-then-mutate pattern (it owns the in-memory list under
        // a lock; we don't, so re-reading is the cheapest equivalent).
        match ProjectsConfig::load(&self.paths) {
            Ok(cfg) => {
                self.projects = cfg;
            }
            Err(err) => {
                eprintln!("warning: failed to reload projects.json before SessionManager::open: {err:#}");
            }
        }
        let Some((project_id, worktree_id)) = self.locate_project_for_path(wd) else {
            return new_id;
        };
        let session = Session {
            id: new_id.clone(),
            worktree_path: wd.to_string_lossy().into_owned(),
            branch: None,
            agent_id: None,
            display_name: None,
            worktree_id,
            last_opened: None,
            agent_session_id: None,
            closed_at: None,
        };
        match SessionManager::open(&mut self.projects, &project_id, session, &now_iso8601()) {
            Ok(_) => {
                if let Err(err) = codescope_core::session::save(&self.projects, &self.paths) {
                    eprintln!("warning: failed to persist session open: {err:#}");
                }
            }
            Err(err) => {
                eprintln!("warning: SessionManager::open rejected new session: {err:#}");
            }
        }
        new_id
    }

    /// Mark `session_id` as soft-closed and persist. Called from
    /// `close_tab` so every Tab close lands in the closed-history
    /// list. Mirrors C# `SessionStore.SoftCloseSessionAsync`.
    /// Best-effort: a free-floating tab whose session id is not in
    /// `projects.json` (path matched no project at spawn time) is a
    /// silent no-op rather than an error. Reload-then-mutate-then-save
    /// keeps us in sync with concurrent sidebar writes.
    fn soft_close_session(&mut self, session_id: &str) {
        match ProjectsConfig::load(&self.paths) {
            Ok(cfg) => {
                self.projects = cfg;
            }
            Err(err) => {
                eprintln!("warning: failed to reload projects.json before soft_close: {err:#}");
            }
        }
        match SessionManager::soft_close(&mut self.projects, session_id, &now_iso8601()) {
            Ok(_pruned) => {
                if let Err(err) = codescope_core::session::save(&self.projects, &self.paths) {
                    eprintln!("warning: failed to persist session soft-close: {err:#}");
                }
            }
            Err(_) => {
                // Free-floating session (no project context at spawn
                // time, or row already removed by the sidebar). Drop
                // silently — mirrors C#'s "session id not found is
                // ok" branch in `CloseTabAsync` where `storedForTab`
                // is null.
            }
        }
    }

    /// Reopen the soft-closed session `session_id`: clear `closed_at`,
    /// stamp `last_opened`, persist, mirror the updated config to the
    /// sidebar so the row leaves the history list, then spawn a tab
    /// pinned to the persisted `worktree_path` and (where applicable)
    /// the persisted `agent_id`'s auto-type command. Mirrors C#
    /// `MainViewModel.ReopenClosedSessionAsync`.
    ///
    /// Best-effort: a `session_id` not found on disk (race with a
    /// retention sweep, or stale event from a closed sidebar that has
    /// since rebuilt) is logged and swallowed — there's nothing useful
    /// to spawn at that point.
    pub(crate) fn reopen_session(
        &mut self,
        session_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Reload-then-mutate-then-save mirrors the pattern around
        // `allocate_session_id` / `soft_close_session`: a sidebar
        // write between two of ours can otherwise clobber the
        // sessions array. Bail on load failure rather than mutating
        // a stale snapshot and persisting it on top of newer disk
        // state — surfaces the failure as a toast so the user sees
        // the row stay in the history disclosure instead of silently
        // racing the on-disk file.
        match ProjectsConfig::load(&self.paths) {
            Ok(cfg) => {
                self.projects = cfg;
            }
            Err(err) => {
                eprintln!("warning: failed to reload projects.json before reopen: {err:#}");
                // `{err:#}` matches the rest of the toast surface
                // (e.g. the agent-launch error path) — pretty-formats
                // the full anyhow error chain so the user sees the
                // root cause, not just the outer "could not read"
                // wrapper.
                self.push_toast(
                    ToastKind::Err,
                    SharedString::from("Reopen failed"),
                    Some(SharedString::from(format!(
                        "Could not read projects.json: {err:#}"
                    ))),
                    cx,
                );
                return;
            }
        }
        let restored = match SessionManager::reopen(
            &mut self.projects,
            &session_id,
            &now_iso8601(),
        ) {
            Ok(s) => s,
            Err(err) => {
                eprintln!("warning: SessionManager::reopen failed: {err:#}");
                return;
            }
        };
        if let Err(err) = codescope_core::session::save(&self.projects, &self.paths) {
            eprintln!("warning: failed to persist session reopen: {err:#}");
        }
        // Mirror the updated config into the sidebar so the closed
        // row disappears from the history disclosure on the same
        // frame as the new tab opens. Without this push the sidebar
        // would still see the row in its `closed_at = Some(_)` state
        // until the next sidebar-side mutation (add/remove project)
        // refreshes the snapshot. `cx.notify()` inside the update
        // forces a redraw — `Sidebar::replace_projects` is data-only
        // and does not notify on its own.
        let projects_for_sidebar = self.projects.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.replace_projects(projects_for_sidebar);
            cx.notify();
        });

        // Build a working SessionDescriptor for the spawn. The shell
        // value is informational here — `spawn_tab_in` resolves the
        // shell from `CODESCOPE_SHELL` / pwsh — but constructing the
        // descriptor ties the reopen flow to the same data shape the
        // agent-launch path already uses, matching C#
        // `SessionManager.BuildDescriptorForSession`.
        let descriptor = codescope_core::session::SessionDescriptor::for_session(
            &restored,
            std::env::var("CODESCOPE_SHELL").unwrap_or_else(|_| "pwsh.exe".into()),
            Vec::new(),
        );
        let working_directory = std::path::PathBuf::from(&descriptor.working_directory);

        // Prefer the same `<Project> · <branch>` convention plain
        // worktree clicks produce — falling back to the descriptor's
        // own title (display_name → branch → id) only when the
        // project / branch pair can't be resolved. An explicit
        // `display_name` override on the persisted row still wins
        // (descriptor's `for_session` already tries that first), so
        // user-renamed sessions come back with their custom name.
        let title: SharedString = if restored.display_name.is_some() {
            descriptor.title.clone().into()
        } else {
            // Single sidebar snapshot for both lookups so they share
            // the same in-memory state and we don't pay the entity
            // borrow twice for one decision.
            let sidebar = self.sidebar.read(cx);
            let project_name = sidebar
                .projects()
                .projects
                .iter()
                .find(|p| p.sessions.iter().any(|s| s.id == restored.id))
                .map(|p| p.name.clone());
            let branch_label = sidebar
                .git_status_for(&descriptor.working_directory)
                .map(|g| g.branch.clone())
                .or_else(|| restored.branch.clone());
            match (project_name, branch_label) {
                (Some(p), Some(b)) => format!("{p} · {b}").into(),
                _ => descriptor.title.clone().into(),
            }
        };

        // `agent_id` → auto-type command. Only the `claude*` family
        // round-trips today; future agent profiles can extend this
        // map as their auto-launch verbs are added. Plain shell
        // sessions (no agent_id) come back as plain shells.
        let auto_type: Option<SharedString> = restored
            .agent_id
            .as_deref()
            .filter(|id| id.starts_with("claude"))
            .map(|_| "claude".into());

        self.spawn_tab_in(
            Some(working_directory),
            Some(title),
            auto_type,
            Some(restored.id),
            window,
            cx,
        );
    }

    fn focused_group(&self) -> &Group {
        &self.groups[self.focused_group]
    }

    /// Toggle / set the Overview panel visibility. When flipped on the
    /// work area (group strip + terminal grid) is replaced by the
    /// full-pane Overview; the sidebar + status bar stay anchored.
    /// Mirrors the `IsOverviewVisible` setter on the C#
    /// `MainViewModel`. `cx.notify()` triggers a re-render so the
    /// flip is visible on the next frame.
    pub(crate) fn set_show_overview(&mut self, value: bool, cx: &mut Context<Self>) {
        if self.show_overview == value {
            return;
        }
        self.show_overview = value;
        // Push the new state into the sidebar so its footer
        // "Overview" button can flip into / out of the active look
        // on the same frame. Mirrors C# `MainViewModel.OnIsOverviewVisibleChanged`
        // updating the bound sidebar VM.
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_overview_visible(value, cx);
        });
        cx.notify();
    }

    /// Read accessor for the overview visibility flag. Used by
    /// `render` to swap the work area for the Overview panel and by
    /// the sidebar (via property forwarding in a future PR) to flip
    /// its footer button into the "active" look.
    #[allow(dead_code)]
    pub(crate) fn show_overview(&self) -> bool {
        self.show_overview
    }

    /// Read-only borrow of the session catalog. Exposed to the
    /// Overview module so it can flatten the live + closed rows
    /// without taking on a clone of the whole `ProjectsConfig` per
    /// render.
    pub(crate) fn projects_snapshot(&self) -> &ProjectsConfig {
        &self.projects
    }

    /// Iterate `(group_idx, tab_idx, session_id, adopted_session_id)`
    /// tuples across every group / tab. The Overview module joins
    /// these against `OverviewRow.session_id` to discover the live
    /// tab a "Focus" click should activate, and against the
    /// telemetry tails to pull live model / token / state data.
    /// Returning owned `String`s keeps the borrow short — the caller
    /// can stash the snapshot in a hashmap without holding `&self`
    /// across the render closure.
    pub(crate) fn overview_tab_snapshot(
        &self,
    ) -> Vec<(usize, usize, String, Option<String>)> {
        let mut out = Vec::new();
        for (g_idx, group) in self.groups.iter().enumerate() {
            for (t_idx, tab) in group.tabs.iter().enumerate() {
                out.push((
                    g_idx,
                    t_idx,
                    tab.session_id.clone(),
                    tab.adopted_session_id.clone(),
                ));
            }
        }
        out
    }

    /// Apply a freshly-loaded `Settings` to the shell. Resolves the
    /// theme by name from the built-in registry, swaps both the
    /// settings and theme `Arc`s, and forwards the new theme to the
    /// sidebar so its chrome repaints in the same frame. Existing
    /// terminals keep their baked-in palette / font; the swap takes
    /// effect for chrome immediately and for new tabs on next spawn.
    /// Live-reapplying palette / font to running terminals lands
    /// when the renderer exposes that knob — until then a settings
    /// edit fully takes over only after the next Ctrl+Shift+T.
    fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let theme = Arc::new(codescope_core::theme::builtin::by_name(&settings.theme));
        self.settings = Arc::new(settings);
        self.theme = theme.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.apply_theme(theme, cx);
        });
        cx.notify();
    }

    /// Open a fresh shell session and append it as a new tab. The new
    /// tab becomes the active one and the terminal grabs focus.
    ///
    /// Without an explicit `working_directory` / `title`, working
    /// directory + tab title come from the sidebar's currently
    /// selected project (cold launch, "+ new tab" button). Callers
    /// that already know which path to pin the terminal to (sidebar
    /// worktree clicks, post-create-worktree spawns) hand both in
    /// directly.
    fn spawn_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.spawn_tab_in(None, None, None, None, window, cx);
    }

    fn spawn_tab_in(
        &mut self,
        working_directory: Option<std::path::PathBuf>,
        title_override: Option<SharedString>,
        auto_type: Option<SharedString>,
        // `Some` on the launch-time rehydrate path so the freshly
        // spawned `Tab` adopts an existing session row rather than
        // appending a new one. `None` on every other call site
        // (sidebar click, dialog spawn, Ctrl+T) — those allocate a
        // fresh id and persist via `SessionManager::open`.
        restore_session_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let shell = std::env::var("CODESCOPE_SHELL")
            .ok()
            .map(|program| Shell::new(program, Vec::new()))
            .or_else(|| {
                if cfg!(windows) {
                    Some(Shell::new("pwsh.exe".into(), Vec::new()))
                } else {
                    None
                }
            });

        let mut env = HashMap::new();
        env.insert("TERM".into(), "xterm-256color".into());
        env.insert("COLORTERM".into(), "truecolor".into());
        env.insert("TERM_PROGRAM".into(), "CodeScope".into());
        env.insert("TERM_PROGRAM_VERSION".into(), "0.0.1".into());

        // Resolve working directory + tab title. Explicit args win;
        // otherwise pull project context from the sidebar — clone the
        // path + name so we don't hold a borrow across `cx.new`.
        let active_project = self
            .sidebar
            .read(cx)
            .active_project()
            .map(|p| (p.path.clone(), p.name.clone()));
        let working_directory = working_directory.or_else(|| {
            active_project
                .as_ref()
                .map(|(path, _)| std::path::PathBuf::from(path))
        });

        // Build the terminal palette + cursor preset from the active
        // theme + settings. Cloned per spawn so each tab carries its
        // own snapshot — themes can swap later without breaking
        // already-running terminals.
        let palette = ColorPalette::from_theme_palette(&self.theme.palette);
        let cursor_preset = CursorStylePreset {
            shape: cursor_shape_from_str(&self.settings.cursor.shape),
            blinking: self.settings.cursor.blinking,
        };
        let font = build_font_config(&self.settings);

        // Clone the resolved working directory before the SpawnConfig
        // moves it — the Tab keeps its own copy so session-restore
        // can re-spawn the pty in the same folder later, without
        // re-resolving via the sidebar (which may have shifted).
        let working_directory_for_tab = working_directory.clone();
        let backend = match Backend::spawn(SpawnConfig {
            shell,
            working_directory,
            env,
            size: TerminalSize {
                num_lines: 30,
                num_cols: 100,
                cell_width: 8,
                cell_height: 18,
            },
            palette: Some(palette.clone()),
            scrollback: self.settings.scrollback,
            default_cursor_style: cursor_preset,
            ..SpawnConfig::default()
        }) {
            Ok(b) => b,
            Err(err) => {
                eprintln!("failed to spawn terminal backend: {err:#}");
                return;
            }
        };

        let terminal = cx.new(|cx| TerminalView::new_full(backend, palette, font, cx));
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let title: SharedString = title_override
            .or_else(|| active_project.map(|(_, name)| name.into()))
            .unwrap_or_else(|| format!("Terminal {}", id + 1).into());
        // Allocate / adopt the persisted session id. `restore_session_id`
        // is `Some` only on the rehydrate path (a session row that was
        // already on disk and is now being brought back into a Tab);
        // every other call site lets us mint a fresh id and append a new
        // row through `SessionManager::open`. Mirrors C#
        // `MainViewModel.NewSessionAsync` (open path) vs. cold-start
        // session restore.
        let session_id =
            self.allocate_session_id(working_directory_for_tab.as_deref(), restore_session_id);
        let group_idx = self.focused_group;
        let group = &mut self.groups[group_idx];
        // Capture the entity so an `auto_type` job can write to it
        // without re-borrowing `self.groups` after the await point.
        let terminal_for_autotype = terminal.clone();
        let agent_id = codescope_core::agent_id_from_auto_type(
            auto_type.as_ref().map(|s| s.as_ref()),
        );
        group.tabs.push(Tab {
            id,
            session_id,
            title,
            terminal,
            working_directory: working_directory_for_tab,
            auto_type: auto_type.clone(),
            spawned_at: SystemTime::now(),
            adopted_session_id: None,
            fired_session_ids: std::collections::HashSet::new(),
            agent_id,
        });
        let new_idx = group.tabs.len() - 1;
        self.activate_tab(group_idx, new_idx, window, cx);

        // Auto-type the requested command after a short settling
        // delay so the shell has had time to print its prompt.
        // Without the delay, the bytes can land before pwsh starts
        // its REPL and get echoed into the banner instead of run.
        if let Some(cmd) = auto_type {
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(Duration::from_millis(250)).await;
                let _ = terminal_for_autotype.update(cx, |term, _cx| {
                    let mut bytes = cmd.as_bytes().to_vec();
                    bytes.push(b'\r');
                    term.write_input(bytes);
                });
            })
            .detach();
        }
    }

    /// Close the tab at `(group_idx, tab_idx)`. When the group's last
    /// tab closes we collapse the group entirely (provided at least
    /// one sibling remains); when no groups remain we quit.
    ///
    /// Closing a tab in an *unfocused* group keeps focus where it was
    /// (mirrors C# `MainViewModel.CloseTabAsync` — the user clicked an
    /// "x" on a sibling, they didn't ask to switch contexts). Auto-
    /// collapsing a focused group that becomes empty does refocus the
    /// fallback group so typing keeps landing somewhere.
    fn close_tab(
        &mut self,
        group_idx: usize,
        tab_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Look up the closing tab's adopted Claude session id (if any)
        // *before* mutating, so we can drop the telemetry tail without
        // holding `&mut group` across the call. The CodeScope session
        // id we soft-close in `SessionManager` is a separate value
        // (always present, allocated at spawn) — read it here too.
        let (adopted, codescope_session_id) = self
            .groups
            .get(group_idx)
            .and_then(|g| g.tabs.get(tab_idx))
            .map(|t| (t.adopted_session_id.clone(), t.session_id.clone()))
            .unwrap_or_default();
        if let Some(sid) = adopted {
            self.unregister_telemetry(&sid);
        }
        // Mark the session row as closed in `projects.json` (mirrors
        // C# `SessionStore.SoftCloseSessionAsync`). Best-effort —
        // failure logs and proceeds; the in-memory tab state is the
        // source of truth for what's on screen.
        if !codescope_session_id.is_empty() {
            self.soft_close_session(&codescope_session_id);
        }
        let Some(group) = self.groups.get_mut(group_idx) else { return };
        if tab_idx >= group.tabs.len() {
            return;
        }
        let was_focused_group = self.focused_group == group_idx;
        group.tabs.remove(tab_idx);
        if group.tabs.is_empty() {
            // Empty group — collapse if there are siblings, otherwise
            // quit.
            if self.groups.len() == 1 {
                cx.quit();
                return;
            }
            self.groups.remove(group_idx);
            // Drop the matching weight slot. Surviving weights stay
            // unchanged — closing one column doesn't redistribute, so
            // the remaining groups keep their ratios. Mirrors C#'s
            // `MainViewModel.CloseGroup`.
            if group_idx < self.group_weights.len() {
                self.group_weights.remove(group_idx);
            }
            // If the focused group was at or after `group_idx`, slide
            // its index left so it keeps pointing at the same group.
            // Re-focus the (possibly new) focused group so keyboard
            // input lands somewhere live.
            if self.focused_group >= self.groups.len() {
                self.focused_group = self.groups.len() - 1;
            } else if group_idx <= self.focused_group && self.focused_group > 0 {
                self.focused_group -= 1;
            }
            let active = self.groups[self.focused_group].active_tab;
            self.activate_tab(self.focused_group, active, window, cx);
            self.save_layout();
            return;
        }
        // Group still has tabs — slide the active index left if we
        // closed at or before it.
        let group = &mut self.groups[group_idx];
        if group.active_tab >= group.tabs.len() {
            group.active_tab = group.tabs.len() - 1;
        } else if group.active_tab > tab_idx {
            group.active_tab -= 1;
        }
        if was_focused_group {
            let new_active = group.active_tab;
            self.activate_tab(group_idx, new_active, window, cx);
        } else {
            // Closing a tab in a sibling group — adjust its `active_tab`
            // (already done above) and redraw, but leave keyboard focus
            // on the user's actually-focused group.
            cx.notify();
        }
    }

    /// Activate `(group_idx, tab_idx)`. Sets focused group, marks the
    /// tab as the group's active one, and routes keyboard focus to the
    /// terminal so typing lands in the right pty without an extra
    /// click.
    pub(crate) fn activate_tab(
        &mut self,
        group_idx: usize,
        tab_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group) = self.groups.get_mut(group_idx) else { return };
        if tab_idx >= group.tabs.len() {
            return;
        }
        group.active_tab = tab_idx;
        let prev_focused = self.focused_group;
        self.focused_group = group_idx;
        let handle = self.groups[group_idx].tabs[tab_idx].terminal.read(cx).focus_handle(cx);
        handle.focus(window);
        cx.notify();
        if prev_focused != group_idx {
            self.save_layout();
        }
    }

    fn next_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let group_idx = self.focused_group;
        let group = &self.groups[group_idx];
        if group.tabs.is_empty() {
            return;
        }
        let next = (group.active_tab + 1) % group.tabs.len();
        self.activate_tab(group_idx, next, window, cx);
    }

    fn prev_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let group_idx = self.focused_group;
        let group = &self.groups[group_idx];
        if group.tabs.is_empty() {
            return;
        }
        let prev = if group.active_tab == 0 {
            group.tabs.len() - 1
        } else {
            group.active_tab - 1
        };
        self.activate_tab(group_idx, prev, window, cx);
    }

    /// Drop the focused group entirely. Pre-condition: the group is
    /// empty (caller checks) and there's at least one sibling. After
    /// removal we land focus on the previous (or first remaining)
    /// group's active tab so typing keeps working without an extra
    /// click. Mirrors `MainViewModel.CloseGroup`.
    fn close_focused_group(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        debug_assert!(self.focused_group < self.groups.len());
        debug_assert!(self.groups[self.focused_group].tabs.is_empty());
        debug_assert!(self.groups.len() > 1);
        let removed = self.focused_group;
        self.groups.remove(removed);
        if removed < self.group_weights.len() {
            self.group_weights.remove(removed);
        }
        if self.focused_group >= self.groups.len() {
            self.focused_group = self.groups.len() - 1;
        }
        let active = self.groups[self.focused_group].active_tab;
        self.activate_tab(self.focused_group, active, window, cx);
        self.save_layout();
    }

    /// Append a new empty group to the right of the focused one and
    /// move focus to it. Mirrors `MainViewModel.SplitRight` (Ctrl+\ in
    /// the C# build). The new group has no tabs — the caller usually
    /// follows up with `spawn_tab`, or the user does via Ctrl+T / the
    /// `+` button on the new strip.
    fn split_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_group_id;
        self.next_group_id += 1;
        let insert_at = self.focused_group + 1;
        self.groups.insert(
            insert_at,
            Group { id, tabs: Vec::new(), active_tab: 0 },
        );
        // New group enters with weight 1.0 — equal share of whatever
        // a unit weight resolves to in the current layout. The
        // existing groups keep their weights so a 1.5/1.0 split that
        // gets a third group becomes 1.5/1.0/1.0 (≈42.9/28.6/28.6%).
        self.group_weights.insert(insert_at, 1.0);
        self.focused_group = insert_at;
        // Drop keyboard focus back on AppShell's root handle so the
        // next typed character isn't routed to a now-stale terminal
        // (the previously focused group's). Once the user types Ctrl+T
        // / clicks +, `activate_tab` will rehome focus.
        self.focus_handle.focus(window);
        cx.notify();
        self.save_layout();
    }

    /// Toggle the sidebar between visible and collapsed. Persists so
    /// the choice survives a restart. When collapsing, the saved
    /// width stays put — re-opening uses the previous width instead
    /// of resetting to default.
    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_visible = !self.sidebar_visible;
        cx.notify();
        self.save_layout();
    }

    /// Begin a sidebar resize drag from the mouse-down on the right
    /// edge handle.
    fn begin_sidebar_drag(&mut self, cursor_x: gpui::Pixels, _cx: &mut Context<Self>) {
        self.sidebar_drag = Some(SidebarDrag {
            start_x: cursor_x,
            start_width: self.sidebar_width,
        });
    }

    /// Update sidebar width during an in-flight drag. Clamped between
    /// `SIDEBAR_MIN_WIDTH` (160 px — below this the project list is
    /// unusable) and `SIDEBAR_MAX_WIDTH` (600 px — beyond this the
    /// work area gets crowded out).
    fn update_sidebar_drag(&mut self, cursor_x: gpui::Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.sidebar_drag.as_ref() else { return };
        let dx: f32 = (cursor_x - drag.start_x).into();
        let new_width = (drag.start_width + dx).clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
        if (new_width - self.sidebar_width).abs() > 0.1 {
            self.sidebar_width = new_width;
            cx.notify();
        }
    }

    fn end_sidebar_drag(&mut self, cx: &mut Context<Self>) {
        if self.sidebar_drag.take().is_some() {
            self.save_layout();
            cx.notify();
        }
    }

    /// Mouse pressed on the splitter at gap `split_idx`. Captures the
    /// snapshot the drag-update needs and stamps the cursor as
    /// col-resize until release.
    fn begin_splitter_drag(
        &mut self,
        split_idx: usize,
        cursor_x: gpui::Pixels,
        window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
        if split_idx + 1 >= self.groups.len() {
            return;
        }
        // Pixels-per-weight conversion. Total work-area width is the
        // viewport minus the sidebar (and minus the per-splitter
        // hit-targets, but those are tiny — close enough for drag
        // feel that we ignore them here). Total weight is the sum of
        // all current weights.
        let viewport: f32 = window.viewport_size().width.into();
        // The sidebar wrapper takes `sidebar_width` plus the 1 px
        // resize handle that sits between sidebar and work area;
        // both are gone when collapsed. We subtract `DIVIDER_VISUAL_WIDTH`
        // (not `SPLITTER_HIT_WIDTH`) here because the handle's flex
        // contribution to the layout is just the visible line — the
        // 6 px hit zone is an absolute overlay that doesn't displace
        // adjacent content. Using the wrong constant would shave 5
        // extra pixels off `work_width` and the splitter drag would
        // drift relative to the cursor.
        let sidebar_pixels = if self.sidebar_visible {
            self.sidebar_width + DIVIDER_VISUAL_WIDTH
        } else {
            0.0
        };
        let work_width = (viewport - sidebar_pixels).max(1.0);
        let total_weight: f32 = self.group_weights.iter().copied().sum::<f32>().max(0.001);
        let px_per_unit = work_width / total_weight;
        self.splitter_drag = Some(SplitterDrag {
            split_idx,
            start_x: cursor_x,
            start_weights: self.group_weights.clone(),
            px_per_unit,
        });
    }

    /// Cursor moved while a splitter drag is in flight. Recomputes the
    /// two affected weights from the start snapshot — re-deriving
    /// from the live values would compound rounding error across the
    /// many per-pixel moves a real drag generates.
    fn update_splitter_drag(&mut self, cursor_x: gpui::Pixels, cx: &mut Context<Self>) {
        let Some(drag) = self.splitter_drag.as_ref() else { return };
        let split_idx = drag.split_idx;
        if split_idx + 1 >= self.group_weights.len() {
            // Group count changed under us (collapse mid-drag) — bail.
            self.splitter_drag = None;
            return;
        }
        let dx: f32 = (cursor_x - drag.start_x).into();
        let mut delta_units = dx / drag.px_per_unit;
        let left = drag.start_weights[split_idx];
        let right = drag.start_weights[split_idx + 1];
        // Clamp so neither side disappears below MIN_GROUP_WEIGHT.
        if left + delta_units < MIN_GROUP_WEIGHT {
            delta_units = MIN_GROUP_WEIGHT - left;
        }
        if right - delta_units < MIN_GROUP_WEIGHT {
            delta_units = right - MIN_GROUP_WEIGHT;
        }
        self.group_weights[split_idx] = left + delta_units;
        self.group_weights[split_idx + 1] = right - delta_units;
        cx.notify();
    }

    /// Mouse released — commit the drag-end weights to disk so the
    /// resized column survives a restart.
    fn end_splitter_drag(&mut self, cx: &mut Context<Self>) {
        if self.splitter_drag.take().is_some() {
            self.save_layout();
            cx.notify();
        }
    }

    /// Build the floating toast stack. Anchored bottom-right, deferred
    /// so it paints over the rest of the chrome. Returns `None`
    /// when there are no toasts so the root render's `.children(...)`
    /// stays an empty iterator.
    fn render_toasts(
        &self,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if self.toasts.is_empty() {
            return None;
        }
        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let danger = theme::danger(theme);
        let accent_clean = theme::status_clean(theme);

        let stack = div()
            .flex()
            .flex_col()
            .gap_2()
            .min_w(px(280.0))
            .max_w(px(420.0))
            .children(self.toasts.iter().map(|t| {
                let id = t.id;
                let stripe = match t.kind {
                    ToastKind::Ok => accent_clean,
                    ToastKind::Err => danger,
                    ToastKind::Info => ink_dim,
                };
                let title = t.title.clone();
                let detail = t.detail.clone();
                div()
                    .id(("toast", id))
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .p_3()
                    .bg(elevated)
                    .border_1()
                    .border_color(divider)
                    .rounded_md()
                    .shadow_lg()
                    .child(
                        div()
                            .w(px(3.0))
                            .h_full()
                            .min_h(px(20.0))
                            .bg(stripe)
                            .rounded_sm(),
                    )
                    .child(
                        div()
                            .flex_grow()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_color(ink)
                                    .text_size(px(13.0))
                                    .child(title),
                            )
                            .children(detail.map(|d| {
                                div()
                                    .text_color(ink_dim)
                                    .text_size(px(11.0))
                                    .child(d)
                            })),
                    )
                    .child(
                        div()
                            .id(("toast-dismiss", id))
                            .w(px(20.0))
                            .h(px(20.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .text_color(ink_ghost)
                            .cursor_pointer()
                            .hover(move |s| s.text_color(ink))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.dismiss_toast(id, cx);
                                }),
                            )
                            .child("×"),
                    )
                    .into_any_element()
            }));

        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(px(0.0), px(0.0)))
                    .anchor(gpui::Corner::BottomRight)
                    .snap_to_window_with_margin(px(16.0))
                    .child(stack),
            )
            .into_any_element(),
        )
    }

    /// Build the floating notifications popover.  Anchored bottom-right
    /// (same corner as the toast stack), deferred so it paints over all
    /// chrome.  Returns `None` when the popover is closed so the root
    /// render's `.children(...)` stays an empty iterator.
    ///
    /// Geometry mirrors `BellPopup` in `StatusBarView.xaml` exactly:
    /// - 360 px wide, max-height 420 px
    /// - Header: 14 l / 12 t / 10 r / 10 b — title + conditional "Clear"
    /// - Entry rows: 14 l/r, 10 t/b, 1 px top divider between consecutive rows
    /// - Kind dot: 6×6, `margin-top: 5`, `margin-right: 10`, top-aligned
    /// - Footer: 14 l/r, 8 t/b, 1 px top border, dim hint text
    fn render_notifications_popover(
        &self,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        if !self.notifications.is_open() {
            return None;
        }

        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_muted = theme::ink_muted(theme);
        let frost = theme::frost_10(theme);
        let accent = theme::accent(theme);
        // Signal.Warn from DesignTokens.xaml: Signal.Color.Warn = #FFFF5A5A.
        // HSL: hue=0 (red), sat=1.0, lum=0.671.
        let signal_warn = gpui::Hsla {
            h: 0.0,
            s: 1.0,
            l: 0.671,
            a: 1.0,
        };

        let has_any = self.notifications.has_any();

        // Header: "Notifications" label + conditional "Clear" button.
        let clear_btn = if has_any {
            Some(
                div()
                    .id("notif-clear-btn")
                    .px(px(6.0))
                    .py(px(3.0))
                    .rounded_md()
                    .text_size(px(11.0))
                    .text_color(ink_dim)
                    .cursor_pointer()
                    .hover(move |s| s.bg(frost))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.notifications.clear_all();
                            cx.notify();
                        }),
                    )
                    .child("Clear"),
            )
        } else {
            None
        };

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .pl(px(14.0))
            .pt(px(12.0))
            .pr(px(10.0))
            .pb(px(10.0))
            .child(
                div()
                    .text_size(px(12.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(ink)
                    .child("Notifications"),
            )
            .children(clear_btn);

        // Body — either the "No notifications yet." empty state or the
        // scrollable entry list.
        let body: gpui::AnyElement = if !has_any {
            div()
                .flex()
                .items_center()
                .justify_center()
                .mt(px(28.0))
                .mb(px(36.0))
                .text_size(px(11.5))
                .text_color(ink_dim)
                .child("No notifications yet.")
                .into_any_element()
        } else {
            let entries: Vec<gpui::AnyElement> = self
                .notifications
                .entries()
                .iter()
                .enumerate()
                .map(|(i, entry)| {
                    let id = entry.id;
                    let dot_color = match entry.kind {
                        crate::notifications::NotificationKind::Generic => ink_dim,
                        crate::notifications::NotificationKind::SessionWaiting => signal_warn,
                        crate::notifications::NotificationKind::SessionReady => accent,
                    };
                    let title = entry.title.clone();
                    let detail = entry.detail.clone();
                    let session_title = entry.session_title.clone();
                    let timestamp =
                        crate::notifications::format_hhmm(entry.timestamp);
                    let has_divider = i > 0;

                    // Hover tint: #0EFFFFFF = ink at ~5.5 % alpha.
                    let hover_tint = gpui::Hsla {
                        h: 1.0,
                        s: 1.0,
                        l: 1.0,
                        a: 0.055,
                    };

                    let _ = has_divider;
                    let row = div()
                        .id(("notif-entry", id))
                        .flex()
                        .flex_row()
                        .items_start()
                        .pl(px(14.0))
                        .pr(px(14.0))
                        .pt(px(10.0))
                        .pb(px(10.0))
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover_tint))
                        // Top divider on *every* row (matches the
                        // XAML `BorderThickness="0,1,0,0"` on the
                        // entry button — that's also what gives
                        // the header→first-entry separator). Was
                        // previously gated on `i > 0` which left
                        // the first entry visually disconnected
                        // from the header.
                        .border_t_1()
                        .border_color(divider);

                    let row = row
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                // `activate` returns the originating
                                // session title (when one was attached
                                // to the entry). If we can find a tab
                                // with that exact title, focus it —
                                // otherwise the entry just gets marked
                                // read. This is what the footer hint
                                // ("Click an entry to jump to its
                                // session.") promises.
                                if let Some(session_title) =
                                    this.notifications.activate(id)
                                {
                                    let target = this
                                        .groups
                                        .iter()
                                        .enumerate()
                                        .find_map(|(g_idx, group)| {
                                            group
                                                .tabs
                                                .iter()
                                                .position(|t| t.title == session_title)
                                                .map(|t_idx| (g_idx, t_idx))
                                        });
                                    if let Some((g_idx, t_idx)) = target {
                                        this.activate_tab(g_idx, t_idx, window, cx);
                                    }
                                }
                                this.notifications.set_open(false);
                                cx.notify();
                            }),
                        )
                        // Kind dot — 6×6, top-aligned, 10 px right margin.
                        .child(
                            div()
                                .w(px(6.0))
                                .h(px(6.0))
                                .mt(px(5.0))
                                .mr(px(10.0))
                                .flex_shrink_0()
                                .rounded_full()
                                .bg(dot_color),
                        )
                        // Text column: title / detail / session_title (mono).
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_grow()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .text_color(ink)
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .mt(px(2.0))
                                        .text_size(px(11.0))
                                        .text_color(ink_muted)
                                        .child(detail),
                                )
                                .children(session_title.map(|st| {
                                    div()
                                        .mt(px(3.0))
                                        .text_size(px(10.5))
                                        .text_color(ink_dim)
                                        .child(st)
                                })),
                        )
                        // Timestamp — top-aligned dim mono label.
                        .child(
                            div()
                                .ml(px(10.0))
                                .mt(px(1.0))
                                .flex_shrink_0()
                                .text_size(px(10.5))
                                .text_color(ink_dim)
                                .child(timestamp),
                        );

                    row.into_any_element()
                })
                .collect();

            // `flex_grow` + `min_h(0)` so the list takes the
            // remaining height in the panel's flex column (mirrors
            // the XAML `RowDefinition Height="*"`) and lets
            // `overflow_y_scroll` actually clip + scroll instead of
            // pushing the panel taller than `max_h`.
            let mut entry_list = div()
                .id("notif-entry-list")
                .flex()
                .flex_col()
                .flex_grow()
                .overflow_y_scroll()
                .children(entries);
            entry_list.style().min_size.height =
                Some(gpui::Length::Definite(px(0.0).into()));
            entry_list.into_any_element()
        };

        // Footer: 1 px top border, dim hint text.
        let footer = div()
            .border_t_1()
            .border_color(divider)
            .pl(px(14.0))
            .pr(px(14.0))
            .pt(px(8.0))
            .pb(px(8.0))
            .text_size(px(10.5))
            .text_color(ink_dim)
            .child("Click an entry to jump to its session.");

        // Outer panel — 360 px wide, max-height 420 px, anchored
        // bottom-right above the status bar (mirroring the Popup
        // `Placement="Top"` in the XAML, offset from the bell button).
        let panel = div()
            .id("notif-popover")
            .w(px(360.0))
            .max_h(px(420.0))
            .flex()
            .flex_col()
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_md()
            .shadow_lg()
            .child(header)
            .child(body)
            .child(footer);

        // Mirror the C# `BellPopup` anchor:
        // `PlacementTarget=BellButton`, `Placement=Top`,
        // `VerticalOffset=-6`. The bell button's window-space rect
        // is recorded each frame by the `canvas` overlay attached to
        // `bell_btn`, so `bell_bounds.top_right()` is the button's
        // actual top-right corner regardless of layout. We move that
        // point up by 6 px and anchor the popover's `BottomRight`
        // corner there.
        //
        // `bell_bounds` is `None` for the very first render before
        // the canvas has had a chance to lay out; we fall back to a
        // window-corner snap in that case so the popover never
        // renders at (0, 0).
        const POPOVER_GAP_PX: f32 = 6.0;
        const SNAP_MARGIN_PX: f32 = 8.0;
        let snap_edges = gpui::Edges {
            top: px(SNAP_MARGIN_PX),
            right: px(SNAP_MARGIN_PX),
            bottom: px(SNAP_MARGIN_PX),
            left: px(SNAP_MARGIN_PX),
        };
        let anchored = if let Some(bell) = self.bell_bounds {
            let top_right = bell.top_right();
            let anchor_point = gpui::point(
                top_right.x,
                top_right.y - px(POPOVER_GAP_PX),
            );
            gpui::anchored()
                .position(anchor_point)
                .anchor(gpui::Corner::BottomRight)
                .snap_to_window_with_margin(snap_edges)
                .child(panel)
        } else {
            // Keep the panel clear of the bar by snapping above the
            // bar plus the same 6 px gap (status bar height comes
            // from the shared `STATUS_BAR_HEIGHT_PX` constant the
            // bar's own `h(...)` uses, so they can't drift).
            let fallback_edges = gpui::Edges {
                top: px(SNAP_MARGIN_PX),
                right: px(SNAP_MARGIN_PX),
                bottom: px(STATUS_BAR_HEIGHT_PX + POPOVER_GAP_PX),
                left: px(SNAP_MARGIN_PX),
            };
            gpui::anchored()
                .position(gpui::point(px(0.0), px(0.0)))
                .anchor(gpui::Corner::BottomRight)
                .snap_to_window_with_margin(fallback_edges)
                .child(panel)
        };
        Some(gpui::deferred(anchored).into_any_element())
    }

    /// Build the tab right-click menu when one is open. Returns
    /// `None` when no menu is showing — caller uses `.children(...)`
    /// so 0 / 1 children both work without splitting the chain.
    fn render_tab_menu(
        &self,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let menu = self.tab_menu.as_ref()?;
        let group_id = menu.group_id;
        let tab_id = menu.tab_id;
        let position = menu.position;
        // Compute "is there at least one other tab in this group?"
        // and "is there at least one tab to the right?" so we can
        // dim / hide the rows that would be no-ops.
        let group = self.groups.iter().find(|g| g.id == group_id)?;
        let pivot_pos = group.tabs.iter().position(|t| t.id == tab_id)?;
        let has_others = group.tabs.len() > 1;
        let has_right = pivot_pos + 1 < group.tabs.len();

        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let frost = theme::frost_10(theme);
        let danger = theme::danger(theme);

        type Action = Box<dyn Fn(&mut AppShell, &mut Window, &mut Context<AppShell>) + 'static>;
        let item = |id: &'static str,
                    label: &'static str,
                    enabled: bool,
                    danger_row: bool,
                    on_click: Action|
         -> gpui::Stateful<gpui::Div> {
            // Disabled rows step down a tone to `ink_ghost` so the
            // user sees the affordance is greyed-out without having
            // to hover. Enabled rows keep the regular `ink_dim` /
            // `danger` palette.
            let base_color = if !enabled {
                ink_ghost
            } else if danger_row {
                danger
            } else {
                ink_dim
            };
            let hover_color = if !enabled {
                ink_ghost
            } else if danger_row {
                danger
            } else {
                ink
            };
            let frost_hover = frost;
            let mut row = div()
                .id(id)
                .h(px(28.0))
                .px_3()
                .flex()
                .flex_row()
                .items_center()
                // Tab context-menu items: `FontSize="12.5"` from
                // `ContextMenuStyles.xaml` default MenuItem style.
                .text_size(px(12.5))
                .text_color(base_color)
                .child(label);
            if enabled {
                row = row
                    .cursor_pointer()
                    .hover(move |s| s.bg(frost_hover).text_color(hover_color))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            on_click(this, window, cx);
                        }),
                    );
            }
            row
        };

        let menu_body = div()
            .flex()
            .flex_col()
            .py_1()
            .min_w(px(200.0))
            .bg(elevated)
            .border_1()
            .border_color(divider)
            .rounded_md()
            .shadow_lg()
            // C# `ContextMenuStyles.xaml` default MenuItem style sets
            // `FontFamily="{DynamicResource Fig.Font.Sans}"`. Apply at
            // the menu root so every row inherits the sans face
            // without each row builder having to repeat the call.
            .font(theme::font_sans())
            .child(item(
                "tab-menu-close",
                "Close",
                true,
                false,
                Box::new(move |this, window, cx| {
                    let g_idx = this
                        .groups
                        .iter()
                        .position(|g| g.id == group_id);
                    if let Some(g_idx) = g_idx
                        && let Some(t_idx) = this.groups[g_idx]
                            .tabs
                            .iter()
                            .position(|t| t.id == tab_id)
                    {
                        this.close_tab_menu(cx);
                        this.close_tab(g_idx, t_idx, window, cx);
                    }
                }),
            ))
            .child(item(
                "tab-menu-close-others",
                "Close others",
                has_others,
                false,
                Box::new(move |this, window, cx| {
                    this.close_other_tabs_in_group(group_id, tab_id, window, cx);
                }),
            ))
            .child(item(
                "tab-menu-close-right",
                "Close all to the right",
                has_right,
                false,
                Box::new(move |this, window, cx| {
                    this.close_tabs_to_right_in_group(group_id, tab_id, window, cx);
                }),
            ))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _, _, cx| cx.stop_propagation()),
            )
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_tab_menu(cx)));

        Some(
            gpui::deferred(
                gpui::anchored()
                    .position(gpui::point(position.x, position.y))
                    .anchor(gpui::Corner::TopLeft)
                    .snap_to_window_with_margin(px(8.0))
                    .child(menu_body),
            )
            .into_any_element(),
        )
    }

    /// Build the bottom status bar. 32 px tall, two clusters mirroring
    /// the C# `StatusBarView`:
    ///
    /// - **Left cluster** — session context dot (green = idle, amber =
    ///   busy / pending tool use) + branch name from
    ///   [`Sidebar::git_status_for`]; git diff `+N −N` (white added,
    ///   dim removed, "changes" fallback when only untracked files
    ///   are present); remote ahead/behind (`↑N` / `↓N` / `↑N ↓N`).
    ///   Hidden entirely on tabs with no working directory (e.g.
    ///   plain pwsh launched without a project).
    /// - **Right cluster** — agent-transcript-derived slots (model,
    ///   tokens + percent, turn count, last turn duration), agent
    ///   rollup `N busy · M idle` across all adopted agent tabs
    ///   (Claude / Copilot / OpenCode / Pi — whichever the active
    ///   tab's `agent_id` selected at spawn), optional `N groups`
    ///   label, workspace summary, and the notifications bell with
    ///   a 4 px unread dot.
    ///
    /// Data sources: [`Sidebar::git_status_for`] (branch + numstat +
    /// ahead/behind), [`AppShell::telemetry_for`] (per-session model
    /// / tokens / turns / state), [`Sidebar::worktree_counts`]
    /// (workspace summary), [`AppShell::notifications`] (bell + dot).
    fn render_status_bar(
        &self,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // ─── Inputs ──────────────────────────────────────────────
        let group_count = self.groups.len();
        let focused_group = self.focused_group();
        let active_tab = focused_group.tabs.get(focused_group.active_tab);

        let active_working_dir: Option<String> = active_tab
            .and_then(|t| t.working_directory.as_ref())
            .map(|p| p.to_string_lossy().into_owned());
        let active_session_id: Option<String> = active_tab
            .and_then(|t| t.adopted_session_id.clone());
        let active_title: Option<SharedString> = active_tab.map(|t| t.title.clone());

        let (git, worktree_total, worktree_dirty, projects_empty) = {
            let sidebar = self.sidebar.read(cx);
            let g = active_working_dir
                .as_deref()
                .and_then(|p| sidebar.git_status_for(p))
                .cloned();
            let (t, d) = sidebar.worktree_counts();
            let empty = sidebar.projects().projects.is_empty();
            (g, t, d, empty)
        };

        let snapshot = active_session_id
            .as_deref()
            .and_then(|sid| self.telemetry_for(sid));
        let (agent_busy, agent_idle) = self.agent_rollup_counts();

        // ─── Theme tokens ───────────────────────────────────────
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_muted = theme::ink_muted(theme);
        let divider_clr = theme::divider(theme);
        let accent = theme::accent(theme);
        // Signal.Ok / Signal.Warn pulled from DesignTokens.xaml via
        // `theme::signal_ok` / `theme::signal_warn` so the dot colour
        // tracks any future theme override and stays exactly in
        // sync with the WPF brushes (#FF4BD87B / #FFFF5A5A).
        let signal_ok = theme::signal_ok();
        let signal_warn = theme::signal_warn();

        // ─── Helpers ────────────────────────────────────────────
        let sep = move || div().w_px().h(px(14.0)).bg(divider_clr);

        // ─── Left cluster ───────────────────────────────────────
        // The session dot + branch/title renders whenever any tab is
        // focused — matches C# `StatusHasSession = SelectedTab is not
        // null`. The dot itself is coloured by telemetry state below;
        // shell tabs without a snapshot fall through to `signal_ok`.
        let session_dot_color = match snapshot.as_ref().map(|s| s.state) {
            Some(codescope_core::SessionState::Busy)
            | Some(codescope_core::SessionState::PendingToolUse) => signal_warn,
            _ => signal_ok,
        };
        // C# `StatusHasSession` = `SelectedTab is not null` — so the
        // session cluster (dot + branch/title) renders whenever there's
        // any focused tab, even when no git context has surfaced yet
        // (shell tabs, unresolved working dir). Branch comes from the
        // worktree's `DisplayBranch` when available; otherwise we fall
        // back to the tab title, exactly like the C# `StatusBranch`.
        let session_cluster = active_tab.map(|_| {
            let branch_text: SharedString = match (git.as_ref(), active_title.clone()) {
                (Some(g), _) => g.branch.clone().into(),
                (None, Some(t)) => t,
                (None, None) => SharedString::from(""),
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .child(
                    div()
                        .w(px(6.0))
                        .h(px(6.0))
                        .rounded_full()
                        .bg(session_dot_color),
                )
                .child(
                    // `flex_grow` + `truncate` so a long tab title /
                    // branch name doesn't shove the right cluster off
                    // the bar — matches the WPF `TextTrimming` on the
                    // C# `StatusBranch` TextBlock.
                    div()
                        .flex_grow()
                        .truncate()
                        .text_color(ink)
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .child(branch_text),
                )
        });

        // Git diff segment — only when we have a `GitStatus` snapshot
        // and either a numstat result or `has_changes` (untracked-only
        // fallback). Mirrors `StatusHasDiffStats` in the C#.
        let diff_segment = git.as_ref().and_then(|g| {
            if g.added == 0 && g.removed == 0 && !g.has_changes {
                return None;
            }
            let mut row = div().flex().flex_row().items_center().gap(px(4.0));
            if g.added > 0 {
                row = row.child(div().text_color(ink).child(format!("+{}", g.added)));
            }
            if g.removed > 0 {
                row = row
                    .child(div().text_color(ink_dim).child(format!("\u{2212}{}", g.removed)));
            }
            if g.added == 0 && g.removed == 0 && g.has_changes {
                row = row.child(div().text_color(ink_dim).child("changes"));
            }
            Some(row)
        });

        // Remote sync ↑/↓ — only when we have an upstream and a delta.
        let ahead_behind_text: Option<String> = git.as_ref().and_then(|g| {
            if !g.has_upstream {
                return None;
            }
            match (g.ahead, g.behind) {
                (0, 0) => None,
                (a, 0) => Some(format!("\u{2191}{a}")),
                (0, b) => Some(format!("\u{2193}{b}")),
                (a, b) => Some(format!("\u{2191}{a} \u{2193}{b}")),
            }
        });
        let remote_segment = ahead_behind_text.map(|t| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .text_color(ink)
                .child(t)
        });

        // ─── Right cluster (telemetry-driven) ───────────────────
        let model_label = snapshot
            .as_ref()
            .and_then(|s| s.model.as_deref())
            .map(codescope_core::model_display_name);
        let model_segment = model_label.map(|m| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .text_color(ink_muted)
                .child(div().text_color(ink_muted).child("\u{25C6}"))
                .child(m)
        });

        let tokens_segment = snapshot.as_ref().filter(|s| s.tokens_used > 0).map(|s| {
            let used = codescope_core::format_tokens(s.tokens_used);
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.0))
                .child(
                    div()
                        .text_color(ink)
                        .child(used),
                )
                .child(div().text_color(ink_dim).child("tok"));
            if let Some(pct) = s.context_pct {
                let pct_text = codescope_core::format_context_pct(pct);
                row = row.child(div().text_color(ink_dim).child(pct_text));
            }
            row
        });

        let turns_segment = snapshot.as_ref().filter(|s| s.turn_count > 0).map(|s| {
            let suffix = if s.turn_count == 1 { "turn" } else { "turns" };
            div()
                .text_color(ink_dim)
                .child(format!("{} {}", s.turn_count, suffix))
        });

        let duration_segment = snapshot
            .as_ref()
            .and_then(|s| s.last_turn_duration)
            .map(|d| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .text_color(ink_dim)
                    .child(div().text_color(ink_muted).child("\u{25CB}"))
                    .child(codescope_core::TranscriptTail::format_duration(d))
            });

        let agent_summary_visible = agent_busy + agent_idle > 0;
        let agent_segment = if agent_summary_visible {
            Some(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .w(px(6.0))
                                    .h(px(6.0))
                                    .rounded_full()
                                    .bg(signal_warn),
                            )
                            .child(
                                div()
                                    .text_color(ink)
                                    .child(format!("{}", agent_busy)),
                            )
                            .child(div().text_color(ink_dim).child("busy")),
                    )
                    .child(div().text_color(ink_dim).child("·"))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(4.0))
                            .child(
                                div()
                                    .w(px(6.0))
                                    .h(px(6.0))
                                    .rounded_full()
                                    .bg(signal_ok),
                            )
                            .child(
                                div()
                                    .text_color(ink)
                                    .child(format!("{}", agent_idle)),
                            )
                            .child(div().text_color(ink_dim).child("idle")),
                    ),
            )
        } else {
            None
        };

        let group_label = if group_count > 1 {
            Some(format!("{} groups", group_count))
        } else {
            None
        };
        // C# `StatusBarView` has no tab counter — the count is implied
        // by the tab strip itself, so we no longer render a "tab N/M"
        // segment here.

        // Workspace summary — `N worktrees · M dirty`. Only rendered
        // when at least one worktree is tracked; the middle dot only
        // appears when the dirty segment is visible. Mirrors
        // C# `StatusWorkspaceVisible` / `StatusDirtyVisible`.
        let workspace_summary = (worktree_total > 0).then(|| {
            let worktree_text = format!(
                "{} {}",
                worktree_total,
                if worktree_total == 1 { "worktree" } else { "worktrees" }
            );
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .text_color(ink_dim)
                .child(div().child(worktree_text));
            if worktree_dirty > 0 {
                row = row
                    .child(div().child("·"))
                    .child(div().child(format!("{} dirty", worktree_dirty)));
            }
            row
        });

        // Empty-state tagline — `StatusEmptyVisible` in C# is
        // `Projects.Count == 0`, regardless of whether a tab is
        // focused. The session cluster yields to it below so the
        // status bar reads as the empty-state hint even if a stray
        // shell tab is open.
        let empty_state = projects_empty.then(|| {
            div()
                .text_color(ink_dim)
                .child("CodeScope — add a project to begin.")
        });

        // ─── Bell button ─────────────────────────────────────────
        let has_unread = self.notifications.has_unread();
        let bell_btn = div()
            .id("status-bell-btn")
            .relative()
            .w(px(22.0))
            .h(px(22.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded_md()
            .cursor_pointer()
            .hover(move |s| s.bg(gpui::Hsla { h: 0.0, s: 0.0, l: 1.0, a: 0.08 }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.notifications.toggle();
                    cx.notify();
                }),
            )
            // Invisible `canvas` child stretched to the bell's hit area
            // — its prepaint callback receives the bell button's
            // window-space `Bounds<Pixels>` and stashes them on the
            // entity so `render_notifications_popover` can anchor to
            // the actual button rect. Updates every frame, so resizes
            // / status-bar reflows keep the popover stuck to the bell.
            // The canvas registers no mouse listeners; the parent
            // `bell_btn`'s `on_mouse_down` still receives the click.
            .child({
                let entity = cx.entity();
                gpui::canvas(
                    move |bounds, _window, cx| {
                        entity.update(cx, |this, cx| {
                            // Only notify when the bounds actually
                            // change — prepaint runs every frame and
                            // we don't want a render loop. The
                            // notify is needed so the popover (a
                            // sibling element built earlier in the
                            // same render) repaints with the new
                            // anchor on the next frame after a
                            // resize / reflow; without it the
                            // popover would only refresh when some
                            // unrelated event triggered a paint.
                            if this.bell_bounds != Some(bounds) {
                                this.bell_bounds = Some(bounds);
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            // Bell glyph — fall back to a unicode symbol since we don't
            // ship vector icons. The C# uses an SVG path; the unicode
            // bell ringer (U+1F514) is a close visual stand-in at this
            // size and stays inside the design-tokens font stack.
            .child(div().text_color(ink_muted).text_size(px(13.0)).child("\u{1F514}"))
            .when(has_unread, |this| {
                this.child(
                    div()
                        .absolute()
                        .top(px(2.0))
                        .right(px(2.0))
                        .w(px(4.0))
                        .h(px(4.0))
                        .rounded_full()
                        .bg(accent),
                )
            });

        // ─── Bar ─────────────────────────────────────────────────
        let mut bar = div()
            .h(px(STATUS_BAR_HEIGHT_PX))
            .flex()
            .flex_row()
            .items_center()
            .px(px(12.0))
            .gap(px(10.0))
            .border_t_1()
            .border_color(divider_clr)
            .bg(theme::elevated(theme))
            // C# `StatusBarView` SbText / SbBranch / SbTextMeta all
            // share `FontFamily="{DynamicResource Fig.Font.Mono}"`
            // and `FontSize="11.5"`. Setting the font + size on the
            // root so every segment inherits — segments override
            // colour only.
            .font(theme::font_mono())
            .text_size(px(11.5))
            .text_color(ink_dim);

        // Segments are grouped into *clusters*; separators sit
        // between non-empty clusters only, matching the C#
        // `StatusBarView` layout where Rectangle separators appear at
        // specific boundaries (session→git, model-cluster→agents,
        // agents→groups, groups/workspace→bell) rather than between
        // every neighbouring segment.
        //
        // Left clusters:
        //   [session dot + branch] | [git diff, remote sync]
        // Right clusters:
        //   [model, tokens, turns, duration] | [agents] | [groups]
        //   | [workspace summary] | [bell]
        let left_clusters: Vec<Vec<gpui::AnyElement>> = {
            let mut clusters: Vec<Vec<gpui::AnyElement>> = Vec::new();
            // Empty-state wins over the session cluster — matches
            // C#: with 0 projects the bar reads "add a project to
            // begin." even if a shell tab happens to be open.
            if let Some(seg) = empty_state {
                clusters.push(vec![seg.into_any_element()]);
            } else if let Some(seg) = session_cluster {
                clusters.push(vec![seg.into_any_element()]);
            }
            let mut git_cluster: Vec<gpui::AnyElement> = Vec::new();
            if let Some(seg) = diff_segment {
                git_cluster.push(seg.into_any_element());
            }
            if let Some(seg) = remote_segment {
                git_cluster.push(seg.into_any_element());
            }
            if !git_cluster.is_empty() {
                clusters.push(git_cluster);
            }
            clusters
        };

        let right_clusters: Vec<Vec<gpui::AnyElement>> = {
            let mut clusters: Vec<Vec<gpui::AnyElement>> = Vec::new();
            let mut model_cluster: Vec<gpui::AnyElement> = Vec::new();
            if let Some(seg) = model_segment {
                model_cluster.push(seg.into_any_element());
            }
            if let Some(seg) = tokens_segment {
                model_cluster.push(seg.into_any_element());
            }
            if let Some(seg) = turns_segment {
                model_cluster.push(seg.into_any_element());
            }
            if let Some(seg) = duration_segment {
                model_cluster.push(seg.into_any_element());
            }
            if !model_cluster.is_empty() {
                clusters.push(model_cluster);
            }
            if let Some(seg) = agent_segment {
                clusters.push(vec![seg.into_any_element()]);
            }
            if let Some(gl) = group_label {
                clusters.push(vec![div().child(gl).into_any_element()]);
            }
            if let Some(seg) = workspace_summary {
                clusters.push(vec![seg.into_any_element()]);
            }
            clusters.push(vec![bell_btn.into_any_element()]);
            clusters
        };

        for (ci, cluster) in left_clusters.into_iter().enumerate() {
            if ci > 0 {
                bar = bar.child(sep());
            }
            for seg in cluster {
                bar = bar.child(seg);
            }
        }
        bar = bar.child(div().flex_grow());
        for (ci, cluster) in right_clusters.into_iter().enumerate() {
            if ci > 0 {
                bar = bar.child(sep());
            }
            for seg in cluster {
                bar = bar.child(seg);
            }
        }
        bar
    }

    /// Recompute the per-path "has an active session" / "has a busy
    /// session" snapshot and push it into the sidebar so worktree
    /// rows can colour their state dot (rest/idle/busy) and project
    /// rows can show the red propagation dot. Paths are canonicalised
    /// via `path_canon::canonicalize_path` so a tab's
    /// `working_directory` matches the projects.json worktree path
    /// regardless of slash direction / case / drive-colon spelling
    /// (Windows users routinely mix `C:\dev` and `c:/dev`).
    ///
    /// Mirrors the implicit data flow C# gets from
    /// `WorktreeViewModel`'s subscription to its child
    /// `SessionTabViewModel.Status` changes — without observable
    /// bindings on the Rust side, this method is the explicit "tick
    /// the sidebar's session-state cache" hook called from
    /// `start_telemetry_poll`. Sidebar `set_session_paths` is a no-op
    /// when neither set changed, so this is cheap to call every poll.
    fn push_sidebar_session_paths(&self, cx: &mut Context<Self>) {
        let mut busy: HashSet<String> = HashSet::new();
        let mut active: HashSet<String> = HashSet::new();
        for group in &self.groups {
            for tab in &group.tabs {
                let Some(ref wd) = tab.working_directory else { continue };
                let Some(sid) = tab.adopted_session_id.as_deref() else { continue };
                let canon = codescope_core::path_canon::canonicalize_path(
                    &wd.to_string_lossy(),
                );
                if canon.is_empty() {
                    continue;
                }
                // Every tab with an adopted session counts as
                // "active" for that path — the C# `HasActiveSession`
                // bool is just `Sessions.Count > 0`. Busy is the
                // subset whose telemetry state is `Busy` or
                // `PendingToolUse`.
                active.insert(canon.clone());
                if let Some(snap) = self.telemetry_for(sid) {
                    if matches!(
                        snap.state,
                        codescope_core::SessionState::Busy
                            | codescope_core::SessionState::PendingToolUse
                    ) {
                        busy.insert(canon);
                    }
                }
            }
        }
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_session_paths(busy, active, cx);
        });
    }

    /// Walk every tab across every group and count adopted agent
    /// sessions by activity state, regardless of agent backend.
    /// Mirrors C#'s `StatusAgentBusy` / `StatusAgentIdle`. Tabs without
    /// an `adopted_session_id` (plain pwsh, or agent tabs whose
    /// transcript hasn't surfaced yet) don't contribute to either
    /// count.
    fn agent_rollup_counts(&self) -> (u32, u32) {
        let mut busy = 0;
        let mut idle = 0;
        for group in &self.groups {
            for tab in &group.tabs {
                let Some(sid) = tab.adopted_session_id.as_deref() else {
                    continue;
                };
                let Some(snap) = self.telemetry_for(sid) else { continue };
                match snap.state {
                    codescope_core::SessionState::Busy
                    | codescope_core::SessionState::PendingToolUse => busy += 1,
                    codescope_core::SessionState::Idle => idle += 1,
                    codescope_core::SessionState::Unknown => {}
                }
            }
        }
        (busy, idle)
    }

    /// Push a toast onto the top of the floating stack. Each kind
    /// has its own lifetime — errors stay longer so the user can
    /// read / copy. New toasts go to the front so the stack reads
    /// newest-on-top.
    ///
    /// `pub(crate)` because the visible signature mentions
    /// `ToastKind` which is also crate-internal — the API is for
    /// internal use only (Sidebar routes through `SidebarEvent::Toast`
    /// rather than calling here directly).
    ///
    /// Cap-evicts at `TOAST_VISIBLE_CAP` so a flurry of actions
    /// can't grow the deque unboundedly. Drops the *back* (oldest)
    /// since the visible stack reads newest-first; the user has
    /// presumably already absorbed those.
    pub(crate) fn push_toast(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        detail: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        let lifetime = match kind {
            ToastKind::Ok => TOAST_LIFETIME_OK,
            ToastKind::Info => TOAST_LIFETIME_INFO,
            ToastKind::Err => TOAST_LIFETIME_ERR,
        };
        self.toasts.push_front(Toast {
            id,
            kind,
            title: title.into(),
            detail,
            expires_at: Instant::now() + lifetime,
        });
        while self.toasts.len() > TOAST_VISIBLE_CAP {
            self.toasts.pop_back();
        }
        cx.notify();
    }

    /// Push a persistent notification entry.  Unlike toasts these
    /// accumulate in the ring buffer until the user clears them or the
    /// ring reaches its cap (50).  Returns the id of the new entry.
    ///
    /// The bell button (integrating PR) wires this up for session events;
    /// callers can also call it directly for generic system events.
    #[allow(dead_code)]
    pub(crate) fn push_notification(
        &mut self,
        kind: crate::notifications::NotificationKind,
        title: impl Into<SharedString>,
        detail: impl Into<SharedString>,
        session_title: Option<SharedString>,
        cx: &mut Context<Self>,
    ) -> u64 {
        let id = self.notifications.push(kind, title, detail, session_title);
        cx.notify();
        id
    }

    /// Dismiss a toast immediately by id. Wired to a small `×` on
    /// each rendered toast so the user can clear them ahead of the
    /// auto-dismiss timer.
    fn dismiss_toast(&mut self, id: u64, cx: &mut Context<Self>) {
        let before = self.toasts.len();
        self.toasts.retain(|t| t.id != id);
        if self.toasts.len() != before {
            cx.notify();
        }
    }

    /// Open the tab right-click menu at `position` for the tab
    /// identified by `(group_id, tab_id)`.
    fn open_tab_menu(
        &mut self,
        group_id: u64,
        tab_id: u64,
        position: gpui::Point<gpui::Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.tab_menu = Some(TabMenu { group_id, tab_id, position });
        cx.notify();
    }

    fn close_tab_menu(&mut self, cx: &mut Context<Self>) {
        if self.tab_menu.take().is_some() {
            cx.notify();
        }
    }

    /// Close every tab in `group_id` *except* the one identified by
    /// `keep_tab_id`. Mirrors the C# build's "Close others" tab
    /// menu row. The kept tab becomes the group's active one.
    ///
    /// Resolves `keep_tab_id` *before* mutating: if the tab vanished
    /// between menu-open and click (rare but possible — concurrent
    /// close, drag-out), we abort silently. Without this guard the
    /// `retain` would drop every tab in the group and leave it in a
    /// broken empty state (it wouldn't even auto-collapse — that
    /// path lives in `close_tab`).
    fn close_other_tabs_in_group(
        &mut self,
        group_id: u64,
        keep_tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group_idx) = self.groups.iter().position(|g| g.id == group_id) else {
            self.close_tab_menu(cx);
            return;
        };
        // Refuse to mutate if the target tab is gone — preserves the
        // existing tab list rather than wiping it via `retain`.
        if !self.groups[group_idx].tabs.iter().any(|t| t.id == keep_tab_id) {
            self.close_tab_menu(cx);
            return;
        }
        self.groups[group_idx].tabs.retain(|t| t.id == keep_tab_id);
        // Only the kept tab remains — pin the active selection to it.
        let prev_focused = self.focused_group;
        self.groups[group_idx].active_tab = 0;
        self.activate_tab(group_idx, 0, window, cx);
        self.close_tab_menu(cx);
        // `activate_tab` only writes layout when focus changes. When
        // the menu was triggered on the already-focused group it
        // won't have saved, so we still need to here. When it *did*
        // change focus, `activate_tab` already saved and another
        // call would just be redundant disk I/O.
        if prev_focused == group_idx {
            self.save_layout();
        }
    }

    /// Close every tab in `group_id` whose position is to the right
    /// of `pivot_tab_id`. Mirrors the C# build's "Close all to the
    /// right" tab menu row. The pivot stays put.
    fn close_tabs_to_right_in_group(
        &mut self,
        group_id: u64,
        pivot_tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(group_idx) = self.groups.iter().position(|g| g.id == group_id) else {
            self.close_tab_menu(cx);
            return;
        };
        let Some(pivot_pos) = self.groups[group_idx]
            .tabs
            .iter()
            .position(|t| t.id == pivot_tab_id)
        else {
            self.close_tab_menu(cx);
            return;
        };
        self.groups[group_idx].tabs.truncate(pivot_pos + 1);
        // Active tab shifts to the pivot if it was past the new end.
        if self.groups[group_idx].active_tab > pivot_pos {
            self.groups[group_idx].active_tab = pivot_pos;
        }
        let prev_focused = self.focused_group;
        let active = self.groups[group_idx].active_tab;
        self.activate_tab(group_idx, active, window, cx);
        self.close_tab_menu(cx);
        // Only save here if `activate_tab` didn't — same dedupe as
        // `close_other_tabs_in_group`.
        if prev_focused == group_idx {
            self.save_layout();
        }
    }

    /// Reparent a tab from one group to another by id. Triggered by
    /// `on_drop` on a group's strip section after the user drags a
    /// tab out. The terminal entity is moved unchanged — no
    /// teardown / respawn — so the pty keeps running and any
    /// agent (claude, …) keeps its session.
    ///
    /// Looking up by id (not index) keeps us robust to concurrent
    /// list mutations between drag-start and drop. No-op when:
    /// - source / target group can't be resolved
    /// - source and target are the same group (within-group reorder
    ///   isn't supported yet — the user can already pick the tab
    ///   they want via click)
    fn move_tab_to_group(
        &mut self,
        source_group_id: u64,
        source_tab_id: u64,
        target_group_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if source_group_id == target_group_id {
            return;
        }
        let Some(source_idx) = self.groups.iter().position(|g| g.id == source_group_id)
        else {
            return;
        };
        let Some(target_idx) = self.groups.iter().position(|g| g.id == target_group_id)
        else {
            return;
        };
        let Some(tab_pos) = self.groups[source_idx]
            .tabs
            .iter()
            .position(|t| t.id == source_tab_id)
        else {
            return;
        };

        let tab = self.groups[source_idx].tabs.remove(tab_pos);

        // Slide the source group's `active_tab` down if we removed
        // at-or-before it; if the source group is now empty, collapse
        // it (mirrors `close_tab`'s collapse path) provided there's a
        // sibling.
        let source_now_empty = self.groups[source_idx].tabs.is_empty();
        if !source_now_empty {
            let g = &mut self.groups[source_idx];
            if g.active_tab >= g.tabs.len() {
                g.active_tab = g.tabs.len() - 1;
            } else if g.active_tab > tab_pos {
                g.active_tab -= 1;
            }
        }

        // Find target index again — `source_idx` might have shifted
        // if we collapsed below it (which we haven't yet, but be
        // defensive). We re-resolve by id here.
        let target_idx = self
            .groups
            .iter()
            .position(|g| g.id == target_group_id)
            .unwrap_or(target_idx.min(self.groups.len().saturating_sub(1)));
        self.groups[target_idx].tabs.push(tab);
        let new_active = self.groups[target_idx].tabs.len() - 1;
        self.groups[target_idx].active_tab = new_active;

        // Collapse the source group if it's empty and we have
        // siblings. After this the target_idx may shift; re-resolve.
        if source_now_empty && self.groups.len() > 1 {
            self.groups.remove(source_idx);
            if source_idx < self.group_weights.len() {
                self.group_weights.remove(source_idx);
            }
        }

        // Activate the moved tab in its new home so keyboard focus
        // follows the user's intent.
        let final_target_idx = self
            .groups
            .iter()
            .position(|g| g.id == target_group_id)
            .unwrap_or(0);
        let final_active = self.groups[final_target_idx].tabs.len().saturating_sub(1);
        self.activate_tab(final_target_idx, final_active, window, cx);
        self.save_layout();
    }

    /// Move focus to the group at `idx`. Routes keyboard focus to the
    /// group's currently-active tab so typing resumes in the right
    /// terminal. No-op when the index is out of range or the group is
    /// empty (an empty group has no terminal to focus — we still set
    /// `focused_group` so a subsequent Ctrl+T lands in this column).
    fn focus_group(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        if idx >= self.groups.len() {
            return;
        }
        if self.groups[idx].tabs.is_empty() {
            self.focused_group = idx;
            self.focus_handle.focus(window);
            cx.notify();
            return;
        }
        let active = self.groups[idx].active_tab;
        self.activate_tab(idx, active, window, cx);
    }

    fn on_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mods = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        // Cmd on macOS, Ctrl elsewhere — gpui already maps `platform`
        // to the right modifier per OS, so we just check both.
        let app_mod = mods.control || mods.platform;

        // Alt-only chords drive group-focus navigation (Alt+Left /
        // Right cycle, Alt+1..9 jump). Mirrors C#'s
        // `MainViewModel.FocusNextGroup` etc. Kept separate from the
        // Ctrl-based tab chords below so they don't conflict.
        if mods.alt && !app_mod {
            match key {
                "left" => {
                    cx.stop_propagation();
                    if self.focused_group > 0 {
                        self.focus_group(self.focused_group - 1, window, cx);
                    }
                    return;
                }
                "right" => {
                    cx.stop_propagation();
                    if self.focused_group + 1 < self.groups.len() {
                        self.focus_group(self.focused_group + 1, window, cx);
                    }
                    return;
                }
                d if d.len() == 1 => {
                    if let Some(n) = d.chars().next().and_then(|c| c.to_digit(10))
                        && (1..=9).contains(&n)
                    {
                        let idx = (n as usize) - 1;
                        if idx < self.groups.len() {
                            cx.stop_propagation();
                            self.focus_group(idx, window, cx);
                            return;
                        }
                    }
                }
                _ => return,
            }
        }

        if !app_mod || mods.alt {
            return;
        }
        // Bindings mirror C#'s `MainWindow.InputBindings`: Ctrl+T new
        // tab, Ctrl+W close tab, Ctrl+Tab / Ctrl+Shift+Tab cycle,
        // Ctrl+1..9 jump, Ctrl+\ split. Ctrl+Shift+T / Ctrl+Shift+W
        // are kept as alternates — they never collide with shell
        // word-shortcuts, so power users typing in readline /
        // PSReadLine can still hit the chord without rebinding.
        match key {
            "t" => {
                cx.stop_propagation();
                self.spawn_tab(window, cx);
            }
            "w" => {
                cx.stop_propagation();
                let g = self.focused_group;
                let group = self.focused_group();
                if group.tabs.is_empty() {
                    // Empty focused group — collapse it so the user
                    // can undo an accidental split right without
                    // having to spawn a tab first. Mirrors the
                    // `tab is null` branch of `MainViewModel.CloseTabAsync`.
                    // No-op when this is the only group; the user has
                    // to close their last tab to quit.
                    if self.groups.len() > 1 {
                        self.close_focused_group(window, cx);
                    }
                } else {
                    let t = group.active_tab;
                    self.close_tab(g, t, window, cx);
                }
            }
            // Ctrl+\ — split the focused group to the right. Matches
            // the C# binding (`SplitRightCommand`). Backslash is a
            // single-key chord on US/most layouts, so we hit it here
            // alongside the shifted variant in case of layouts that
            // treat the bare key as a different glyph.
            "\\" => {
                cx.stop_propagation();
                self.split_right(window, cx);
            }
            // Ctrl+B — toggle sidebar visibility. Matches VS Code +
            // most editors with a project tree, so the muscle memory
            // carries over.
            "b" if !mods.shift => {
                cx.stop_propagation();
                self.toggle_sidebar(cx);
            }
            // Ctrl+Shift+O — toggle the Overview panel. Mirrors the
            // C# build's `Ctrl+Shift+O` input binding (see
            // `MainViewModel.Palette` and `MainWindow.InputBindings`).
            "o" if mods.shift => {
                cx.stop_propagation();
                let next = !self.show_overview;
                self.set_show_overview(next, cx);
            }
            "tab" if !mods.shift => {
                cx.stop_propagation();
                self.next_tab(window, cx);
            }
            "tab" if mods.shift => {
                cx.stop_propagation();
                self.prev_tab(window, cx);
            }
            d if !mods.shift && d.len() == 1 => {
                if let Some(n) = d.chars().next().and_then(|c| c.to_digit(10))
                    && (1..=9).contains(&n)
                {
                    let idx = (n as usize) - 1;
                    let group_idx = self.focused_group;
                    if idx < self.groups[group_idx].tabs.len() {
                        cx.stop_propagation();
                        self.activate_tab(group_idx, idx, window, cx);
                    }
                }
            }
            _ => {}
        }
    }
}

impl Focusable for AppShell {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppShell {
    fn render(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = self.theme.clone();
        // Snapshot per-group + per-tab metadata up front so each
        // `cx.listener` closure can hold owned values without
        // overlapping the immutable borrow `self.groups.iter()` would
        // otherwise extend across the rest of `render`.
        let focused_group_idx = self.focused_group;
        let total_groups = self.groups.len();
        let groups_meta: Vec<GroupRenderData> = self
            .groups
            .iter()
            .enumerate()
            .map(|(g_idx, group)| GroupRenderData {
                group_idx: g_idx,
                group_id: group.id,
                active_tab: group.active_tab,
                is_focused: g_idx == focused_group_idx,
                is_last_group: g_idx + 1 == total_groups,
                weight: self.group_weights.get(g_idx).copied().unwrap_or(1.0),
                tabs: group
                    .tabs
                    .iter()
                    .enumerate()
                    .map(|(t_idx, tab)| {
                        // Resolve the tab's telemetry-derived busy
                        // state. Tabs without an adopted Claude
                        // session id have no telemetry to read and
                        // sit calm (`busy: false`), matching C#
                        // `SessionTabViewModel.Status`'s default of
                        // `TabStatus.Idle` for plain shell tabs.
                        let busy = tab
                            .adopted_session_id
                            .as_deref()
                            .and_then(|sid| self.telemetry_for(sid))
                            .map(|s| matches!(
                                s.state,
                                codescope_core::SessionState::Busy
                                    | codescope_core::SessionState::PendingToolUse
                            ))
                            .unwrap_or(false);
                        TabRenderData {
                            tab_idx: t_idx,
                            tab_id: tab.id,
                            title: tab.title.clone(),
                            busy,
                        }
                    })
                    .collect(),
                active_terminal: group
                    .tabs
                    .get(group.active_tab)
                    .map(|t| t.terminal.clone()),
            })
            .collect();

        // Caption controls: minimise, maximise/restore, close.
        // 46×40 hitboxes hugging the right edge of the caption row.
        //
        // On Windows the `WindowControlArea::*` annotations + the
        // gpui NC-mouse-up handler give us a *correct* maximize ↔
        // restore toggle natively (`zoom_window()` on Windows is
        // `SW_MAXIMIZE`-only with no restore path). We *also* wire
        // imperative `on_mouse_down` handlers as a defensive
        // fallback for clicks where the hit-test races the mouse
        // hitbox update — the native NC-up handler still toggles on
        // top of our minimize/close calls (idempotent), so the user
        // gets the right behaviour either way.
        //
        // On non-Windows targets the imperative handlers are the
        // *primary* path — there's no NC-button equivalent.
        let ink = theme::ink(&theme);
        let ink_dim = theme::ink_dim(&theme);
        let frost_hover = theme::frost_10(&theme);
        let close_hover_bg = theme::danger(&theme);
        let caption_base = move |id: &'static str, area: WindowControlArea, glyph: &'static str| {
            div()
                .id(id)
                .h(px(40.0))
                .w(px(46.0))
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.0))
                .text_color(ink_dim)
                .cursor_pointer()
                .window_control_area(area)
                .child(glyph)
        };

        // Caption-button click handlers. On Windows we send the
        // proper `WM_SYSCOMMAND` messages via the `win32_titlebar`
        // helper — that gives us a correct maximize ↔ restore
        // toggle (gpui's `zoom_window` is `SW_MAXIMIZE`-only with
        // no public restore path). Other platforms keep gpui's
        // `minimize_window` / `zoom_window` / `remove_window`.
        let minimize_btn = caption_base("titlebar-min", WindowControlArea::Min, "—")
            .hover(move |s| s.bg(frost_hover).text_color(ink));
        let maximize_btn = caption_base("titlebar-max", WindowControlArea::Max, "▢")
            .hover(move |s| s.bg(frost_hover).text_color(ink));
        let close_btn = caption_base("titlebar-close", WindowControlArea::Close, "✕")
            .hover(move |s| s.bg(close_hover_bg).text_color(ink));

        // The Win32 paths (`minimize` / `toggle_maximize` / `close` in
        // `win32_titlebar`) all post via `PostMessageW` — async,
        // non-blocking, no synchronous WndProc re-entry — so the
        // re-entrant-borrow problem `start_drag` had with `SendMessage`
        // doesn't apply here. The `window.defer` is kept anyway for
        // consistency with the title-bar drag region (which *does*
        // need it because `WM_NCLBUTTONDOWN(HTCAPTION)` enters the
        // OS's modal NC drag loop on the next pump iteration) and as
        // insurance against any future change to the win32 helper
        // that swaps `PostMessageW` for a synchronous send.
        #[cfg(target_os = "windows")]
        let minimize_btn = minimize_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, window, cx| {
                window.defer(cx, |window, _| crate::win32_titlebar::minimize(window));
            }),
        );
        #[cfg(target_os = "windows")]
        let maximize_btn = maximize_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, window, cx| {
                window.defer(cx, |window, _| crate::win32_titlebar::toggle_maximize(window));
            }),
        );
        #[cfg(target_os = "windows")]
        let close_btn = close_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, window, cx| {
                window.defer(cx, |window, _| crate::win32_titlebar::close(window));
            }),
        );

        #[cfg(not(target_os = "windows"))]
        let minimize_btn = minimize_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, window, _| window.minimize_window()),
        );
        #[cfg(not(target_os = "windows"))]
        let maximize_btn = maximize_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, window, _| window.zoom_window()),
        );
        #[cfg(not(target_os = "windows"))]
        let close_btn = close_btn.on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _, window, _| window.remove_window()),
        );

        // Split-right caption button — lives in the caption row left
        // of the min/max/close trio. Pure client-area button (no
        // `WindowControlArea` annotation) so `on_mouse_down` is the
        // primary click path on every platform.
        let split_btn = div()
            .id("titlebar-split")
            .h(px(40.0))
            .w(px(46.0))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(13.0))
            .text_color(ink_dim)
            .cursor_pointer()
            .hover(move |s| s.bg(frost_hover).text_color(ink))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    this.split_right(window, cx);
                }),
            )
            // Two thin verticals — pure-shape glyph, no font needed.
            // Mirrors C#'s `Ctx.Icon.SplitGroup`.
            .child(
                div()
                    .w(px(14.0))
                    .h(px(12.0))
                    .flex()
                    .flex_row()
                    .gap(px(2.0))
                    .child(
                        div()
                            .w(px(6.0))
                            .h_full()
                            .border_1()
                            .border_color(ink_dim)
                            .rounded_sm(),
                    )
                    .child(
                        div()
                            .w(px(6.0))
                            .h_full()
                            .border_1()
                            .border_color(ink_dim)
                            .rounded_sm(),
                    ),
            );

        // Brand mark — top-left of the caption row. Pure-shape port
        // of the C# splash's `.brand-mark` (accent rounded square
        // with a small black inset). Decorative; clicking it just
        // contributes to the drag region.
        let accent_clr = theme::accent(&theme);
        let brand_mark = div()
            .w(px(40.0))
            .h(px(40.0))
            .flex()
            .items_center()
            .justify_center()
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .w(px(20.0))
                    .h(px(20.0))
                    .rounded(px(5.0))
                    .bg(accent_clr)
                    .flex()
                    .flex_row()
                    .items_start()
                    .justify_end()
                    .p(px(4.0))
                    .child(
                        div()
                            .w(px(6.0))
                            .h(px(6.0))
                            .rounded(px(2.0))
                            .bg(gpui::black()),
                    ),
            );

        // Build the per-group strip sections + per-group panes in one
        // pass so the tab strip and the body row stay in lock-step (a
        // group is one column; both layers must agree on which group
        // is in which column).
        let group_count = groups_meta.len();
        let mut strip_sections: Vec<gpui::AnyElement> = Vec::with_capacity(group_count * 2);
        let mut group_panes: Vec<gpui::AnyElement> = Vec::with_capacity(group_count * 2);
        let divider_color = theme::divider(&theme);
        for (col_idx, gmeta) in groups_meta.into_iter().enumerate() {
            if col_idx > 0 {
                // The split lives between groups[col_idx-1] (left) and
                // groups[col_idx] (right). The strip layer gets a
                // pure-visual 1 px divider; the work-area layer gets a
                // wider interactive splitter so the user can grab it
                // without pixel-perfect aiming.
                let split_idx = col_idx - 1;
                // Strip divider — single-pixel rule. Matches the
                // visual width of the work-area splitter below
                // (which is also 1 px visible plus a 6 px absolute
                // hit overlay) so column boundaries in the tab
                // strip and the splitters between panes line up
                // pixel-for-pixel.
                strip_sections.push(
                    div()
                        .w(px(DIVIDER_VISUAL_WIDTH))
                        .h_full()
                        .bg(divider_color)
                        .into_any_element(),
                );
                // Splitter between two panes. Visually a single-
                // pixel rule painted in `divider_color`, with a
                // 6 px transparent hit-target overlaid on top so
                // the user can grab it without pixel-perfect
                // aiming. The overlay is absolute-positioned and
                // *extends beyond* the 1 px line into the adjacent
                // panes — that's the only way to get a wider grab
                // zone without making the visible line itself
                // wider (which would show two seams of canvas
                // colour around the line, the symptom the user
                // reported).
                let hit_overhang = (SPLITTER_HIT_WIDTH - DIVIDER_VISUAL_WIDTH) / 2.0;
                let splitter = div()
                    .id(("group-splitter", split_idx as u64))
                    .relative()
                    .w(px(DIVIDER_VISUAL_WIDTH))
                    .h_full()
                    .bg(divider_color)
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left(px(-hit_overhang))
                            .w(px(SPLITTER_HIT_WIDTH))
                            .h_full()
                            .cursor_col_resize()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, event: &gpui::MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.begin_splitter_drag(split_idx, event.position.x, window, cx);
                                }),
                            ),
                    );
                group_panes.push(splitter.into_any_element());
            }
            let (strip, pane) = self.render_group(&theme, &gmeta, cx);
            strip_sections.push(strip.into_any_element());
            group_panes.push(pane.into_any_element());
        }

        // ─── Title bar (40 px) ────────────────────────────────────
        // Single row, Chrome / VS Code / Windows Terminal layout:
        // tabs live in the title bar so we don't waste a second
        // chrome row.
        //
        // Caption controls (split, min, max, close) are
        // **absolute-positioned** in the top-right corner instead of
        // being normal flex children. Reason: the per-group tab
        // strips need to span the *same horizontal extent* as the
        // panes below them so column-dividers line up with
        // splitters — both sides have to divide the same total
        // width by the same `weight` factor. If the caption
        // controls were inline flex children they'd eat ~184 px from
        // the right of the tab area, the tab strips would compute
        // off a shorter total, and dividers would drift left of the
        // splitters. Floating them on top means the rightmost
        // group's strip extends to the window edge (its trailing
        // whitespace just sits visually under the caption controls);
        // tab/"+" content normally lives well to the left of the
        // overlap so it stays clickable.
        //
        // The brand cluster (mark + wordmark/version label) spans
        // the full width above the sidebar so the column header
        // visually "owns" the sidebar — the wordmark label grows
        // to fill `(sidebar_width + divider) - brand_mark` when the
        // sidebar is visible. When the sidebar is collapsed it
        // falls back to a fixed 150 px so the wordmark still has a
        // readable footprint. With the wordmark absorbing the
        // space the strip pad below stays a 0 px placeholder; the
        // brand cluster runs straight into the first tab.
        let brand_label_w = if self.sidebar_visible {
            (self.sidebar_width + DIVIDER_VISUAL_WIDTH - 40.0).max(BRAND_LABEL_FALLBACK_W)
        } else {
            BRAND_LABEL_FALLBACK_W
        };
        // The strip pad is no longer needed (the wordmark absorbs
        // the column width), but kept as a 0 px drag region so the
        // assembly below stays structurally identical.
        let strip_left_pad_w: f32 = 0.0;
        // Drag spots route through `handle_titlebar_press` for
        // shared single-vs-double-click discrimination.
        let strip_left_pad = div()
            .id("titlebar-strip-pad")
            .w(px(strip_left_pad_w))
            .h_full()
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.handle_titlebar_press(event, window, cx);
                }),
            );
        let brand_mark = brand_mark.id("titlebar-brand").on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                this.handle_titlebar_press(event, window, cx);
            }),
        );

        // Brand label + version slug — mirrors the C# title bar's
        // "CodeScope" wordmark + `V0.2.6` slug pair (MainWindow.xaml
        // lines 166–184). Sans 16/SemiBold for the wordmark, mono-
        // ish 10 dim for the slug; the slug is baked at build time
        // from `git describe --tags --always --dirty` (see build.rs)
        // so tagging a release flips every consumer automatically.
        // Both elements participate in the window-drag region so the
        // user can grab the chrome anywhere along the brand cluster.
        // `concat!` keeps the slug as a `&'static str` so the
        // caption-row hot path doesn't pay for a `format!` on
        // every render.
        const VERSION_DISPLAY: &str = concat!("V", env!("CODESCOPE_VERSION_DISPLAY"));
        // Fallback width when the sidebar is collapsed — the
        // wordmark + slug fit comfortably; long dev-build slugs
        // (`V0.2.6-52-g…-dirty`) get clipped via `overflow_hidden`
        // rather than pushing the tabs around. With the sidebar
        // visible the label grows to span the column (see
        // `brand_label_w` above).
        const BRAND_LABEL_FALLBACK_W: f32 = 150.0;
        let brand_label = div()
            .id("titlebar-brand-label")
            .w(px(brand_label_w))
            .h(px(40.0))
            .pl(px(2.0))
            .pr(px(10.0))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .overflow_hidden()
            .window_control_area(WindowControlArea::Drag)
            .child(
                // CodeScope wordmark — `Fig.Font.Sans` @
                // `FontSize="16"` SemiBold, see `MainWindow.xaml`.
                div()
                    .font(theme::font_sans())
                    .text_size(px(16.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::ink(&theme))
                    .child("CodeScope"),
            )
            .child(
                // Version slug — `Fig.Font.Mono` @ `FontSize="10"` in
                // `MainWindow.xaml` (Text.Faint foreground; the closest
                // themable analogue here is `ink_ghost`).
                div()
                    .font(theme::font_mono())
                    .text_size(px(10.0))
                    .text_color(theme::ink_ghost(&theme))
                    .truncate()
                    .child(VERSION_DISPLAY),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.handle_titlebar_press(event, window, cx);
                }),
            );

        // While the Overview is visible the per-group tab strip is
        // structurally meaningless — its only target (the group
        // grid below) is hidden. Render an empty placeholder so the
        // caption row keeps the same layout footprint but doesn't
        // surface stale tab affordances.
        let tab_strip_inline = if self.show_overview {
            div()
                .flex()
                .flex_row()
                .flex_grow()
                .h_full()
        } else {
            div()
                .flex()
                .flex_row()
                .flex_grow()
                .h_full()
                .children(strip_sections)
        };
        // Right-side caption-controls cluster — absolute-positioned
        // so it overlays the rightmost portion of the tab area
        // without consuming flex width. See the long comment above
        // for the alignment rationale.
        //
        // Opaque `bg(elevated)` so any tab content from the
        // rightmost group's strip that bleeds underneath (when the
        // user drags a divider hard right and tabs no longer fit in
        // the reduced strip) is hidden behind the buttons instead
        // of poking through. The reserve+overflow_hidden inside the
        // last-group strip is the *primary* clip; this opaque
        // overlay is the safety net for the cases gpui's flex
        // layout doesn't fully shrink the inner content past its
        // children's `min_w`.
        let caption_controls = div()
            .absolute()
            .top_0()
            .right_0()
            .h(px(40.0))
            .flex()
            .flex_row()
            .bg(theme::elevated(&theme))
            .child(split_btn)
            .child(minimize_btn)
            .child(maximize_btn)
            .child(close_btn);
        let caption_row = div()
            .relative()
            .h(px(40.0))
            .flex()
            .flex_row()
            .border_b_1()
            .border_color(theme::divider(&theme))
            .bg(theme::elevated(&theme))
            .child(brand_mark)
            .child(brand_label)
            .child(strip_left_pad)
            .child(tab_strip_inline)
            .child(caption_controls);

        // Work area swap: when the Overview panel is up, hide the
        // group strip + terminal grid entirely and render the
        // Overview in their place. The sidebar + status bar stay
        // anchored on either side / below so the user can dismiss
        // via the same sidebar button. Mirrors the C# build's
        // `IsOverviewVisible` DataTrigger swap in `MainWindow.xaml`.
        let work_area: gpui::AnyElement = if self.show_overview {
            self.render_overview(&theme, cx).into_any_element()
        } else {
            div()
                .flex_grow()
                .flex()
                .flex_row()
                .children(group_panes)
                .into_any_element()
        };

        // Sidebar wrapper — sized by AppShell so we can drag-resize
        // and collapse without poking the Sidebar entity. Hidden
        // entirely (zero-width child) when `sidebar_visible` is
        // false; the toggle in the titlebar brings it back. The
        // 6 px right-edge handle is the resize hit-target — same
        // width / cursor / drag pattern as the group splitter.
        let sidebar_drag_color = theme::divider(&theme);
        // Sidebar resize handle — same single-pixel-with-wide-hit
        // overlay pattern as the group splitter. See the long
        // comment on `splitter` for the rationale.
        let sidebar_hit_overhang = (SPLITTER_HIT_WIDTH - DIVIDER_VISUAL_WIDTH) / 2.0;
        let sidebar_handle = div()
            .id("sidebar-resize-handle")
            .relative()
            .w(px(DIVIDER_VISUAL_WIDTH))
            .h_full()
            .bg(sidebar_drag_color)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left(px(-sidebar_hit_overhang))
                    .w(px(SPLITTER_HIT_WIDTH))
                    .h_full()
                    .cursor_col_resize()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.begin_sidebar_drag(event.position.x, cx);
                        }),
                    ),
            );

        let main_row = if self.sidebar_visible {
            div()
                .flex_grow()
                .flex()
                .flex_row()
                .child(
                    div()
                        .w(px(self.sidebar_width))
                        .h_full()
                        .flex_shrink_0()
                        .child(self.sidebar.clone()),
                )
                .child(sidebar_handle)
                .child(work_area)
        } else {
            div()
                .flex_grow()
                .flex()
                .flex_row()
                .child(work_area)
        };

        // While a splitter drag is in flight we listen for mouse
        // moves anywhere in the window (the cursor commonly leaves
        // the 6 px hit-target during a fast drag) and clamp them
        // against the original snapshot. Mouse up — anywhere —
        // commits the new weights. We attach both handlers
        // unconditionally because gpui doesn't have an `is_some()`-
        // gated handler primitive; the closures are tiny and bail
        // when no drag is in flight.
        div()
            .key_context("AppShell")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if this.splitter_drag.is_some() {
                    this.update_splitter_drag(event.position.x, cx);
                }
                if this.sidebar_drag.is_some() {
                    this.update_sidebar_drag(event.position.x, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.end_splitter_drag(cx);
                    this.end_sidebar_drag(cx);
                }),
            )
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::canvas(&theme))
            .text_color(theme::ink(&theme))
            .child(caption_row)
            .child(main_row)
            .child(self.render_status_bar(&theme, cx))
            .children(self.render_tab_menu(&theme, cx))
            .children(self.render_toasts(&theme, cx))
            .children(self.render_notifications_popover(&theme, cx))
    }
}

/// Frame-local snapshot for one tab inside a group. Owned strings so
/// the listener closures can `move` the title without keeping a borrow
/// on `self.groups`.
struct TabRenderData {
    tab_idx: usize,
    tab_id: u64,
    title: SharedString,
    /// Tab activity state derived from the latest telemetry snapshot,
    /// mirroring C# `SessionTabViewModel.Status` (`TabStatus.Idle` /
    /// `TabStatus.Busy`). Drives the colour of the 6 px dot on the
    /// tab strip — green for idle / no-telemetry, red for the agent
    /// composing or paused on a tool call. Snapshotted up front so
    /// the per-tab render closure doesn't have to re-borrow
    /// `self.telemetry_tails`.
    busy: bool,
}

/// Frame-local snapshot for one group. We snapshot `Entity<TerminalView>`
/// for the active tab so the body row can render it without
/// re-borrowing `self.groups` after `groups_meta` is moved into the
/// per-group render loop.
struct GroupRenderData {
    group_idx: usize,
    group_id: u64,
    active_tab: usize,
    is_focused: bool,
    /// True for the rightmost group in the workspace. The strip for
    /// the last group reserves the trailing
    /// `CAPTION_CTRLS_W` pixels for the absolute-positioned caption
    /// controls (split / min / max / close) and clips its tab/"+"
    /// content with `overflow_hidden` so tabs can't slide *under*
    /// the buttons when the user drags a divider hard right.
    is_last_group: bool,
    /// Flex weight for this group's column; mapped to `flex_grow` on
    /// both the strip section and the pane below so they stay
    /// column-aligned. `1.0` = equal share, `1.5` = 1.5× a sibling at
    /// `1.0`. Snapshotted from `AppShell.group_weights` at render time.
    weight: f32,
    tabs: Vec<TabRenderData>,
    active_terminal: Option<Entity<TerminalView>>,
}

impl AppShell {
    /// Build one group's tab strip section + body pane. Returned as a
    /// pair so `render` can interleave dividers between adjacent
    /// groups while keeping the strip and the pane below it in the
    /// same column.

    /// Handle a left-press on any of the title-bar drag regions
    /// (brand mark, strip-left padding, per-group trailing
    /// whitespace). Discriminates real human clicks from the
    /// *synthetic* `WM_NCLBUTTONDOWN` echo our own `start_drag` posts:
    ///
    /// 1. Sub-10 ms after the previous press ⇒ synthetic, ignore.
    /// 2. 10–500 ms after the previous press ⇒ real double-click,
    ///    toggle maximize.
    /// 3. Otherwise ⇒ first press of a real click sequence, start
    ///    the OS drag (or window-move on non-Windows).
    ///
    /// On non-Windows there's no synthetic echo because we use
    /// `start_window_move` instead of `PostMessage`, but the same
    /// time-delta logic still gives us double-click → zoom_window.
    #[allow(unused_variables)]
    fn handle_titlebar_press(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let now = std::time::Instant::now();
        let prev = self.last_titlebar_press_at;
        self.last_titlebar_press_at = Some(now);

        if let Some(prev_t) = prev {
            let dt = now.duration_since(prev_t);
            if dt < std::time::Duration::from_millis(10) {
                // Synthetic echo from our own PostMessage(WM_NCLBUTTONDOWN).
                // Roll the timestamp back so a *real* second click
                // measures from the original press, not from the echo.
                self.last_titlebar_press_at = Some(prev_t);
                return;
            }
            if dt < std::time::Duration::from_millis(500) {
                #[cfg(target_os = "windows")]
                window.defer(cx, |window, _| {
                    crate::win32_titlebar::toggle_maximize(window);
                });
                #[cfg(not(target_os = "windows"))]
                window.zoom_window();
                return;
            }
        }

        #[cfg(target_os = "windows")]
        window.defer(cx, |window, _| {
            crate::win32_titlebar::start_drag(window);
        });
        #[cfg(not(target_os = "windows"))]
        window.start_window_move();
    }

    fn render_group(
        &self,
        theme: &Arc<Theme>,
        gmeta: &GroupRenderData,
        cx: &mut Context<Self>,
    ) -> (gpui::Stateful<gpui::Div>, gpui::Stateful<gpui::Div>) {
        let group_idx = gmeta.group_idx;
        let group_id = gmeta.group_id;
        let active_tab = gmeta.active_tab;
        let is_focused = gmeta.is_focused;
        let frost_10 = theme::frost_10(theme);
        let frost_20 = theme::frost_20(theme);
        let canvas = theme::canvas(theme);
        let elevated = theme::elevated(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let accent = theme::accent(theme);
        let divider = theme::divider(theme);

        let theme_for_drag = theme.clone();
        // Tab-strip status dot colours — 6 px dot per tab, mirrors
        // C# `GroupStripView`'s `TabStatusToBrushConverter`. Busy
        // ⇒ Signal.Warn (red, agent composing / paused on tool),
        // idle ⇒ Signal.Ok (green, awaiting your input). Sourced via
        // `theme::signal_*` so the values stay in sync with
        // DesignTokens.xaml without hard-coded hex.
        let signal_ok = theme::signal_ok();
        let signal_warn = theme::signal_warn();
        let tabs = gmeta.tabs.iter().map(|tmeta| {
            let tab_idx = tmeta.tab_idx;
            let tab_id = tmeta.tab_id;
            let title = tmeta.title.clone();
            let active = tab_idx == active_tab && is_focused;
            let card = tab_idx == active_tab;
            let bg = if card { canvas } else { gpui::transparent_black() };
            let text_color = if active { ink } else { ink_dim };
            let top_border = if active { accent } else { gpui::transparent_black() };
            // Status dot: green when idle / no telemetry, red when
            // the agent is busy (Composing / PendingToolUse). Selection
            // chrome lives elsewhere (text colour, top accent rail) so
            // the dot is reserved for agent state, exactly the way C#
            // `GroupStripView` keeps the dot tied to `Status` rather
            // than `IsSelected`.
            let status_dot = if tmeta.busy { signal_warn } else { signal_ok };
            // Drag payload — stable ids + the title so the drag
            // preview can render without holding a borrow on
            // `self.groups`.
            let drag_payload = TabDragData {
                source_group_id: group_id,
                source_tab_id: tab_id,
                title: title.clone(),
            };
            let title_for_drag = title.clone();
            let theme_for_preview = theme_for_drag.clone();
            div()
                .id(("tab", tab_id))
                .h_full()
                .min_w(px(140.0))
                .max_w(px(240.0))
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_3()
                .border_t_2()
                .border_color(top_border)
                .bg(bg)
                .text_color(text_color)
                // Tab title font — `Fig.Font.Sans` @ `FontSize="13"`
                // from `GroupStripView.xaml`. The C# template also
                // dials FontWeight from 340 (unselected) to Medium
                // (selected); gpui's variable-axis support is uneven
                // across platforms so we keep a single weight here
                // and rely on the ink_dim → ink contrast to mark the
                // active tab.
                .font(theme::font_sans())
                .text_size(px(13.0))
                .hover(move |s| {
                    if active { s } else { s.bg(frost_10).text_color(ink) }
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, window, cx| {
                        this.activate_tab(group_idx, tab_idx, window, cx);
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(
                        move |this, event: &gpui::MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            this.open_tab_menu(group_id, tab_id, event.position, cx);
                        },
                    ),
                )
                // Make the tab draggable. The constructor builds a
                // fresh `DraggedTab` view that gpui paints attached
                // to the cursor for the duration of the drag. The
                // payload (`TabDragData`) is what `on_drop` sees on
                // the target strip section.
                .on_drag(drag_payload, move |_payload, _offset, _window, cx| {
                    let theme = theme_for_preview.clone();
                    let title = title_for_drag.clone();
                    cx.new(|_| DraggedTab { title, theme })
                })
                .child(
                    div()
                        .w(px(8.0))
                        .h(px(8.0))
                        .rounded_full()
                        .bg(status_dot),
                )
                .child(div().flex_grow().truncate().child(title))
                .child(
                    div()
                        .id(("close", tab_id))
                        .w(px(20.0))
                        .h(px(20.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        // Close glyph — `FontSize="14"` on the WPF
                        // `Button.Template` `TextBlock` (see
                        // `GroupStripView.xaml`).
                        .text_size(px(14.0))
                        .text_color(ink_ghost)
                        .hover(move |s| s.bg(frost_20).text_color(ink))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.close_tab(group_idx, tab_idx, window, cx);
                            }),
                        )
                        .child("×"),
                )
        });

        // Per-strip "+" button — same height/width as the cold-start
        // singleton's, but lives inside the strip section so each group
        // gets its own. Click spawns into *this* group regardless of
        // which one is currently focused.
        let new_tab_frost = frost_10;
        let new_tab_accent = accent;
        let new_tab_button = div()
            .id(("new-tab", group_id))
            .h(px(40.0))
            .w(px(40.0))
            .flex()
            .items_center()
            .justify_center()
            .text_color(ink_dim)
            // `+` glyph — `FontSize="20"` on the new-session Button in
            // `GroupStripView.xaml`. The button is 40×40 on Rust
            // (vs 32×32 in C# since the Rust strip is merged with
            // the title-bar row); the glyph itself stays at 20.
            .text_size(px(20.0))
            .cursor_pointer()
            .hover(move |s| s.bg(new_tab_frost).text_color(new_tab_accent))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    // Spawning routes through the currently-focused
                    // group, so focus this one first to make the click
                    // do what the user expects (spawn here, not
                    // wherever focus happened to be).
                    this.focus_group(group_idx, window, cx);
                    this.spawn_tab(window, cx);
                }),
            )
            .child("+");

        // Strip section: tabs + "+" button. Click anywhere in the
        // remaining whitespace focuses the group so the next Ctrl+T
        // lands here. Drop a tab on it and the tab moves to this
        // group (`on_drop` fires when the user releases a
        // `TabDragData` over this hitbox).
        //
        // `flex_grow` is set via `style().flex_grow = Some(weight)`
        // because gpui's chainable `.flex_grow()` only sets the value
        // to 1.0 — we need arbitrary weights for the column layout.
        let target_group_id = group_id;
        // Empty trailing region inside the strip. Doubles as a drag
        // region in the merged title bar — the rightmost group's
        // trailing whitespace is the gap under the caption controls,
        // and intermediate groups' trailing whitespace is the
        // "between tabs and next group's divider" gap. Annotated as
        // `WindowControlArea::Drag` and wired with the same Win32
        // start_drag + double-click→toggle_maximize handlers as the
        // brand-mark/strip_left_pad drag spots.
        // Drag-region wiring routes through `handle_titlebar_press`,
        // which time-discriminates real clicks from the synthetic
        // `WM_NCLBUTTONDOWN` echo our own `start_drag` posts. See
        // that method for the full rationale.
        let trailing_drag = div()
            .id(("strip-trailing", group_id))
            .flex_grow()
            .h_full()
            .window_control_area(WindowControlArea::Drag)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.handle_titlebar_press(event, window, cx);
                }),
            );
        // For the rightmost group we wrap the tab content (tabs +
        // "+" + trailing drag) in an inner flex-row with
        // `overflow_hidden`, then add a fixed-width transparent
        // reserve sibling whose width matches the caption-controls
        // cluster. The reserve sits exactly under the absolute-
        // positioned caption controls; the inner content area is
        // therefore bounded to `strip_width - CAPTION_CTRLS_W` and
        // any overflow (when the user drags a divider hard right and
        // tabs no longer fit) is clipped at the buttons' left edge
        // instead of bleeding under them. Non-last groups don't need
        // this — their right edge is the divider before the next
        // group, well clear of the caption-controls overlay.
        let mut strip = div()
            .id(("group-strip", group_id))
            .h_full()
            .flex()
            .flex_row()
            .flex_shrink()
            .bg(elevated)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.focus_group(group_idx, window, cx);
                }),
            )
            .on_drop(
                cx.listener(move |this, payload: &TabDragData, window, cx| {
                    this.move_tab_to_group(
                        payload.source_group_id,
                        payload.source_tab_id,
                        target_group_id,
                        window,
                        cx,
                    );
                }),
            );
        if gmeta.is_last_group {
            let mut content = div()
                .h_full()
                .flex()
                .flex_row()
                .overflow_hidden()
                .children(tabs)
                .child(new_tab_button)
                .child(trailing_drag);
            content.style().flex_grow = Some(1.0);
            content.style().flex_basis = Some(gpui::Length::Definite(px(0.0).into()));
            content.style().min_size.width = Some(gpui::Length::Definite(px(0.0).into()));
            let reserve = div()
                .w(px(CAPTION_CTRLS_W))
                .h_full()
                .flex_shrink_0();
            strip = strip.child(content).child(reserve);
        } else {
            strip = strip
                .children(tabs)
                .child(new_tab_button)
                .child(trailing_drag);
        }
        strip.style().flex_grow = Some(gmeta.weight);
        strip.style().flex_basis = Some(gpui::Length::Definite(px(0.0).into()));

        // Pane: active tab's terminal, or a black void when the group
        // has no tabs (split-right + Ctrl+T-not-yet-pressed). 2 px
        // accent rail at the top of the focused group's pane mirrors
        // the C# `EditorGroupView` `IsFocused` cue.
        let rail_color = if is_focused { accent } else { divider };
        let body_inner: gpui::AnyElement = if let Some(term) = gmeta.active_terminal.as_ref() {
            div().size_full().child(term.clone()).into_any_element()
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(ink_ghost)
                .text_size(px(12.0))
                .child("Empty group · press Ctrl+Shift+T to open a tab")
                .into_any_element()
        };
        let mut pane = div()
            .id(("group-pane", group_id))
            .h_full()
            .flex()
            .flex_col()
            .flex_shrink()
            .border_t_2()
            .border_color(rail_color)
            .bg(canvas)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.focus_group(group_idx, window, cx);
                }),
            )
            .child(body_inner);
        pane.style().flex_grow = Some(gmeta.weight);
        pane.style().flex_basis = Some(gpui::Length::Definite(px(0.0).into()));

        (strip, pane)
    }
}

// ─── Window-state extraction ────────────────────────────────────────

/// Snapshot the platform window into the on-disk `WindowState`. We
/// always store the *restore* bounds so unmaximising lands at a
/// sensible size; `maximised` is a separate flag the loader uses to
/// pick `WindowBounds::Maximized` vs `Windowed`.
fn window_state_from_window(window: &Window) -> WindowState {
    let restore = match window.window_bounds() {
        WindowBounds::Windowed(b) => b,
        WindowBounds::Maximized(b) => b,
        WindowBounds::Fullscreen(b) => b,
    };
    let f32_x: f32 = restore.origin.x.into();
    let f32_y: f32 = restore.origin.y.into();
    let f32_w: f32 = restore.size.width.into();
    let f32_h: f32 = restore.size.height.into();
    WindowState {
        x: f32_x as i32,
        y: f32_y as i32,
        width: f32_w.max(0.0) as u32,
        height: f32_h.max(0.0) as u32,
        maximised: window.is_maximized(),
    }
}

// ─── Settings → terminal-config helpers ─────────────────────────────

fn cursor_shape_from_str(s: &str) -> codescope_terminal::CursorShape {
    use codescope_terminal::CursorShape;
    match s {
        "block" => CursorShape::Block,
        "underline" | "underscore" => CursorShape::Underline,
        "hollow-block" | "hollow_block" => CursorShape::HollowBlock,
        // "beam", anything else → beam (Windows-Terminal default).
        _ => CursorShape::Beam,
    }
}

fn build_font_config(settings: &Settings) -> FontConfig {
    let env_family = std::env::var("CODESCOPE_FONT").ok();
    let family_candidates = terminal_font_candidates(settings, env_family.as_deref());
    let family = theme::resolve_font_family(&family_candidates)
        .unwrap_or_else(|| FontConfig::default().family);
    let fallbacks = settings.font.fallbacks.iter().map(|s| s.clone().into()).collect();
    let size = px(settings.font.size.max(1.0));
    // line_height_multiplier is recorded for later — gpui's
    // text_system measures line height directly, so we just stash
    // a placeholder here. A multiplier knob lands when the renderer
    // exposes it.
    let line_height = px(settings.font.size * settings.font.line_height_multiplier.max(0.5));
    FontConfig {
        family,
        fallbacks,
        size,
        line_height,
        cell_width: px(settings.font.size * 0.6),
    }
}

fn terminal_font_candidates(settings: &Settings, env_family: Option<&str>) -> Vec<String> {
    let mut candidates = Vec::new();
    push_non_empty_font_candidate(&mut candidates, &settings.font.family);

    if let Some(env_family) = env_family {
        push_non_empty_font_candidate(&mut candidates, env_family);
    }

    for fallback in &settings.font.fallbacks {
        push_non_empty_font_candidate(&mut candidates, fallback);
    }

    if candidates.is_empty() {
        let defaults = FontSettings::default();
        push_non_empty_font_candidate(&mut candidates, &defaults.family);
        for fallback in defaults.fallbacks {
            push_non_empty_font_candidate(&mut candidates, &fallback);
        }
    }

    candidates
}

fn push_non_empty_font_candidate(candidates: &mut Vec<String>, family: &str) {
    let family = family.trim();
    if !family.is_empty() {
        candidates.push(family.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_font_candidates_keep_settings_ahead_of_env() {
        let mut settings = Settings::default();
        settings.font.family = "Settings Mono".into();
        settings.font.fallbacks = vec!["Fallback Mono".into()];

        let candidates = terminal_font_candidates(&settings, Some("Env Mono"));

        assert_eq!(
            candidates,
            vec![
                "Settings Mono".to_string(),
                "Env Mono".to_string(),
                "Fallback Mono".to_string(),
            ]
        );
    }

    #[test]
    fn terminal_font_candidates_restore_mono_defaults_when_user_clears_everything() {
        let mut settings = Settings::default();
        settings.font.family = "  ".into();
        settings.font.fallbacks = vec!["".into(), "  ".into()];

        let candidates = terminal_font_candidates(&settings, None);
        let defaults = FontSettings::default();

        assert_eq!(candidates.first(), Some(&defaults.family));
        assert!(candidates.contains(&"Cascadia Mono".to_string()));
        assert!(candidates.contains(&"Consolas".to_string()));
    }

    #[test]
    fn path_eq_ci_handles_trailing_slash_on_every_platform() {
        // Trailing-separator stripping is platform-independent — both
        // POSIX `/` and Windows `\` get trimmed regardless of host.
        assert!(path_eq_ci("/usr/local/bin/", "/usr/local/bin"));
        assert!(path_eq_ci("C:\\Repos\\Foo\\", "C:\\Repos\\Foo"));
        assert!(!path_eq_ci("foo", "foobar"));
    }

    #[cfg(windows)]
    #[test]
    fn path_eq_ci_on_windows_is_case_insensitive() {
        assert!(path_eq_ci("C:\\Repos\\Foo", "c:\\repos\\foo"));
        assert!(!path_eq_ci("C:\\Repos\\Foo", "C:\\Repos\\Bar"));
    }

    #[cfg(not(windows))]
    #[test]
    fn path_eq_ci_off_windows_is_case_sensitive() {
        // Linux ext4 / case-sensitive APFS treat "/Repo" and "/repo"
        // as distinct paths — comparing case-insensitively here would
        // route a tab to the wrong project.
        assert!(!path_eq_ci("/repos/Foo", "/repos/foo"));
        assert!(path_eq_ci("/repos/foo", "/repos/foo"));
    }
}
