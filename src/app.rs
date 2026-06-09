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
//! Visuals follow `legacy:CodeScope.App/Styles/DesignTokens.xaml`:
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
    AppContext, ClipboardItem, Context, Entity, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, MouseButton, ParentElement, Render, SharedString, Styled, Window,
    WindowBounds, WindowControlArea, div, px, svg,
};
use parking_lot::Mutex;

use crate::sidebar::{
    RenameRequest, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH, Sidebar,
    SidebarEvent, ToastSeverity,
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
/// Period of the dialog-input caret blink. 530 ms is the conventional
/// rate the C# WPF TextBox uses (and matches the terminal pane's own
/// blink in `terminal/src/view.rs`). The phase is shared across every
/// input on screen so they all flip in lockstep — half a second of
/// drift between two adjacent fields would feel unsynced.
pub(crate) const TEXT_BLINK_PERIOD: Duration = Duration::from_millis(530);

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

/// Rescale `weights` so the sum equals the slice length (i.e. mean is
/// `1.0`) while preserving ratios. Used after any structural removal
/// from `AppShell::group_weights` to keep the sum at `len() ≥ 1` —
/// taffy's flex implementation truncates the distributed free-space
/// to `sum_flex_grow * free_space` when `sum_flex_grow < 1.0`
/// (`taffy-0.9.0/src/compute/flexbox.rs:1260`), so a survivor with
/// weight `0.5` after a heavy splitter drag would otherwise occupy
/// only half the work-area width and leave the other half blank.
///
/// Defensive on degenerate inputs: empty slices and non-finite or
/// non-positive sums both fall back to assigning `1.0` to every
/// element so the layout still gets a sane sum.
///
/// **Ratio preservation is best-effort:** results below
/// `MIN_GROUP_WEIGHT` are clamped to that floor, which slightly
/// shifts the ratios in the (rare) case where rescaling would
/// produce a sub-minimum weight. In practice this only fires after
/// extreme drag combinations and the user is dragging anyway — the
/// floor keeps every column visible.
fn normalise_group_weights(weights: &mut [f32]) {
    let count = weights.len();
    if count == 0 {
        return;
    }
    let sum: f32 = weights.iter().copied().sum();
    if !sum.is_finite() || sum <= 0.0 {
        for w in weights.iter_mut() {
            *w = 1.0;
        }
        return;
    }
    let scale = count as f32 / sum;
    for w in weights.iter_mut() {
        *w = (*w * scale).max(MIN_GROUP_WEIGHT);
    }
}
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
///
#[derive(Clone)]
struct Toast {
    id: u64,
    kind: ToastKind,
    title: SharedString,
    detail: Option<SharedString>,
    /// `None` for persistent toasts (action-bearing ones). The
    /// auto-dismiss task skips these; the user must click the action
    /// or the explicit × to clear.
    expires_at: Option<Instant>,
    action: Option<ToastAction>,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum ToastKind {
    Ok,
    Err,
    Info,
}

/// Action button payload attached to a toast. When present, the toast
/// renders an extra button with `label` and disables auto-dismiss
/// (the user must act on it or click the × explicitly).
#[derive(Clone, Debug)]
pub(crate) struct ToastAction {
    pub label: SharedString,
    pub kind: ToastActionKind,
}

/// What clicking a toast-action button does. Restricted to the
/// update flow's exact needs; expand only when a new caller appears.
#[derive(Clone, Debug)]
pub(crate) enum ToastActionKind {
    /// Start downloading + atomic-swapping the binary for this
    /// release. macOS callers should use `OpenReleasesPage` instead —
    /// Gatekeeper blocks unsigned swaps inside `.app` bundles.
    ApplyUpdate(codescope_core::update_check::ReleaseInfo),
    /// Atomic swap is done; click closes the app so the user can
    /// re-launch the new binary.
    RestartForUpdate,
    /// Open the GitHub releases page in the default browser.
    OpenReleasesPage,
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

/// Telemetry transcript-tail poll cadence while at least one tail
/// is registered. Matches the C# `ClaudeTelemetryService` 250 ms
/// flat poll — keeps the idle → busy transition latency under one
/// tick instead of the previous 2 s adaptive cadence (the user
/// reported a ~2 s lag before the wait pulse appeared).
const TELEMETRY_POLL_ACTIVE: Duration = Duration::from_millis(250);
/// Telemetry poll cadence when no tails are registered. The C#
/// build disarms its timer entirely here; we keep the task alive
/// (avoids a wakeup/teardown dance per tab) but throttle to a
/// 30 s heartbeat so an idle CodeScope doesn't burn cycles.
const TELEMETRY_POLL_IDLE: Duration = Duration::from_secs(30);
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
}

/// Live drop-cursor state: which group's strip is currently being
/// hovered by an in-flight tab drag, and at what slot (between which
/// two tabs) the user would drop. Cleared on drop and on drag exit.
///
/// Updated each frame by the `on_drag_move` handler on the strip
/// element: the strip-relative cursor X is fed to
/// [`compute_drop_index`] together with the snapshotted per-tab
/// rects, and the result drives both the 3 px drop-indicator
/// position and the index passed to `move_tab_to_group` on drop.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TabDropHover {
    /// Stable id of the group whose strip the cursor is over.
    group_id: u64,
    /// Insert position in `0..=tabs.len()` of the target group.
    drop_index: usize,
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
        // Drag-chip styling — mirrors the HTML spec at
        // `docs/design/html/CodeScope - Tab Drag.html`:
        //   - 1 px accent border
        //   - dark canvas fill
        //   - 3 px translucent accent "inner ring" + 24 px soft blue
        //     outer glow via stacked box-shadows
        //   - -1.5° rotation (NOT applied — gpui 0.2.x exposes
        //     `with_rotation` only on SVG elements, not on `div`;
        //     deferred until a future gpui release adds transform
        //     support to the styled-element surface).
        let accent = theme::accent(&self.theme);
        let glow_inner = gpui::BoxShadow {
            color: gpui::hsla(accent.h, accent.s, accent.l, 0.20),
            offset: gpui::point(px(0.0), px(0.0)),
            blur_radius: px(0.0),
            spread_radius: px(3.0),
        };
        let glow_outer = gpui::BoxShadow {
            color: gpui::hsla(accent.h, accent.s, accent.l, 0.35),
            offset: gpui::point(px(0.0), px(0.0)),
            blur_radius: px(24.0),
            spread_radius: px(0.0),
        };
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
            .border_color(accent)
            .rounded_md()
            .shadow(vec![glow_inner, glow_outer])
            .text_size(px(13.0))
            .text_color(theme::ink(&self.theme))
            .child(div().w(px(8.0)).h(px(8.0)).rounded_full().bg(accent))
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
    Claude(codescope_core::ClaudeTranscriptTail),
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

    fn snapshot(&self) -> Option<codescope_core::TelemetrySnapshot> {
        match self {
            AgentTail::Claude(t) => t.snapshot.clone(),
            AgentTail::Copilot(t) => t.snapshot.clone(),
            AgentTail::OpenCode(t) => t.snapshot.clone(),
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
    /// shortcuts (Ctrl+Shift+T, Ctrl+Shift+\, Ctrl+Shift+W,
    /// Ctrl+Shift+1..9) target this group; click on any pane / tab
    /// strip section moves the focus.
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
    /// In-flight tab drag-hover state. `Some` between an `on_drag_move`
    /// over any strip and the matching drop / drag-cancel. Drives the
    /// 3 px blue drop-indicator that previews "drop here" and the
    /// target index passed to `move_tab_to_group` on drop.
    ///
    /// Cleared explicitly by:
    /// - `on_drop` (drag completed by releasing over a strip);
    /// - the top of `render` when `cx.has_active_drag()` is `false`
    ///   (drag cancelled / released outside any strip). gpui 0.2.x
    ///   has no `on_drag_end` hook on the drop target, so the
    ///   per-frame poll is the safety net that keeps a stale
    ///   indicator from lingering after a drop on empty space.
    tab_drag_hover: Option<TabDropHover>,
    /// Per-group cache of the tab bounds captured during the most
    /// recent render. Key = group id, value = list of
    /// `(tab_id, window-space-bounds)` in render order. Populated by
    /// per-tab `canvas` prepaint hooks (mirroring the `bell_bounds`
    /// pattern). Reads (`on_drag_move`, drop-indicator) go through
    /// [`AppShell::resolved_tab_rects`] which prefers `tab_rects` and
    /// falls back to `prev_tab_rects` for the gap between
    /// `render`'s clear and the same frame's prepaint pass.
    tab_rects: HashMap<u64, Vec<(u64, gpui::Bounds<gpui::Pixels>)>>,
    /// Previous frame's `tab_rects`. Swapped in at the top of each
    /// `render` so the drop-indicator math (which runs at render-time,
    /// before the canvas prepaint callbacks fire) still has stable
    /// bounds to read. Without this, the indicator clamps to `x=0`
    /// for one frame after layout changes.
    prev_tab_rects: HashMap<u64, Vec<(u64, gpui::Bounds<gpui::Pixels>)>>,
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
    /// Set to the press origin only while a *maximized* window has an
    /// armed-but-not-yet-started title-bar drag; `None` otherwise.
    /// A windowed press starts its drag immediately and never sets this
    /// (see [`AppShell::handle_titlebar_press`]); a maximized press arms
    /// it so [`AppShell::update_titlebar_drag`] can start the
    /// restore-and-drag once the cursor moves into the content area, and
    /// `on_mouse_up` clears it. The stored point isn't read back — it's
    /// just an "armed" marker — but is kept as an origin in case a future
    /// change wants a movement threshold. Double-click is handled
    /// separately — see [`AppShell::last_titlebar_down`].
    titlebar_press: Option<gpui::Point<gpui::Pixels>>,
    /// `(time, position)` of the previous title-bar left-press, for our
    /// own double-click detection. We can't rely solely on gpui's
    /// `MouseDownEvent::click_count` for the *windowed* case: starting
    /// the drag on the press posts a synthetic `WM_NCLBUTTONDOWN` whose
    /// `LPARAM(0)` makes gpui's `ClickState` record a bogus position,
    /// which resets the count so the real second click reads as 1 — and
    /// double-click-to-maximize silently breaks. We therefore also fire
    /// the maximize toggle when two presses land close in time *and*
    /// space ourselves. (Maximized double-click never starts a drag, so
    /// `click_count` stays correct there; this is the windowed fix.)
    last_titlebar_down: Option<(std::time::Instant, gpui::Point<gpui::Pixels>)>,
    /// Global blink phase for dialog input fields (rename, new-project,
    /// new-worktree, settings, command palette). `true` paints the
    /// caret bar; `false` hides it. Flipped on a 530 ms cadence by the
    /// task spawned from `AppShell::new`. Any keystroke that mutates
    /// an input also resets this to `true` via
    /// [`AppShell::wake_text_blink`] so the caret is visible the
    /// instant the user types — mirrors the C# WPF
    /// `TextBoxBase.CaretBlinkTime` reset on edit.
    pub(crate) text_blink_phase: bool,
    /// Persistent notification ring buffer + popover visibility state.
    /// Mirrors `INotificationService` / `NotificationService` from the
    /// C# build.  The bell button (landing in the integrating PR) calls
    /// `notifications.toggle()` and the render calls
    /// `render_notifications_popover` alongside `render_toasts`.
    pub(crate) notifications: crate::notifications::Notifications,
    /// Per-session previous activity state, keyed by adopted-agent
    /// session id. Drives the bell-notification fire on transition.
    /// Mirrors C# `MainViewModel._lastActivity`. Entries are reaped
    /// after each telemetry tick to match `telemetry_tails`, so a
    /// rotated/closed session doesn't leak.
    last_session_state: HashMap<String, codescope_core::SessionState>,
    /// Cross-platform OS-toast notifier for the "turn complete"
    /// signal. Port of C# `WindowsIdleToastNotifier`. Owns its own
    /// 2 s per-session de-dupe so the FS-watcher and poll-fallback
    /// re-fires don't stack two toasts. Actual `Notification::show()`
    /// runs on the background executor in [`Self::apply_activity_notifications`]
    /// so the gpui main loop stays snappy.
    idle_notifier: crate::idle_notifier::IdleNotifier,
    /// `Window::is_window_active()` value captured at the most recent
    /// render. The telemetry poll task can't query gpui's `Window`
    /// directly (it runs without a `&Window` borrow), so we cache the
    /// flag here every frame and read it from
    /// [`Self::apply_activity_notifications`] as the gate for OS
    /// toasts. Up to one frame stale; on a 250 ms telemetry cadence
    /// that's well below the threshold the user would notice.
    /// Defaults to `true` so a transition fired during start-up (rare)
    /// is treated as "user can see the app".
    window_active_cached: bool,
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
    /// In-memory snapshot of the on-disk session catalog used by
    /// the session-lifecycle helpers. Initialised at construction
    /// from a [`SessionManager::load_with_sweep`] pass so the
    /// retention sweep runs once per launch (mirrors C#
    /// `SessionStore.LoadAsync`). Each session mutation (open /
    /// soft-close) re-reads from disk first so concurrent sidebar
    /// writes (project / worktree mutations also persist to
    /// `projects.json`) are not clobbered, then applies the change
    /// via [`SessionManager`] and writes back via
    /// [`ProjectsConfig::save`]. The Sidebar still owns its own
    /// `ProjectsConfig` clone for project / worktree rendering —
    /// they converge through the shared file. This split mirrors
    /// the C# build where `SessionStore` is the orchestrator and
    /// `SidebarViewModel.StoreSync` projects from it.
    pub(crate) projects: ProjectsConfig,
    /// Registry of agent profiles built from `settings.agents`
    /// overrides (or the shipped built-in defaults when none are
    /// configured). Mirrors C# `AgentRegistry` — owned at the shell
    /// level so the new-session menu can list agents, pick the
    /// user's preferred default, and look up by id on session restore.
    agent_registry: codescope_core::AgentRegistry,
    /// Open command palette state, if any. `Some` between
    /// Ctrl+Shift+P press and Enter / Esc. Holds the action list,
    /// search query, and selection cursor — see
    /// [`crate::command_palette::CommandPaletteState`].
    command_palette: Option<crate::command_palette::CommandPaletteState>,
    /// Overview-panel visibility. While `true`, the work area
    /// (group strip + terminal grid) is hidden and replaced by the
    /// full-pane [`crate::overview::AppShell::render_overview`]; the
    /// sidebar and status bar stay anchored. Toggled by the sidebar
    /// footer "Overview" button (PR #126), the `Ctrl+Shift+O`
    /// shortcut, and the in-panel "← Back to workspace" link.
    /// Mirrors C# `MainViewModel.IsOverviewVisible`.
    show_overview: bool,
    /// Diff viewer panel state; `Some` = visible, replacing the work
    /// area exactly like `show_overview` does (the two are mutually
    /// exclusive — opening one closes the other). See
    /// [`crate::diff_viewer`].
    pub(crate) diff_viewer: Option<crate::diff_viewer::DiffViewerState>,
    /// Monotonic sequence for diff-viewer background requests, so a
    /// stale `git diff` result can never clobber a newer one.
    pub(crate) diff_request_seq: u64,
    /// Open Settings dialog, if any. Surfaces `settings.json` fields
    /// via a centered modal — the Rust port's replacement for the C#
    /// build's hand-edit-the-file workflow. See ADR-0018. Visible
    /// fields mirror exactly what's in [`codescope_core::Settings`];
    /// no schema additions.
    pub(crate) settings_dialog: Option<crate::dialogs::settings::SettingsDialogState>,
    /// Open Rename dialog, if any. Surfaces a single text-input modal
    /// for renaming a project or a session (live or closed). Mirrors
    /// the C# build's `Dialogs.RenameDialog.Prompt` — the Rust port
    /// owns it on AppShell instead of opening a modal `Window` because
    /// gpui doesn't have a modal-window primitive. See
    /// `src/dialogs/rename.rs` for the full rationale.
    pub(crate) rename_dialog: Option<crate::dialogs::rename::RenameDialogState>,
    /// Open Confirm dialog, if any. Themed in-app replacement for the
    /// OS-native `window.prompt(...)` used by destructive sidebar
    /// actions (remove project, discard worktree changes, remove
    /// worktree, force-retry, remove from history). Mirrors C#
    /// `Dialogs.ConfirmDialog.Confirm` / `Destructive`. See
    /// `src/dialogs/confirm.rs`.
    pub(crate) confirm_dialog: Option<crate::dialogs::confirm::ConfirmDialogState>,
    /// Multiplatform taskbar / dock badge driver. Mirrors C#
    /// `TaskbarBadgeService`. Refreshed from the same telemetry-poll
    /// callback that updates the sidebar dots
    /// ([`AppShell::push_sidebar_session_paths`]) so the badge tracks
    /// agent-rollup state with no extra polling cost. Cleared when
    /// the shell drops.
    taskbar_badge: crate::taskbar_badge::TaskbarBadge,
    /// Set to `true` while [`AppShell::rehydrate_or_cold_start`] is
    /// walking live sessions and calling [`AppShell::spawn_tab_in`] for
    /// each one. All `save_layout` calls (direct and indirect, via
    /// `activate_tab`'s focused-group hook) short-circuit while this is
    /// set; the rehydrate driver does a single `save_layout` at the
    /// end. Without this, N live sessions produced N read-modify-write
    /// cycles on `layout.json` per cold start AND each intermediate
    /// write persisted a *partial* `session_placements` snapshot — a
    /// crash mid-rehydrate would leave already-spawned placements but
    /// drop the rest (they would still come back via `projects.json`,
    /// but in group 0 instead of their saved group).
    suppress_layout_save: bool,
    /// Updater state slot — written by background threads in
    /// `crate::update::start_poll` / `start_install`, read every
    /// render to decide whether to surface the update toast.
    /// Initialized and handed to `start_poll` in `AppShell::new`.
    update_state: crate::update::UpdateState,
    /// Tag (or sentinel string) of the most-recently-surfaced
    /// "update available" / "ready" / "failed" toast. Keeps the
    /// per-frame `surface_update_state` from spam-pushing the same
    /// toast over and over while the persistent bell entry is already
    /// in place.
    last_announced_update: Option<String>,
    /// Id of the live progress toast shown during Downloading /
    /// Installing. `Some` while an install is in flight; the toast's
    /// title/detail are rewritten in place each frame from the
    /// UpdateStatus snapshot, then it's removed on Ready / Failed.
    update_progress_toast_id: Option<u64>,
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
        // Build the agent registry up-front so the sidebar can consume
        // it on construction — its worktree-menu "New {DisplayName}
        // session" rows iterate this list. Mirrors C#
        // `AgentRegistry.FromConfig`, honouring `settings.agents`
        // overrides + the `default_agent` flag.
        let agent_registry = codescope_core::AgentRegistry::from_settings(&settings);
        // One focus handle for the sidebar's filter input — owned at
        // the shell level so it survives sidebar entity recreation.
        let sidebar_filter_focus = cx.focus_handle();
        let sidebar = {
            let agent_registry_for_sidebar = agent_registry.clone();
            let filter_focus = sidebar_filter_focus.clone();
            let layout_for_sidebar = layout.clone();
            let theme_for_sidebar = theme.clone();
            let paths_for_sidebar = paths.clone();
            cx.new(move |cx| {
                let sidebar = Sidebar::new(
                    projects,
                    layout_for_sidebar,
                    theme_for_sidebar,
                    paths_for_sidebar,
                    agent_registry_for_sidebar,
                    filter_focus,
                );
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
                // Dialog-input caret blink (Add project, New worktree).
                // Independent timer; same 530 ms cadence as AppShell's
                // so paired inputs across the two entities still feel
                // synced.
                sidebar.start_text_blink(cx);
                sidebar
            })
        };

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
                    agent_id,
                    force_new,
                    force_shell,
                } => {
                    // Three-tier "open session" resolution, mirroring
                    // the C# `MainViewModel.OnTreeDoubleClick` flow:
                    //
                    //   1. An open tab pinned to this `wd` already
                    //      exists → just focus it.
                    //   2. No open tab, but the worktree has a
                    //      soft-closed session in history → reopen
                    //      the most-recently-closed one. This is the
                    //      user expectation for "double-click the
                    //      same branch again" — pick up where you
                    //      left off, don't keep stamping fresh
                    //      free-floating sessions.
                    //   3. Otherwise → spawn a brand new session
                    //      (the same path `force_new` takes
                    //      unconditionally for explicit "New
                    //      session" / "New <agent> session" menu
                    //      rows).
                    if !*force_new {
                        // Tier 1: focus an already-open tab.
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

                        // Tier 2: reopen the most-recent closed
                        // session for this worktree. Path matching
                        // goes through `paths_match` so a `\\`-vs-`/`
                        // mismatch between the spawn event and the
                        // persisted `worktree_path` doesn't drop the
                        // history hit (same reason
                        // `locate_project_for_path` uses it). Sort
                        // by `closed_at` desc — newest first —
                        // mirroring the sidebar's history disclosure
                        // ordering so a double-click and the visible
                        // top-of-history row stay in lockstep. ISO-
                        // 8601 sorts lexicographically the same way
                        // it sorts chronologically, so a plain str
                        // cmp is enough.
                        let wd_str = working_directory.to_string_lossy();
                        let most_recent_closed = this
                            .projects
                            .projects
                            .iter()
                            .flat_map(|p| p.sessions.iter())
                            .filter(|s| s.closed_at.is_some())
                            .filter(|s| {
                                codescope_core::path_canon::paths_match(
                                    &s.worktree_path,
                                    &wd_str,
                                )
                            })
                            .max_by(|a, b| {
                                a.closed_at.as_deref().cmp(&b.closed_at.as_deref())
                            })
                            .map(|s| s.id.clone());
                        if let Some(session_id) = most_recent_closed {
                            this.reopen_session(session_id, window, cx);
                            return;
                        }
                    }

                    // Tier 3: spawn a fresh session. Reached when
                    // `force_new` was set explicitly OR when there
                    // is neither an open tab nor a closed session
                    // for this worktree. The `agent_id` from the
                    // event is denormalised alongside `auto_type`
                    // (same `AgentProfile`) so the persisted Session
                    // row can record which agent backed this tab —
                    // without that stamp `reopen_session` would
                    // later see `agent_id: None` and fall back to a
                    // plain shell.
                    //
                    // "Open session" / generic worktree click emit
                    // both fields as `None` (the emitter doesn't
                    // know yet whether the spawn will actually
                    // happen — focus-or-open might short-circuit it
                    // away). For those we resolve the default agent
                    // *here*, at the moment we know a fresh spawn
                    // is happening, so the new row still gets a
                    // useful `agent_id`.
                    let (effective_agent_id, effective_auto_type) =
                        match (agent_id.clone(), auto_type.clone()) {
                            (Some(id), at) => (Some(id), at),
                            (None, Some(at)) => (None, Some(at)),
                            // Explicit shell intent: emitter wants a
                            // bare terminal regardless of the user's
                            // default agent. Skip the fallback so the
                            // sidebar's "Shell" row keeps its promise.
                            (None, None) if *force_shell => (None, None),
                            (None, None) => match default_agent_launch_for(&this.settings) {
                                Some((id, at)) => (Some(id), at),
                                None => (None, None),
                            },
                        };
                    this.spawn_tab_in(
                        Some(working_directory.clone()),
                        Some(title.clone()),
                        effective_auto_type,
                        effective_agent_id,
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
                SidebarEvent::OpenDiff { worktree_path } => {
                    this.open_diff_viewer(Some(worktree_path.clone()), cx);
                }
                SidebarEvent::OpenRenameDialog { target, current_name } => {
                    // Reload `projects.json` before opening so the
                    // dialog operates on the freshest snapshot — same
                    // reload-then-mutate discipline `allocate_session_id`
                    // / `soft_close_session` use. A sidebar add/remove
                    // between two AppShell mutations would otherwise
                    // make the rename target invisible at submit time.
                    //
                    // Bail on load failure rather than mutating a stale
                    // snapshot and persisting it on top of newer disk
                    // state (same rule `reopen_session` uses). Surface
                    // the error so the user sees the row stay
                    // un-renamed instead of silently racing the file.
                    match ProjectsConfig::load(&this.paths) {
                        Ok(cfg) => {
                            this.projects = cfg;
                        }
                        Err(err) => {
                            eprintln!(
                                "warning: failed to reload projects.json before rename: {err:#}"
                            );
                            this.push_toast(
                                ToastKind::Err,
                                SharedString::from("Rename failed"),
                                Some(SharedString::from(format!(
                                    "Could not read projects.json: {err:#}"
                                ))),
                                cx,
                            );
                            return;
                        }
                    }
                    this.open_rename_dialog(
                        target.clone(),
                        current_name.clone(),
                        window,
                        cx,
                    );
                }
                SidebarEvent::OpenConfirmDialog { spec, action } => {
                    this.handle_open_confirm_dialog(
                        spec.clone(),
                        action.clone(),
                        window,
                        cx,
                    );
                }
                SidebarEvent::OpenSettings => {
                    // Opened from the sidebar project context menu's
                    // "Settings…" row. Same entry point Ctrl+Shift+,
                    // and the status-bar gear left-click use —
                    // `open_settings_dialog` is idempotent on an
                    // already-open dialog.
                    this.open_settings_dialog(window, cx);
                }
                SidebarEvent::WorktreeBranchChanged { path, branch } => {
                    // `git checkout other` inside a worktree's pty
                    // flipped the branch; the git-status poller saw
                    // the new value on this tick. Rewrite the title
                    // of every tab pinned to this path so the strip
                    // follows. Mirrors C#
                    // `MainViewModel.RefreshTabTitlesForWorktree`.
                    this.rename_tabs_for_path(path, branch, cx);
                }
            }
        })
        .detach();
        let pending_window_save: Arc<Mutex<Option<PendingWindowSave>>> = Arc::new(Mutex::new(None));

        // Persist live window geometry, and force a re-render so the
        // terminal panes pick up the new viewport bounds (see the
        // closure body for the gpui race that makes the second job
        // non-trivial). The observer fires for every resize / move
        // tick; we just stash the latest state and let the background
        // debounce task hit disk once the dust settles.
        cx.observe_window_bounds(window, {
            let pending = pending_window_save.clone();
            let diag_paths = paths.clone();
            move |_, window, cx| {
                append_window_diag(&diag_paths, "bounds_changed", window);
                let state = window_state_from_window(window);
                *pending.lock() = Some(PendingWindowSave {
                    state,
                    set_at: Instant::now(),
                });
                // Force a re-render so the entity tree picks up the
                // new viewport bounds (otherwise the terminal panes
                // keep their pre-transition layout until something
                // else marks the tree dirty — e.g. tab swap, output,
                // focus change).
                //
                // `cx.notify()` alone is not enough. The observer
                // can fire mid-frame (gpui calls it from inside
                // `Window::bounds_changed`, which is dispatched off
                // an OS resize tick that can land while `draw_phase
                // != None`). When that happens, `WindowInvalidator::
                // invalidate_view` (gpui-0.2.2 `window.rs:116`) adds
                // the entity to `dirty_views` but skips the
                // `dirty = true` set — the window's dirty flag never
                // flips, so no next frame is scheduled. Same race
                // gpui's own `Window::refresh()` has, which is why
                // it didn't backstop us either.
                //
                // `window.on_next_frame` queues a callback that runs
                // at the start of the next `on_request_frame` tick,
                // where `draw_phase == None` is guaranteed. The
                // `app.notify` there reliably flips `dirty = true`
                // and the resize cascades through `render_group` →
                // `TerminalView` canvas prepaint → `maybe_resize` →
                // `Backend::resize`. `cx.notify()` stays as the
                // fast path for the (common) outside-of-draw case.
                cx.notify();
                let entity = cx.entity_id();
                window.on_next_frame(move |_, app| app.notify(entity));
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
                    this.toasts.retain(|t| t.expires_at.map_or(true, |e| e > now));
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
        // Renormalise on load too. layout.json round-trips the raw
        // post-drag weights (a single drag against a peer can leave a
        // survivor at 0.5), and taffy treats `flex_grow = 0.5` as
        // "claim half of free space" even on a lone item — the rest of
        // the work area stays blank. close/split/move already call
        // this on mutation; the load path was the missing site, so a
        // fresh boot showed the half-width regression even before the
        // user touched anything.
        normalise_group_weights(&mut sanitized_weights);
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

        // `agent_registry` was built earlier (before the sidebar) so
        // the sidebar entity could consume a clone for its worktree-
        // menu rows. We reuse the same registry here for the shell.

        // Bind the updater state slot up-front so we can hand a
        // clone to the background poll thread while keeping the
        // canonical Arc on the AppShell. `start_poll` has a built-in
        // ~15s startup delay so it stays off the first-frame
        // critical path.
        let update_state = crate::update::new_state();
        crate::update::start_poll(update_state.clone());

        let mut shell = Self {
            groups,
            focused_group,
            group_weights: sanitized_weights,
            splitter_drag: None,
            sidebar_width,
            sidebar_visible,
            sidebar_drag: None,
            tab_menu: None,
            tab_drag_hover: None,
            tab_rects: HashMap::new(),
            prev_tab_rects: HashMap::new(),
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
            titlebar_press: None,
            last_titlebar_down: None,
            text_blink_phase: true,
            notifications: crate::notifications::Notifications::new(),
            last_session_state: HashMap::new(),
            idle_notifier: crate::idle_notifier::IdleNotifier::new(),
            window_active_cached: true,
            telemetry_tails: HashMap::new(),
            bell_bounds: None,
            projects: projects_for_sessions,
            agent_registry,
            command_palette: None,
            show_overview: false,
            diff_viewer: None,
            diff_request_seq: 0,
            settings_dialog: None,
            rename_dialog: None,
            confirm_dialog: None,
            taskbar_badge: crate::taskbar_badge::TaskbarBadge::new(window),
            suppress_layout_save: false,
            update_state,
            last_announced_update: None,
            update_progress_toast_id: None,
        };
        shell.start_telemetry_poll(cx);
        shell.start_agent_discovery_poll(cx);
        shell.schedule_taskbar_badge_init(cx);
        shell.start_text_blink(cx);
        shell.rehydrate_or_cold_start(window, cx);
        shell
    }

    /// Schedule the deferred Windows COM bring-up for the taskbar
    /// badge.
    ///
    /// `TaskbarBadge::new` only captures the HWND — the
    /// `CoCreateInstance(TaskbarList)` call is deferred to a
    /// `cx.spawn` foreground task because that call synchronously
    /// pumps the Win32 message loop while marshalling the shell
    /// proxy, and pumping while the AppShell entity's `RefCell` was
    /// still borrowed (i.e. inside `cx.new(|cx| AppShell::new(...))`)
    /// re-entered the gpui window proc and tripped
    /// `RefCell already borrowed` (session 38 PR #149 startup crash).
    ///
    /// The body crucially does **not** call `this.update(...)` — the
    /// COM init writes through a cloned `Rc<RefCell<...>>` slot so
    /// no entity borrow is live while the message pump runs.
    fn schedule_taskbar_badge_init(&self, cx: &mut Context<Self>) {
        let init = self.taskbar_badge.init_handle();
        cx.spawn(async move |this, cx| {
            // A tiny timer is enough to bounce the work out of the
            // current effect cycle so the AppShell entity's
            // construction borrow has dropped before we run. We do
            // not call `this.upgrade()` strictly — the entity could
            // already be torn down by then on a fast quit — but a
            // dropped slot is fine: `init.run()` just writes through
            // the shared Rc, which the WindowsBadge may or may not
            // still be observing.
            cx.background_executor()
                .timer(Duration::from_millis(50))
                .await;
            let _ = this;
            init.run();
        })
        .detach();
    }
}

impl Drop for AppShell {
    /// Best-effort clear of the OS taskbar / dock overlay on
    /// graceful teardown. Only runs on normal unwind — a hard
    /// abort / OS-level kill skips destructors, but in that case
    /// Windows itself releases the overlay when the HWND is
    /// destroyed. Mirrors WPF's automatic `TaskbarItemInfo`
    /// cleanup when the `MainWindow` is closed.
    fn drop(&mut self) {
        self.taskbar_badge.clear();
    }
}

impl AppShell {
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
        // Short-circuit while rehydrate is walking live sessions.
        // Otherwise the N spawn_tab_in calls become N read-modify-write
        // cycles on layout.json *and* each intermediate write persists
        // a partial session_placements list (only the tabs spawned so
        // far). The rehydrate driver flushes once at the end via
        // `flush_layout_after_rehydrate`. Outside rehydrate this is
        // always false so user-driven mutations save normally.
        if self.suppress_layout_save {
            return;
        }
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
        on_disk.session_placements = self.snapshot_session_placements();
        // Drop the legacy `open_tabs` array — `session_placements` is
        // authoritative now, and `projects.json` carries the open-vs-
        // closed answer. Belt-and-braces: `LayoutState::load` already
        // clears `open_tabs` after migration, but a save kicked off
        // before the next load (e.g. an upgrade installer that swaps
        // binaries mid-session) would otherwise re-emit it.
        on_disk.open_tabs.clear();
        if let Err(err) = on_disk.save(self.paths.as_ref()) {
            eprintln!("warning: failed to save layout.json: {err:#}");
            return;
        }
        self.layout = on_disk;
    }

    // -----------------------------------------------------------------------
    // Claude telemetry
    // -----------------------------------------------------------------------

    /// Stamp `Session.agent_session_id` for the CodeScope session
    /// identified by `session_id` (the stable id from `projects.json`,
    /// **not** the agent-minted UUID) and persist `projects.json` so
    /// `build_resume_auto_type` (PR #178) can resume the session by id
    /// after a restart.
    ///
    /// Called from the agent-discovery loop the moment any of the four
    /// agents (Claude, Copilot, OpenCode, Pi) mints a session id we
    /// can recognise. Mirrors C# `MainViewModel.ApplyAdoption` →
    /// `SessionStore.UpdateAgentSessionIdAsync` round-trip.
    ///
    /// Lenient on missing sessions: a freshly-spawned tab's first
    /// discovery tick can race [`SessionManager::open`]'s write to
    /// `projects.json`. We swallow that — the next tick (~350 ms
    /// later) will land after the row exists and the stamp will
    /// succeed.
    ///
    /// Reload-before-mutate: the sidebar persists `projects.json`
    /// independently (add / remove project, soft-close session, …), so
    /// stamping a stale in-memory `self.projects` would clobber any
    /// changes the sidebar wrote between two AppShell mutations.
    /// Mirrors the discipline used in `allocate_session_id`,
    /// `soft_close_session`, `reopen_session`, and `open_rename_dialog`.
    /// On reload failure we leave `self.projects` untouched and bail
    /// out without writing — same rule the other persist sites use, so
    /// a transient I/O hiccup doesn't end with a stale snapshot landing
    /// on top of newer disk state. Persist failures themselves are
    /// logged and otherwise ignored to match the rest of the persist
    /// callsites in this file (`spawn_tab_in`, `close_tab`, etc).
    fn persist_agent_session_id(&mut self, session_id: &str, agent_session_id: &str) {
        match ProjectsConfig::load(&self.paths) {
            Ok(cfg) => {
                self.projects = cfg;
            }
            Err(err) => {
                eprintln!(
                    "warning: failed to reload projects.json before persist of agent_session_id for {session_id}: {err:#}"
                );
                return;
            }
        }
        let changed = SessionManager::set_agent_session_id_lenient(
            &mut self.projects,
            session_id,
            agent_session_id,
        );
        if !changed {
            return;
        }
        if let Err(err) = self.projects.save(&self.paths) {
            eprintln!(
                "warning: failed to persist agent_session_id for {session_id}: {err:#}"
            );
        }
    }

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
                AgentTail::Claude(codescope_core::ClaudeTranscriptTail::for_session(
                    &root,
                    working_directory,
                    &session_id,
                ))
            }
            codescope_core::AgentId::Copilot => {
                let Some(root) = codescope_core::agents::copilot::telemetry::default_session_state_root()
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
                let Some(root) = codescope_core::agents::opencode::telemetry::default_data_root() else {
                    eprintln!(
                        "[telemetry] no USERPROFILE / HOME — skipping opencode registration for {session_id}"
                    );
                    return;
                };
                AgentTail::OpenCode(codescope_core::OpenCodeMessageTail::for_session(
                    &root,
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
                let Some(root) = codescope_core::agents::pi::telemetry::default_sessions_root() else {
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
    /// Cadence mirrors the C# [`ClaudeTelemetryService`]:
    ///
    /// - **250 ms** while *any* tail is registered, regardless of busy
    ///   state. The C# build uses a flat 250 ms `Timer` armed only
    ///   when watches exist (see `RefreshTimerArmed`); we match that
    ///   so an idle → busy transition surfaces inside the next tick
    ///   instead of waiting up to the previous 2 s idle cadence
    ///   (the original "twee seconds before the wait pulse appears"
    ///   bug — issue: agent thinking-state latency).
    /// - **30 s** when no tails are registered. The C# build fully
    ///   disarms the timer here; we keep the task alive (avoids a
    ///   wakeup/teardown dance on every tab open/close) but throttle
    ///   it down to a heartbeat.
    ///
    /// Polling cost: each tick is a `FileInfo::Length` stat per
    /// tail; reads only happen when the offset has moved. On Windows
    /// this is sub-millisecond per file, so 4 tails × 4 Hz is
    /// negligible. The C# build runs the same rate across four
    /// agent services (~16 Hz combined) in production.
    ///
    /// Called from `AppShell::new` after the struct is constructed;
    /// the first tick fires after construction is done (avoids the
    /// borrow-at-construction race that `start_dirty_poll` also
    /// guards against).
    /// `true` when any AppShell-owned text input is currently on
    /// screen (rename dialog, settings dialog, or command palette).
    /// The blink timer uses this to gate `cx.notify()` so an idle
    /// app — no dialog up — doesn't redraw twice a second just to
    /// re-paint nothing.
    fn any_text_input_visible(&self) -> bool {
        self.rename_dialog.is_some()
            || self.settings_dialog.is_some()
            || self.command_palette.is_some()
    }

    /// Drive the global dialog-input caret blink. Flips
    /// `text_blink_phase` every [`TEXT_BLINK_PERIOD`], but only emits
    /// a `cx.notify()` while at least one text-input surface
    /// ([`any_text_input_visible`]) is on screen — an idle app
    /// otherwise pays a repaint twice a second for nothing. The
    /// phase itself keeps ticking either way so opening a dialog
    /// finds the caret already in its conventional rhythm. Matches
    /// the WPF TextBox default and the terminal view's own blink
    /// cadence so adjacent inputs flip in lockstep.
    fn start_text_blink(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(TEXT_BLINK_PERIOD).await;
                if this.upgrade().is_none() {
                    break;
                }
                let _ = this.update(cx, |this, cx| {
                    this.text_blink_phase = !this.text_blink_phase;
                    if this.any_text_input_visible() {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
    }

    /// Reset the caret to "visible" right now. Called by every dialog
    /// key handler on any keystroke that mutates the buffer so the
    /// caret doesn't disappear in the middle of a fast type burst —
    /// mirrors WPF `TextBoxBase` which resets the blink timer on
    /// every edit.
    pub(crate) fn wake_text_blink(&mut self, cx: &mut Context<Self>) {
        if !self.text_blink_phase {
            self.text_blink_phase = true;
            cx.notify();
        }
    }

    fn start_telemetry_poll(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            // Start at the active cadence — the first registration
            // typically races construction, so we don't want to
            // sleep 30 s on cold-start before the first tick.
            let mut interval = TELEMETRY_POLL_ACTIVE;
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
                        // Same logic for the taskbar overlay: a tab
                        // that just closed should clear the badge.
                        this.refresh_taskbar_badge();
                        return TELEMETRY_POLL_IDLE;
                    }
                    for tail in this.telemetry_tails.values_mut() {
                        tail.poll();
                    }
                    // After every poll, recompute the per-path
                    // active/busy snapshot the sidebar uses to colour
                    // its worktree dots and propagate "agent busy" up
                    // to a collapsed project row. Cheap — one map
                    // lookup per tab; the sidebar `set_session_paths`
                    // call short-circuits with no notify when nothing
                    // changed, so a 250 ms cadence doesn't drive
                    // a redraw every tick unless a tab actually
                    // flipped state.
                    this.push_sidebar_session_paths(cx);
                    // Taskbar / dock overlay refresh runs on the
                    // same cadence — the badge driver itself
                    // de-dupes redundant `apply` calls so a quiet
                    // busy stretch doesn't repaint every 250 ms.
                    this.refresh_taskbar_badge();
                    // Fire bell-notification entries on activity
                    // transitions. Mirrors C#
                    // `MainViewModel.ApplyTelemetry` calling
                    // `PushActivityNotification` per snapshot.
                    this.apply_activity_notifications(cx);
                    TELEMETRY_POLL_ACTIVE
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
                        /// Agent-minted UUID we'd previously adopted
                        /// for this tab, if any — used to unregister
                        /// the old telemetry tail on rotation
                        /// (Claude `/clear`, Pi re-invocation).
                        previous_agent_session_id: Option<String>,
                        /// Agent-minted UUID this tick is adopting.
                        new_agent_session_id: String,
                        working_directory: String,
                        /// CodeScope session id (from `projects.json`),
                        /// the stable handle we stamp
                        /// `agent_session_id` against. Captured here
                        /// so the second pass can persist the
                        /// agent-minted id without re-borrowing the
                        /// tab. Distinct from
                        /// `new_agent_session_id` above — that's the
                        /// UUID the agent CLI minted, this is the
                        /// CodeScope-allocated id from
                        /// `projects.json::Session.id`.
                        codescope_session_id: String,
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
                                    codescope_core::agents::claude::discovery::scan(
                                        &root, &wd_str, tab.spawned_at,
                                    )
                                    .into_iter()
                                    .map(|c| (c.session_id, c.path))
                                    .collect()
                                }
                                codescope_core::AgentId::Pi => {
                                    let Some(root) =
                                        codescope_core::agents::pi::telemetry::default_sessions_root()
                                    else {
                                        continue;
                                    };
                                    codescope_core::agents::pi::discovery::scan(
                                        &root, &wd_str, tab.spawned_at,
                                    )
                                    .into_iter()
                                    .map(|c| (c.session_id, c.path))
                                    .collect()
                                }
                                codescope_core::AgentId::OpenCode => {
                                    let Some(root) =
                                        codescope_core::agents::opencode::telemetry::default_data_root()
                                    else {
                                        continue;
                                    };
                                    codescope_core::agents::opencode::discovery::scan(
                                        &root, &wd_str, tab.spawned_at,
                                    )
                                    .into_iter()
                                    .map(|c| (c.session_id, c.message_path))
                                    .collect()
                                }
                                codescope_core::AgentId::Copilot => {
                                    let Some(root) =
                                        codescope_core::agents::copilot::telemetry::default_session_state_root()
                                    else {
                                        continue;
                                    };
                                    codescope_core::agents::copilot::discovery::scan(
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
                                    previous_agent_session_id: tab.adopted_session_id.clone(),
                                    new_agent_session_id: sid,
                                    working_directory: wd_str,
                                    codescope_session_id: tab.session_id.clone(),
                                });
                            }
                        }
                    }
                    for f in found {
                        if let Some(prev) = f.previous_agent_session_id.as_deref()
                            && prev != f.new_agent_session_id {
                                this.unregister_telemetry(prev);
                            }
                        this.register_telemetry(
                            f.agent_id,
                            f.new_agent_session_id.clone(),
                            &f.working_directory,
                        );
                        // Persist the agent-minted id back to
                        // `projects.json` so `build_resume_auto_type`
                        // (PR #178) can resume the session by id after
                        // a restart. Lenient on missing sessions —
                        // see `persist_agent_session_id` for the
                        // cold-start race rationale.
                        this.persist_agent_session_id(
                            &f.codescope_session_id,
                            &f.new_agent_session_id,
                        );
                        if let Some(group) = this.groups.get_mut(f.group_idx)
                            && let Some(tab) = group.tabs.get_mut(f.tab_idx) {
                                tab.adopted_session_id =
                                    Some(f.new_agent_session_id.clone());
                                tab.fired_session_ids.insert(f.new_agent_session_id);
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

    // -----------------------------------------------------------------------
    // Layout persistence
    // -----------------------------------------------------------------------

    /// Capture the on-disk placement of every currently-open tab whose
    /// session id resolves to a row in `projects.json`. Mirrors C#
    /// `MainViewModel.CaptureLayout` (`LayoutStore.Layout.SessionToGroup`):
    /// the authoritative "is this session open?" answer lives in
    /// `projects.json`'s `closed_at` field; this snapshot only records
    /// the layout decisions (which group + which tab was active).
    ///
    /// Free-floating tabs (no project context at spawn time → no row
    /// in `projects.json`) are skipped: they don't survive a restart
    /// in the new model because the rehydrate path drives off live
    /// sessions from `projects.json`. Matches C# where every tab
    /// belongs to a project (the "unsorted" bucket is the catch-all).
    fn snapshot_session_placements(&self) -> Vec<codescope_core::SessionPlacement> {
        let mut out = Vec::new();
        for (g_idx, group) in self.groups.iter().enumerate() {
            for (t_idx, tab) in group.tabs.iter().enumerate() {
                if self.lookup_session_by_id(&tab.session_id).is_none() {
                    continue;
                }
                out.push(codescope_core::SessionPlacement {
                    session_id: tab.session_id.clone(),
                    group_index: g_idx,
                    active_in_group: t_idx == group.active_tab,
                });
            }
        }
        out
    }

    /// Rehydrate the tab strip from `projects.json` live sessions
    /// (rows where `closed_at = None`). Mirrors C#
    /// `MainViewModel.HydrateFromLoaded`: the authoritative source of
    /// "what should be open" is the project store, and `layout.json`
    /// only decides which group each session lands in (via
    /// [`SessionPlacement`]) plus widths / focus. This guarantees that
    /// a tab opened minutes before a crash — even with no intervening
    /// `save_layout` — comes back on next launch, because the
    /// `SessionManager::open` call that spawned the tab also persisted
    /// the row to `projects.json`.
    ///
    /// Sessions whose `worktree_path` no longer exists are skipped
    /// (matches C# `Directory.Exists` guard). When no live session
    /// survives, we leave the window empty so the user lands on the
    /// neutral cold-start state instead of an auto-spawned default
    /// shell pinned to an arbitrary project.
    fn rehydrate_or_cold_start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Snapshot the live sessions up front so the spawn loop can
        // mutate `self` (focused_group, groups) without colliding with
        // a long-lived borrow on `self.projects`. Project iteration
        // order is the on-disk order, matching C#'s
        // `foreach (var p in loaded.Projects) foreach (var s in p.Sessions)`.
        struct RehydrateEntry {
            session_id: String,
            worktree_path: String,
            agent_id: Option<String>,
            agent_session_id: Option<String>,
            project_name: String,
            branch: Option<String>,
        }
        let entries: Vec<RehydrateEntry> = self
            .projects
            .projects
            .iter()
            .flat_map(|p| {
                let project_name = p.name.clone();
                p.sessions.iter().filter_map(move |s| {
                    if s.closed_at.is_some() {
                        return None;
                    }
                    let branch = s.branch.clone().or_else(|| {
                        p.worktrees
                            .iter()
                            .find(|w| Some(&w.id) == s.worktree_id.as_ref())
                            .and_then(|w| w.branch.clone())
                    });
                    Some(RehydrateEntry {
                        session_id: s.id.clone(),
                        worktree_path: s.worktree_path.clone(),
                        agent_id: s.agent_id.clone(),
                        agent_session_id: s.agent_session_id.clone(),
                        project_name: project_name.clone(),
                        branch,
                    })
                })
            })
            .collect();

        if entries.is_empty() {
            // Cold launch — leave the window empty. Focus the AppShell
            // root so app-level chords (Ctrl+Shift+T to open a new tab,
            // Ctrl+Shift+P for the palette, etc.) fire from a fresh
            // boot — without explicit focus there's no focused element
            // at all and the key handler never triggers.
            self.focus_handle.focus(window);
            cx.notify();
            return;
        }

        // Build a placement lookup keyed by session id. Stale entries
        // (no matching live session) are skipped implicitly; missing
        // entries fall back to group 0, non-active.
        let placements: std::collections::HashMap<String, codescope_core::SessionPlacement> = self
            .layout
            .session_placements
            .iter()
            .map(|p| (p.session_id.clone(), p.clone()))
            .collect();

        let group_count = self.groups.len();
        let mut active_by_group: Vec<Option<usize>> = vec![None; group_count];
        let mut spawned_any = false;
        // `(session_id, resolved_agent_id)` pairs to backfill into
        // projects.json after the loop. Populated only when a
        // rehydrated row had `agent_id = None` and we rescued it via
        // `settings.default_agent` — see the loop body for context.
        // Empty in the steady state once all legacy rows are healed.
        let mut pending_backfills: Vec<(String, String)> = Vec::new();

        // Suppress per-spawn `save_layout` writes while the loop runs;
        // we flush a single complete snapshot at the end. Without this,
        // each `spawn_tab_in` would read+write `layout.json` (and a
        // crash mid-loop would persist a partial `session_placements`
        // list — sessions not yet spawned would still rehydrate via
        // `projects.json` on the next boot, just demoted to group 0).
        self.suppress_layout_save = true;

        for entry in entries {
            let path = std::path::PathBuf::from(&entry.worktree_path);
            if !path.exists() {
                eprintln!(
                    "info: skipping live session — worktree path no longer exists: {} ({})",
                    entry.worktree_path, entry.session_id
                );
                continue;
            }

            let placement = placements.get(&entry.session_id);
            let group_idx = placement
                .map(|p| p.group_index)
                .unwrap_or(0)
                .min(group_count.saturating_sub(1));
            let active_in_group = placement.map(|p| p.active_in_group).unwrap_or(false);

            // `spawn_tab_in` always lands in the focused group, so
            // we move focus first then restore the saved focus index
            // after the loop.
            self.focused_group = group_idx;

            // Legacy / pre-PR-#223 rows come back with `agent_id =
            // None` (the spawn side started stamping the id only
            // after that fix). Falling back to a plain shell there
            // is the "1 of my Claude tabs rehydrated as PowerShell"
            // bug reported on rc.11. Mirror `reopen_session`: rescue
            // via `settings.default_agent` so the tab resumes as the
            // configured agent. The backfill list below pushes the
            // resolved id back into projects.json so the next boot
            // skips the fallback entirely.
            let resolved_agent_id: Option<String> = entry
                .agent_id
                .clone()
                .or_else(|| default_agent_launch_for(&self.settings).map(|(id, _)| id.to_string()));
            // Gate the persistence on `agent_session_id.is_some()` —
            // only agent rows ever stamp that UUID, so its presence
            // is positive evidence the row was an agent. Rows with
            // both fields `None` could be legacy plain shells; writing
            // `claude` to those would be a permanent regression for
            // users who intend them as shells. Matches Copilot's
            // review on #230 and the same gate `reopen_session` uses.
            if entry.agent_id.is_none()
                && entry.agent_session_id.is_some()
                && let Some(id) = resolved_agent_id.clone()
            {
                pending_backfills.push((entry.session_id.clone(), id));
            }

            // Resume-by-id when we have an agent + its session UUID.
            // `build_resume_auto_type` returns `claude --resume <id>` /
            // `pi --session <id>` / `copilot --resume=<id>` depending on
            // the agent profile. Falls back to the agent's "most recent"
            // resume args when the persisted `agent_session_id` is
            // missing, and to `None` for plain shells.
            let agent_profile = resolved_agent_id
                .as_deref()
                .and_then(|aid| self.agent_registry.get_by_id(aid));
            let auto: Option<SharedString> = agent_profile
                .and_then(|profile| {
                    codescope_core::build_resume_auto_type(
                        profile,
                        entry.agent_session_id.as_deref(),
                    )
                })
                .map(SharedString::from);

            // Title mirrors C# `MainViewModel.HydrateFromLoaded`
            // (`legacy:CodeScope.Ui/ViewModels/MainViewModel.cs:1126-1133`):
            // when the worktree has a branch we override the
            // descriptor title with `{project} · {branch}`, otherwise
            // we fall back to the descriptor's own title — which
            // `CreateAgentSession` builds as `{agent.DisplayName} ·
            // {folderName}` and `CreateShellSession` builds as just
            // `Path.GetFileName(workingDirectory)`. `displayNameOverride`
            // is explicitly `null` at hydrate time in C#, so the
            // user-set `Session.DisplayName` does NOT participate in
            // the tab title here — it only flows into the sidebar
            // row title (a separate projection). Mirroring that
            // exactly avoids the asymmetry where a renamed session
            // with a branch would lose its rename on restart while
            // the same rename without a branch would survive.
            let folder_name = std::path::Path::new(&entry.worktree_path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| entry.worktree_path.clone());
            let title: SharedString = if let Some(branch) = entry.branch.as_deref() {
                SharedString::from(format!("{} · {}", entry.project_name, branch))
            } else if let Some(profile) = agent_profile {
                SharedString::from(format!("{} · {}", profile.display_name, folder_name))
            } else {
                SharedString::from(folder_name)
            };

            self.spawn_tab_in(
                Some(path),
                Some(title),
                auto,
                // Rehydrate path: pass `resolved_agent_id` so the
                // in-memory tab state knows which agent it's running,
                // even when projects.json still carries the legacy
                // `agent_id: null`. `restore_session_id = Some`
                // short-circuits the `allocate_session_id`
                // persistence branch — the disk-side backfill goes
                // through the `pending_backfills` flush below.
                resolved_agent_id.clone().map(SharedString::from),
                Some(entry.session_id.clone()),
                window,
                cx,
            );
            spawned_any = true;
            if active_in_group {
                let new_idx = self.groups[group_idx].tabs.len() - 1;
                active_by_group[group_idx] = Some(new_idx);
            }
        }

        // Re-enable layout persistence so the final flush below
        // actually hits disk. Even on the all-paths-missing branch we
        // need to clear this — otherwise a later user action would
        // silently no-op every save_layout.
        self.suppress_layout_save = false;

        // Heal legacy `agent_id = None` rows we rescued via the
        // default-agent fallback. One disk write per rehydrate
        // covers every row at once; subsequent boots skip the
        // fallback because the row now carries the resolved id.
        if !pending_backfills.is_empty() {
            let mut changed = false;
            for (sid, aid) in &pending_backfills {
                match codescope_core::SessionManager::update_agent_id(
                    &mut self.projects,
                    sid,
                    Some(aid),
                ) {
                    Ok(true) => changed = true,
                    Ok(false) => {}
                    Err(err) => {
                        eprintln!(
                            "warning: failed to backfill agent_id on rehydrate \
                             (session {sid}): {err:#}"
                        );
                    }
                }
            }
            if changed
                && let Err(err) = self.projects.save(&self.paths)
            {
                eprintln!("warning: failed to persist rehydrate backfill: {err:#}");
            }
        }

        if !spawned_any {
            // Every live session's worktree was missing — leave the
            // window empty instead of fabricating a default-shell tab
            // on a path the user already lost. Matches C#'s
            // "everything skipped → empty workspace" behaviour.
            self.focus_handle.focus(window);
            cx.notify();
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

        // Single flush of the complete `session_placements` snapshot.
        // All the per-spawn saves above were suppressed; this is the
        // one disk write that captures the final rehydrated layout.
        self.save_layout();
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
    /// wins over a project-root path match. Path comparison goes
    /// through [`codescope_core::path_canon::paths_match`] so a
    /// `C:\dev\repo` `wd` still matches a `c:/dev/repo` row in
    /// projects.json (slash direction + case both collapse) — the
    /// weaker `path_eq_ci` helper used to live here, but it only
    /// trimmed trailing separators and case-folded on Windows, so a
    /// spawn path that arrived with a different slash convention than
    /// the persisted worktree fell through to "no project found" and
    /// `allocate_session_id` minted a free-floating session row that
    /// never landed in `projects.json` — `soft_close` then couldn't
    /// find the id at tab close, the session never got a `closed_at`
    /// stamp, and it never appeared in the worktree's history list.
    /// Returns `(project_id, worktree_id)` so the caller can stamp
    /// both fields on the `Session` row exactly the way the C# build
    /// does.
    fn locate_project_for_path(&self, working_directory: &std::path::Path) -> Option<(String, Option<String>)> {
        let target = working_directory.to_string_lossy();
        for project in &self.projects.projects {
            for wt in &project.worktrees {
                if codescope_core::path_canon::paths_match(&wt.path, &target) {
                    return Some((project.id.clone(), Some(wt.id.clone())));
                }
            }
            if codescope_core::path_canon::paths_match(&project.path, &target) {
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

    /// Rewrite the title of every tab whose `working_directory` matches
    /// `path` so its branch segment reflects `new_branch`.
    ///
    /// Skips:
    /// * Tabs whose persisted [`Session`] row has an explicit
    ///   `display_name` (user-renamed via the Rename dialog — the
    ///   rename always wins, mirroring the "explicit user intent" rule).
    /// * Tabs whose title doesn't have the standard `Project · Branch`
    ///   shape — [`codescope_core::rebuild_title`] returns `None`,
    ///   which we treat as "leave it alone".
    /// * Tabs whose rebuilt title equals the current one (no work).
    ///
    /// Path matching uses [`codescope_core::path_canon::paths_match`]
    /// so a `C:\Dev\Repo` tab still matches a `c:/dev/repo` event
    /// (Windows path-case + slash normalisation).
    ///
    /// Fires `cx.notify()` only when at least one title actually
    /// changed — quiet ticks (poll re-confirmed the same branch)
    /// don't re-render.
    ///
    /// Mirrors C# `MainViewModel.RefreshTabTitlesForWorktree`.
    fn rename_tabs_for_path(
        &mut self,
        path: &std::path::Path,
        new_branch: &str,
        cx: &mut Context<Self>,
    ) {
        let event_path = path.to_string_lossy().into_owned();
        // Snapshot per-session display_name lookup so we don't borrow
        // `self.projects` across the mutable iteration over groups.
        // Sessions with an explicit display_name set must not be
        // overwritten — the user renamed them.
        let renamed_session_ids: HashSet<String> = self
            .projects
            .projects
            .iter()
            .flat_map(|p| p.sessions.iter())
            .filter(|s| {
                s.display_name
                    .as_deref()
                    .map(|d| !d.trim().is_empty())
                    .unwrap_or(false)
            })
            .map(|s| s.id.clone())
            .collect();

        let mut any_changed = false;
        for group in self.groups.iter_mut() {
            for tab in group.tabs.iter_mut() {
                let Some(wd) = tab.working_directory.as_deref() else {
                    continue;
                };
                if !codescope_core::path_canon::paths_match(
                    &wd.to_string_lossy(),
                    &event_path,
                ) {
                    continue;
                }
                if renamed_session_ids.contains(&tab.session_id) {
                    continue;
                }
                if let Some(new_title) =
                    codescope_core::rebuild_title(tab.title.as_ref(), new_branch)
                {
                    tab.title = SharedString::from(new_title);
                    any_changed = true;
                }
            }
        }
        if any_changed {
            cx.notify();
        }
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
        persist_agent_id: Option<String>,
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
            // Persist the agent the spawn was for so a later
            // `reopen_session` lookup resolves the right profile —
            // see the `persist_agent_id` doc on `spawn_tab_in` for
            // the full rationale. `None` ends up as `null` in
            // projects.json and lets reopen fall back to the
            // current `Settings.default_agent` (handled in
            // `reopen_session`).
            agent_id: persist_agent_id,
            display_name: None,
            worktree_id,
            last_opened: None,
            agent_session_id: None,
            closed_at: None,
        };
        match SessionManager::open(&mut self.projects, &project_id, session, &now_iso8601()) {
            Ok(_) => {
                if let Err(err) = self.projects.save(&self.paths) {
                    eprintln!("warning: failed to persist session open: {err:#}");
                }
            }
            Err(err) => {
                eprintln!("warning: SessionManager::open rejected new session: {err:#}");
            }
        }
        new_id
    }

    /// Find a persisted [`codescope_core::Session`] by id in the
    /// current `self.projects` snapshot. Returns a clone so callers
    /// can drop the borrow on `self` immediately — the layout
    /// rehydrate path needs that to call `spawn_tab_in` afterwards.
    /// `None` when the id no longer exists (project removed,
    /// hard-remove from history, race with a concurrent sidebar
    /// write).
    fn lookup_session_by_id(&self, session_id: &str) -> Option<codescope_core::Session> {
        for project in self.projects.projects.iter() {
            if let Some(s) = project.sessions.iter().find(|s| s.id == session_id) {
                return Some(s.clone());
            }
        }
        None
    }

    /// Mark `session_id` as soft-closed and persist. Called from
    /// `close_tab` so every Tab close lands in the closed-history
    /// list. Mirrors C# `SessionStore.SoftCloseSessionAsync`.
    /// Best-effort: a free-floating tab whose session id is not in
    /// `projects.json` (path matched no project at spawn time) is a
    /// silent no-op rather than an error. Reload-then-mutate-then-save
    /// keeps us in sync with concurrent sidebar writes.
    ///
    /// After a successful close we push the freshly-mutated
    /// `ProjectsConfig` into the sidebar entity — the sidebar keeps
    /// its own copy of `projects` and builds the per-worktree
    /// closed-session disclosure off it at render time, so without
    /// this push the new `closed_at` stamp would only become visible
    /// after the next sidebar-side mutation refreshed the snapshot.
    /// Mirrors the same `replace_projects` + `cx.notify()` dance
    /// `reopen_session` does for the reverse transition.
    fn soft_close_session(&mut self, session_id: &str, cx: &mut Context<Self>) {
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
                if let Err(err) = self.projects.save(&self.paths) {
                    eprintln!("warning: failed to persist session soft-close: {err:#}");
                    // Fall through to the sidebar mirror anyway —
                    // the in-memory `closed_at` stamp is correct,
                    // only the disk write failed. Letting the
                    // sidebar see the new state keeps both snapshots
                    // consistent until the next save retry; an early
                    // return here would leave the sidebar's copy
                    // stale until some unrelated mutation refreshed
                    // it, which is the exact "closed row doesn't
                    // show in history" symptom this helper is
                    // supposed to prevent. `reopen_session` uses
                    // the same pattern.
                }
                let projects_for_sidebar = self.projects.clone();
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.replace_projects(projects_for_sidebar);
                    cx.notify();
                });
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
    /// Open the themed ConfirmDialog and dispatch the carried
    /// [`crate::sidebar::ConfirmAction`] once the user resolves it.
    /// Bridges the sidebar's "I want to confirm this destructive
    /// action" event to the AppShell-owned dialog overlay. On
    /// confirm, sidebar-scoped actions go through
    /// [`Sidebar::execute_confirm_action`]; the
    /// `RemoveSessionFromHistory` variant is handled here because the
    /// session store lives on AppShell.
    pub(crate) fn handle_open_confirm_dialog(
        &mut self,
        spec: crate::dialogs::confirm::ConfirmSpec,
        action: crate::sidebar::ConfirmAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let rx = self.open_confirm_dialog(spec, window, cx);
        cx.spawn(async move |this, cx| {
            let confirmed = rx.await.unwrap_or(false);
            if !confirmed {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                match action {
                    crate::sidebar::ConfirmAction::RemoveSessionFromHistory {
                        session_id,
                    } => {
                        this.remove_session_from_history(session_id, cx);
                    }
                    other => {
                        this.sidebar.update(cx, |sidebar, cx| {
                            sidebar.execute_confirm_action(other, cx);
                        });
                    }
                }
            });
        })
        .detach();
    }

    /// Drop a closed-session row from history. Reload-then-mutate-
    /// then-save mirrors the `reopen_session` discipline so a sidebar
    /// write between two of ours can't clobber the array. Surfaces
    /// failures as toasts so the user sees the row stay in the
    /// disclosure instead of silently racing the on-disk file.
    /// Mirrors C# `RemoveSessionFromHistoryAsync` /
    /// `SessionStore.RemoveSessionAsync`.
    pub(crate) fn remove_session_from_history(
        &mut self,
        session_id: String,
        cx: &mut Context<Self>,
    ) {
        match ProjectsConfig::load(&self.paths) {
            Ok(cfg) => {
                self.projects = cfg;
            }
            Err(err) => {
                eprintln!(
                    "warning: failed to reload projects.json before remove-from-history: {err:#}"
                );
                self.push_toast(
                    ToastKind::Err,
                    SharedString::from("Remove failed"),
                    Some(SharedString::from(format!(
                        "Could not read projects.json: {err:#}"
                    ))),
                    cx,
                );
                return;
            }
        }
        if let Err(err) = SessionManager::hard_remove(&mut self.projects, &session_id) {
            self.push_toast(
                ToastKind::Err,
                SharedString::from("Remove failed"),
                Some(SharedString::from(format!("{err:#}"))),
                cx,
            );
            return;
        }
        if let Err(err) = self.projects.save(&self.paths) {
            self.push_toast(
                ToastKind::Err,
                SharedString::from("Remove failed"),
                Some(SharedString::from(format!("Failed to save: {err:#}"))),
                cx,
            );
            return;
        }
        // Mirror the updated config into the sidebar so the row
        // disappears from the history disclosure on this frame.
        self.mirror_projects_to_sidebar(cx);
    }

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
        // Legacy / pre-PR-#223 rows come back with `agent_id = None`
        // even though the user has been running them as an agent. The
        // launch-side fallback below picks `settings.default_agent` so
        // the tab still resumes correctly; mirror that decision into
        // the persisted row so the next cold-start rehydrate doesn't
        // need the same rescue.
        //
        // Gate the persistence on `agent_session_id.is_some()` — only
        // agent rows ever stamp that UUID, so its presence is positive
        // evidence the row was an agent. Rows with both fields `None`
        // could be legacy plain shells (or pre-fix agent rows that
        // never logged a session id); writing `claude` to those would
        // be a permanent regression for users who intend them as
        // shells. The narrower gate matches Copilot's review on #230.
        let backfill_agent_id: Option<String> = if restored.agent_id.is_none()
            && restored.agent_session_id.is_some()
        {
            default_agent_launch_for(&self.settings).map(|(id, _)| id.to_string())
        } else {
            None
        };
        if let Some(id) = backfill_agent_id.as_deref()
            && let Err(err) =
                SessionManager::update_agent_id(&mut self.projects, &session_id, Some(id))
        {
            eprintln!("warning: failed to backfill agent_id on reopen: {err:#}");
        }
        if let Err(err) = self.projects.save(&self.paths) {
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

        // `agent_id` → auto-type command. Looks up the persisted id
        // in `agent_registry` so non-Claude reopens
        // (Codex / OpenCode / Copilot / Pi, plus any
        // `settings.agents` overrides) come back with the right
        // command instead of dropping to a plain shell.
        //
        // When `Session.agent_session_id` is known we resume the
        // specific conversation via `resume_by_id_args` (`claude
        // --resume <id>`, `pi --session <id>`, `opencode --session
        // <id>`, `copilot --resume=<id>`). Without a stored id we
        // fall back to `resume_args` (`pi -c`, `copilot --continue`,
        // bare `claude`). Mirrors C# `SessionManager.CreateAgentSession`
        // resume branch + `JoinResumeByIdArgs`.
        //
        // Legacy / pre-fix rows persisted before the spawn-side
        // started stamping `agent_id` (or plain-shell sessions
        // closed under the old code path) come back with
        // `restored.agent_id = None`. Falling back to a plain shell
        // there is what surfaced the user-reported bug "reopen via
        // worktree double-click hands me a shell instead of
        // Claude". Resolve the current `Settings.default_agent` as
        // the rescue value so a legacy row still reopens in the
        // configured agent. New rows persisted after this PR will
        // carry their own `agent_id` and never need the fallback.
        let resolved_agent_id: Option<String> = restored
            .agent_id
            .clone()
            .or_else(|| default_agent_launch_for(&self.settings).map(|(id, _)| id.to_string()));
        let auto_type: Option<SharedString> = resolved_agent_id
            .as_deref()
            .and_then(|id| self.agent_registry.get_by_id(id))
            .and_then(|profile| {
                codescope_core::build_resume_auto_type(
                    profile,
                    restored.agent_session_id.as_deref(),
                )
            })
            .map(SharedString::from);

        self.spawn_tab_in(
            Some(working_directory),
            Some(title),
            auto_type,
            resolved_agent_id.map(SharedString::from),
            Some(restored.id),
            window,
            cx,
        );
    }

    fn focused_group(&self) -> &Group {
        &self.groups[self.focused_group]
    }

    /// Public entry-point for the "New project…" flow. The actual
    /// dialog lives on [`crate::sidebar::Sidebar`]; this helper hides
    /// the entity-update plumbing so call sites that don't carry the
    /// sidebar reference (command palette, empty-state CTA, etc.)
    /// stay terse. Mirrors `MainViewModel.OpenNewProjectDialog`.
    pub(crate) fn open_new_project_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.open_new_project_dialog(window, cx);
        });
    }

    /// Same as [`Self::open_new_project_dialog`] but lands the user
    /// directly on the "Clone from URL" tab. The Rust port's New
    /// Project dialog already implements the full clone flow
    /// (`git clone <url> <parent>/<name>` then register), so the
    /// empty-state secondary CTA can reuse it instead of being
    /// disabled the way the C# build's was.
    pub(crate) fn open_new_project_dialog_clone(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.open_new_project_dialog(window, cx);
            sidebar.set_new_project_mode(
                crate::dialogs::new_project::DialogMode::Clone,
                cx,
            );
        });
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
        // Overview and the diff viewer share the work-area slot;
        // flipping the Overview on dismisses an open diff viewer.
        if value {
            self.close_diff_viewer(cx);
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

    /// Read-only borrow of the session catalog. Exposed to the
    /// Overview module so it can flatten the live + closed rows
    /// without taking on a clone of the whole `ProjectsConfig` per
    /// render.
    pub(crate) fn projects_snapshot(&self) -> &ProjectsConfig {
        &self.projects
    }

    /// Snapshot of every currently-open tab's session id, deduped.
    /// Used by the Overview to filter `ProjectsConfig.sessions` down
    /// to actually-running rows (persisted `closed_at = None` can drift
    /// from live state — crashes leave orphans, layout-restored rows
    /// may never have been re-spawned). Mirrors C#
    /// `MainViewModel.OpenTabs.Select(t => t.SessionId).ToHashSet()`.
    pub(crate) fn live_session_ids(&self) -> std::collections::HashSet<String> {
        self.groups
            .iter()
            .flat_map(|g| g.tabs.iter().map(|t| t.session_id.clone()))
            .collect()
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
    pub(crate) fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        let theme = Arc::new(codescope_core::theme::builtin::by_name(&settings.theme));
        // Rebuild the AgentRegistry from the new settings so the
        // sidebar's "New session ▸" default-row reflects the freshly
        // chosen `default_agent` and any `agents` overrides. Without
        // this the sidebar keeps its construction-time registry and
        // the Settings dialog appears to do nothing for agent choice.
        let agent_registry =
            codescope_core::AgentRegistry::from_settings(&settings);
        self.settings = Arc::new(settings);
        self.theme = theme.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.apply_theme(theme, cx);
            sidebar.apply_agent_registry(agent_registry, cx);
        });
        cx.notify();
    }

    /// Swap the active theme for live preview without touching the
    /// `Settings` on disk or in memory. Used by the Settings dialog
    /// as the user clicks through the theme list — gives them a
    /// real-time look at each theme before committing via Save.
    /// Cancel explicitly reapplies the snapshot taken at open-time
    /// (see `cancel_settings_dialog`); the file-watch poller cannot
    /// undo the preview by itself because `settings.json` is never
    /// touched during a preview.
    pub(crate) fn set_theme_preview(&mut self, theme: Arc<Theme>, cx: &mut Context<Self>) {
        self.theme = theme.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.apply_theme(theme, cx);
        });
        cx.notify();
    }

    /// Read-only borrow of the on-disk path bundle. Used by the
    /// Settings dialog to write `settings.json`.
    pub(crate) fn paths_ref(&self) -> &Arc<AppPaths> {
        &self.paths
    }

    /// Push the AppShell-side `ProjectsConfig` mirror over to the
    /// sidebar's copy so the rendered list reflects the latest mutation
    /// on this frame. Same pattern `reopen_session` uses — extracted
    /// here so the Rename dialog (and any future AppShell-side mutator)
    /// doesn't duplicate the clone / `update` / `notify` triple.
    pub(crate) fn mirror_projects_to_sidebar(&mut self, cx: &mut Context<Self>) {
        let projects_for_sidebar = self.projects.clone();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.replace_projects(projects_for_sidebar);
            cx.notify();
        });
    }

    /// Read-only borrow of the current `Settings`. Used by the
    /// Settings dialog to seed its draft from the live in-memory
    /// state.
    pub(crate) fn settings_ref(&self) -> &Arc<Settings> {
        &self.settings
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
    ///
    /// `auto_type` is resolved from `Settings.default_agent` so the
    /// new tab boots into the user's preferred agent CLI. Falls back
    /// to a plain shell when the active project context is missing
    /// (no folder for the agent to operate in) or when no default
    /// agent is configured.
    fn spawn_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Only auto-type the default agent when we actually have a
        // working directory to land the agent in — a plain shell
        // fallback keeps the empty / no-project state usable.
        let has_project_context = self.sidebar.read(cx).active_project().is_some();
        let (agent_id, auto_type) = if has_project_context {
            match default_agent_launch_for(&self.settings) {
                Some((id, at)) => (Some(id), at),
                None => (None, None),
            }
        } else {
            (None, None)
        };
        self.spawn_tab_in(None, None, auto_type, agent_id, None, window, cx);
    }

    fn spawn_tab_in(
        &mut self,
        working_directory: Option<std::path::PathBuf>,
        title_override: Option<SharedString>,
        auto_type: Option<SharedString>,
        // Agent profile id to stamp on the persisted `Session` row.
        // Threaded through from `SidebarEvent::OpenSession` (or
        // resolved at the spawn site via
        // [`default_agent_launch_for`] when the emitter didn't know
        // yet whether a fresh spawn would happen). `None` is the
        // correct value for plain-shell spawns; non-`None` lets a
        // later `reopen_session` rebuild the agent's auto_type by
        // looking the id up in `AgentRegistry` instead of falling
        // back to a plain shell because the row was stored with
        // `agent_id: None`.
        //
        // Named `persist_agent_id` (not just `agent_id`) to keep it
        // out of the way of `Tab.agent_id` — the latter is a runtime
        // detection value derived from `auto_type` via
        // `agent_id_from_auto_type`, used for telemetry-bus routing,
        // and lives on the live `Tab` struct; this one is the
        // *persisted* identifier that ends up in `projects.json`
        // and is read back at reopen.
        persist_agent_id: Option<SharedString>,
        // `Some` on the launch-time rehydrate path so the freshly
        // spawned `Tab` adopts an existing session row rather than
        // appending a new one. `None` on every other call site
        // (sidebar click, dialog spawn, Ctrl+T) — those allocate a
        // fresh id and persist via `SessionManager::open`.
        restore_session_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

        // Resolve the shell + argv. When `auto_type` is set and we
        // have a working directory to land in *and* the default pwsh
        // shell is in play, boot the agent through
        // `pwsh -NoExit -Command "& { <agent> }"` so the CLI starts
        // immediately without a visible `PS C:\> claude` echoed line
        // — the regression this branch fixes. Mirrors C#
        // `SessionManager.CreateAgentSession`'s `ShellArgs` layout.
        //
        // The agent-via-`-Command` path is *Windows + pwsh* specific.
        // `CODESCOPE_SHELL` (dev override) and non-Windows targets fall
        // through to the auto-type fallback below, which writes the
        // agent command into the spawned shell after a short settle
        // delay — slightly slopier but preserves agent-launch behaviour
        // for any shell. `agent_launched_via_args` tracks which path we
        // took so the fallback only fires when needed.
        //
        // Plain shell tabs (`auto_type = None`) and agent tabs with no
        // working directory (no project context, nothing for the agent
        // to operate against) keep the previous bare-shell shape.
        let mut agent_launched_via_args = false;
        let shell = std::env::var("CODESCOPE_SHELL")
            .ok()
            .map(|program| Shell::new(program, Vec::new()))
            .or_else(|| {
                if cfg!(windows) {
                    let program = "pwsh.exe".to_string();
                    match (auto_type.as_ref(), working_directory.as_ref()) {
                        (Some(cmd), Some(wd)) => {
                            let args = codescope_core::build_agent_shell_args(
                                cmd.as_ref(),
                                &wd.to_string_lossy(),
                            );
                            agent_launched_via_args = true;
                            Some(Shell::new(program, args))
                        }
                        _ => Some(Shell::new(program, Vec::new())),
                    }
                } else {
                    None
                }
            });

        // Build the terminal palette + cursor preset from the active
        // theme + settings. Cloned per spawn so each tab carries its
        // own snapshot — themes can swap later without breaking
        // already-running terminals.
        let palette = ColorPalette::from_theme_palette(&self.theme.palette);
        let cursor_preset = CursorStylePreset {
            shape: cursor_shape_from_setting(self.settings.cursor.shape),
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

        let terminal = cx.new(|cx| {
            TerminalView::new(
                backend,
                palette,
                font,
                working_directory_for_tab.clone(),
                cx,
            )
        });
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
        let session_id = self.allocate_session_id(
            working_directory_for_tab.as_deref(),
            persist_agent_id.as_ref().map(|s| s.to_string()),
            restore_session_id,
        );
        let group_idx = self.focused_group;
        let group = &mut self.groups[group_idx];
        // Capture the entity so the fallback `auto_type` job below can
        // write to it without re-borrowing `self.groups` after the
        // await point.
        let terminal_for_autotype = terminal.clone();
        let agent_id = codescope_core::agent_id_from_auto_type(
            auto_type.as_ref().map(|s| s.as_ref()),
        );
        // `auto_type` is kept on the Tab as metadata (agent
        // detection via `agent_id_from_auto_type` + layout persistence
        // so a restart can rebuild the command). On the Windows + pwsh
        // happy path it doesn't drive a post-spawn pty write any more
        // because the agent is already booted via
        // `pwsh -Command "& { ... }"`, mirroring C#
        // `SessionManager.CreateAgentSession`. On the
        // `CODESCOPE_SHELL` override path and non-Windows targets the
        // post-spawn auto-type below is still the launch mechanism.
        group.tabs.push(Tab {
            id,
            session_id,
            title,
            terminal,
            working_directory: working_directory_for_tab,
            spawned_at: SystemTime::now(),
            adopted_session_id: None,
            fired_session_ids: std::collections::HashSet::new(),
            agent_id,
        });
        let new_idx = group.tabs.len() - 1;
        self.activate_tab(group_idx, new_idx, window, cx);
        // Persist the placement now so a crash before the next state
        // change (close / split / drag) still leaves a `layout.json`
        // that mentions this session's group. `activate_tab` only
        // saves when focused_group actually changes — for a spawn
        // into the already-focused group it wouldn't fire, so the
        // tab would come back in group 0 next launch even though
        // `projects.json` carries the live row. The `projects.json`
        // write already happened inside `allocate_session_id`.
        self.save_layout();

        // Fallback: auto-type the agent command into the shell when we
        // *couldn't* build the agent into the shell argv up-front
        // (CODESCOPE_SHELL override / non-Windows targets). Without
        // this the agent would never launch on those paths. Skipped on
        // the Windows + pwsh happy path so the user no longer sees a
        // `PS C:\> claude` echoed line — the regression this branch
        // fixes.
        if !agent_launched_via_args
            && let Some(cmd) = auto_type
        {
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
            self.soft_close_session(&codescope_session_id, cx);
        }
        let Some(group) = self.groups.get_mut(group_idx) else { return };
        if tab_idx >= group.tabs.len() {
            return;
        }
        let was_focused_group = self.focused_group == group_idx;
        group.tabs.remove(tab_idx);
        if group.tabs.is_empty() {
            // Empty group — collapse if there are siblings. When this
            // is the last surviving group we keep it as an empty
            // workspace so the user can open a new session from the
            // sidebar (double-click a worktree, Ctrl+Shift+T, "+ new
            // tab") without losing the whole window. The app only
            // exits when the user closes the window itself (caption X
            // / Alt+F4 / Cmd+Q / etc.).
            if self.groups.len() == 1 {
                // Without a focused terminal the AppShell-level
                // key handler is the only thing listening; route
                // focus back to the shell root so Ctrl+Shift+T (and
                // every other app chord) still fires from the empty
                // state. Copilot caught this on PR #184.
                self.focus_handle.focus(window);
                // No focused tab left — clear the sidebar's active
                // context highlight (issue #248).
                self.push_sidebar_active_context(cx);
                cx.notify();
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
            // Renormalise the surviving weights so they sum back to
            // their length (i.e. average 1.0 each, ratios preserved).
            //
            // Taffy (gpui 0.2.2's flexbox engine) implements the CSS
            // "flex factor < 1" rule literally: when the sum of
            // `flex-grow` across a flex line is below `1.0`, only
            // `sum * free_space` gets distributed and the rest stays
            // blank. See
            // `taffy-0.9.0/src/compute/flexbox.rs:1260`:
            //
            //     let free_space = if growing && sum_flex_grow < 1.0 {
            //         initial_free_space * sum_flex_grow - …
            //     };
            //
            // If the user drags a splitter hard (e.g. weights
            // `[1.5, 0.5]`) and then closes the larger column, the
            // survivor inherits `weight = 0.5`. With a single flex
            // child and `sum_flex_grow = 0.5 < 1.0`, taffy hands it
            // exactly 50 % of the work-area width and leaves the
            // other 50 % empty — the symptom the user reported as
            // "leftover empty space where the closed group used to
            // be". WPF `*` columns sidestep the same edge case
            // because they normalise the star sum on every layout
            // pass; the Rust flex pipeline needs us to renormalise
            // explicitly. Scaling so the sum equals the surviving
            // count keeps every weight ≥ MIN_GROUP_WEIGHT (modulo
            // rounding) and the layout proportional, while pinning
            // the sum at ≥ 1.0 for any non-empty workspace.
            normalise_group_weights(&mut self.group_weights);
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
        // The AppShell root has `track_focus(&self.focus_handle)`, which makes
        // gpui register a bubble-phase mouse-down listener that auto-focuses
        // AppShell on any click inside it (see `paint_mouse_listeners` in
        // gpui's `div.rs`). When a tab is clicked, our user listener fires
        // first (bubble runs deepest-child first), focuses the terminal, then
        // AppShell's auto-focus fires and steals focus back — leaving the
        // first keystrokes after the tab switch unrouted. `prevent_default`
        // is the gpui-blessed way to suppress that ancestor auto-focus; it
        // resets per dispatch so calls outside an event are harmless.
        window.prevent_default();
        // Follow the focused tab in the sidebar — highlight the
        // project/worktree row it belongs to (issue #248). Cheap +
        // no-ops in the sidebar when the context is unchanged.
        self.push_sidebar_active_context(cx);
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
        // See `close_tab` for the rationale: a lingering fractional
        // weight sum (e.g. `0.5` after a heavy splitter drag, then
        // closing the dominant column) triggers taffy's "flex factor
        // < 1" branch and leaves the survivor occupying only that
        // fraction of the work area. Rescaling so the sum equals the
        // surviving count keeps ratios but pins the sum at the count,
        // which lets gpui's flex layout fill the workspace.
        normalise_group_weights(&mut self.group_weights);
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
        // The newly-focused group is empty, so there's no active tab —
        // clear the sidebar's active-context wash (#248). It returns
        // when `activate_tab` runs for a tab opened here.
        self.push_sidebar_active_context(cx);
        cx.notify();
        self.save_layout();
    }

    /// Peel the named tab out of its current group and into a fresh
    /// group immediately to the right. Fired from the tab right-click
    /// menu's "Move to new group" row. No-op when the tab can't be
    /// resolved (concurrent close / drag) or when it's already the
    /// sole tab in its group (move would just shuffle empty groups).
    /// Mirrors the C# `MainViewModel.MoveTabToNewGroup` command.
    fn move_tab_to_new_group(
        &mut self,
        source_group_id: u64,
        source_tab_id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(source_idx) =
            self.groups.iter().position(|g| g.id == source_group_id)
        else {
            self.close_tab_menu(cx);
            return;
        };
        // Refuse the move when the tab is already alone in its group —
        // the C# build also bails here (no behaviour change, just an
        // extra empty group).
        if self.groups[source_idx].tabs.len() <= 1 {
            self.close_tab_menu(cx);
            return;
        }
        let Some(tab_pos) = self.groups[source_idx]
            .tabs
            .iter()
            .position(|t| t.id == source_tab_id)
        else {
            self.close_tab_menu(cx);
            return;
        };

        let id = self.next_group_id;
        self.next_group_id += 1;
        let insert_at = source_idx + 1;
        self.groups.insert(
            insert_at,
            Group { id, tabs: Vec::new(), active_tab: 0 },
        );
        self.group_weights.insert(insert_at, 1.0);

        // Re-resolve the source row's index — the insert above pushed
        // groups to the right of `source_idx` down by one; `source_idx`
        // itself stayed put because we inserted *after* it.
        let tab = self.groups[source_idx].tabs.remove(tab_pos);
        // Slide the source's active-tab cursor if we removed at or
        // before it; same logic as `move_tab_to_group`'s cross-group
        // arm.
        let g = &mut self.groups[source_idx];
        if g.active_tab >= g.tabs.len() {
            g.active_tab = g.tabs.len().saturating_sub(1);
        } else if g.active_tab > tab_pos {
            g.active_tab -= 1;
        }

        self.groups[insert_at].tabs.push(tab);
        self.groups[insert_at].active_tab = 0;
        self.focused_group = insert_at;
        self.close_tab_menu(cx);
        self.activate_tab(insert_at, 0, window, cx);
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
        let danger = theme::danger();
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
                    .children(t.action.as_ref().map(|action| {
                        let toast_id = t.id;
                        let kind = action.kind.clone();
                        let label = action.label.clone();
                        div()
                            .id(("toast-action", toast_id))
                            .px_3()
                            .py_1()
                            .rounded_sm()
                            .bg(ink_dim)
                            .text_color(elevated)
                            .text_size(px(11.0))
                            .cursor_pointer()
                            .hover(|s| s.opacity(0.85))
                            .child(label)
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.dispatch_toast_action(toast_id, kind.clone(), cx);
                                }),
                            )
                    }))
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
                        crate::notifications::NotificationKind::SessionWaiting => signal_warn,
                        crate::notifications::NotificationKind::SessionReady => accent,
                        crate::notifications::NotificationKind::UpdateAvailable => accent,
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
        // Snapshot per-tab data needed by the Reveal / Copy / Rename
        // rows. Looking up by id (not position) keeps these stable
        // against concurrent tab mutation between menu-open and click.
        let tab = group.tabs.get(pivot_pos)?;
        let tab_session_id = tab.session_id.clone();
        let tab_title = tab.title.clone();
        let tab_working_dir = tab.working_directory.clone();
        let tab_working_dir_str = tab_working_dir
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned());
        // "Move to new group" is meaningful only when there are
        // siblings worth peeling away from; a single-tab single-group
        // workspace would just no-op into the same shape it's already
        // in. Mirrors WPF's `IsEnabled` gating on the equivalent row.
        let can_move_to_new_group = has_others || self.groups.len() > 1;

        let elevated = theme::elevated(theme);
        let divider = theme::divider(theme);
        let ink = theme::ink(theme);
        let ink_dim = theme::ink_dim(theme);
        let ink_ghost = theme::ink_ghost(theme);
        let frost = theme::frost_10(theme);
        let danger = theme::danger();

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

        let header_title = tab_title.clone();
        let header_subtitle: SharedString = tab_working_dir
            .as_ref()
            .and_then(|p| p.file_name().map(|s| s.to_string_lossy().into_owned()))
            .map(SharedString::from)
            .unwrap_or_else(|| SharedString::from("session"));
        let reveal_path = tab_working_dir_str.clone();
        let wt_path = tab_working_dir_str.clone();
        let copy_path = tab_working_dir_str.clone();
        let copy_session_id = tab_session_id.clone();
        let rename_session_id = tab_session_id.clone();
        let rename_current = tab_title.to_string();
        // "Open remote" / "Open PR" gating mirrors the sidebar's
        // worktree-row menu — same `Sidebar` accessors so the two
        // surfaces agree row-for-row. We only emit the rows when the
        // tab's working directory matches a tracked worktree (Open
        // remote) and the PR cache has a `Resolved { info: Some(_) }`
        // entry whose branch still matches the live branch (Open PR).
        // Each path is captured into its own `Option<String>` so the
        // closure for each row only takes ownership of the path it
        // needs.
        let (open_remote_path, open_pr_path) = {
            let sidebar = self.sidebar.read(cx);
            match tab_working_dir_str.as_ref() {
                Some(path) => {
                    let tracked = sidebar.path_is_tracked_worktree(path);
                    let has_pr = sidebar.pr_url_for_path(path).is_some();
                    (
                        tracked.then(|| path.clone()),
                        has_pr.then(|| path.clone()),
                    )
                }
                None => (None, None),
            }
        };
        // Snapshot for the divider gate — the row builders below
        // consume the `Option<String>`s by `.map(|path| …)` and we
        // need the boolean answer to "should we paint a divider
        // above the section" regardless of which row is enabled.
        let has_open_in_browser_rows = open_remote_path.is_some() || open_pr_path.is_some();

        let mut menu_body = div()
            .flex()
            .flex_col()
            .py_1()
            .min_w(px(220.0))
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
            // Contextual header — mirrors `ContextMenuFactory.BuildContextHeader`
            // in `GroupStripView.OnTabContextMenuOpening`: mono title at
            // 11 px, sans subtitle at 10 px. Non-interactive.
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(px(10.0))
                    .text_color(ink_ghost)
                    .child(
                        div()
                            .text_color(ink)
                            .font(theme::font_mono())
                            .text_size(px(11.0))
                            .truncate()
                            .child(header_title),
                    )
                    .child(div().truncate().child(header_subtitle)),
            )
            .child(div().h_px().bg(divider).my_1())
            // ── Reveal section (mirrors C# `GroupStripView`'s
            // `PopulateTabContextMenu` Reveal block) ──
            .child(item(
                "tab-menu-reveal",
                crate::sidebar::reveal_in_file_browser_label(),
                reveal_path.is_some(),
                false,
                Box::new(move |this, _window, cx| {
                    if let Some(p) = reveal_path.clone() {
                        crate::sidebar::reveal_path_in_file_browser(&p);
                    }
                    this.close_tab_menu(cx);
                }),
            ));
        if cfg!(target_os = "windows") {
            menu_body = menu_body.child(item(
                "tab-menu-wt",
                "Open in Windows Terminal",
                wt_path.is_some(),
                false,
                Box::new(move |this, _window, cx| {
                    if let Some(p) = wt_path.clone() {
                        crate::sidebar::open_path_in_windows_terminal(&p);
                    }
                    this.close_tab_menu(cx);
                }),
            ));
        }
        let menu_body = menu_body
            .child(item(
                "tab-menu-copy-path",
                "Copy path",
                copy_path.is_some(),
                false,
                Box::new(move |this, _window, cx| {
                    if let Some(p) = copy_path.clone() {
                        cx.write_to_clipboard(ClipboardItem::new_string(p));
                    }
                    this.close_tab_menu(cx);
                }),
            ))
            .child(item(
                "tab-menu-copy-session-id",
                "Copy session id",
                true,
                false,
                Box::new(move |this, _window, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(
                        copy_session_id.clone(),
                    ));
                    this.close_tab_menu(cx);
                }),
            ))
            .child(div().h_px().bg(divider).my_1())
            // ── Manage section ──
            .child(item(
                "tab-menu-rename",
                "Rename tab…",
                true,
                false,
                Box::new(move |this, window, cx| {
                    let target = RenameRequest::Session {
                        session_id: rename_session_id.clone(),
                    };
                    let current = rename_current.clone();
                    this.close_tab_menu(cx);
                    this.open_rename_dialog(target, current, window, cx);
                }),
            ))
            .child(item(
                "tab-menu-move-new-group",
                "Move to new group",
                can_move_to_new_group,
                false,
                Box::new(move |this, window, cx| {
                    this.move_tab_to_new_group(group_id, tab_id, window, cx);
                }),
            ))
            // ── Open in browser section ──
            //
            // Both rows are gated against `Sidebar` state at menu-build
            // time so the menu height collapses cleanly when neither
            // surface applies (e.g. a plain shell tab with no project
            // context, or a worktree whose PR cache hasn't landed).
            // We don't render a greyed-out unclickable affordance —
            // the row disappears entirely instead. Mirrors the C#
            // tab menu's `HasOriginRemote` / `HasPullRequest` gates
            // in `GroupStripView.PopulateTabContextMenu`.
            .children(
                has_open_in_browser_rows
                    .then(|| div().h_px().bg(divider).my_1().into_any_element()),
            )
            .children(open_remote_path.map(|path| {
                item(
                    "tab-menu-open-remote",
                    "Open remote in browser",
                    true,
                    false,
                    Box::new(move |this, _window, cx| {
                        let path = path.clone();
                        this.close_tab_menu(cx);
                        this.sidebar.update(cx, |sidebar, cx| {
                            sidebar.open_remote_in_browser_for_path(path, cx);
                        });
                    }),
                )
                .into_any_element()
            }))
            .children(open_pr_path.map(|path| {
                item(
                    "tab-menu-open-pr",
                    "Open PR in browser",
                    true,
                    false,
                    Box::new(move |this, _window, cx| {
                        let path = path.clone();
                        this.close_tab_menu(cx);
                        this.sidebar.update(cx, |sidebar, cx| {
                            sidebar.open_pr_in_browser_for_path(path, cx);
                        });
                    }),
                )
                .into_any_element()
            }))
            .child(div().h_px().bg(divider).my_1())
            // ── Close section ──
            .child(item(
                "tab-menu-close",
                "Close tab",
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
                "Close other tabs",
                has_others,
                false,
                Box::new(move |this, window, cx| {
                    this.close_other_tabs_in_group(group_id, tab_id, window, cx);
                }),
            ))
            .child(item(
                "tab-menu-close-right",
                "Close tabs to the right",
                has_right,
                true,
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
            let mut row = div().flex().flex_row().items_center().gap(px(6.0)).child(
                // Branch icon — matches the C# git-dirty Path (three
                // commit dots joined by a branch).
                svg()
                    .path("icons/branch.svg")
                    .w(px(12.0))
                    .h(px(12.0))
                    .text_color(ink_dim),
            );
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
                .gap(px(6.0))
                .text_color(ink)
                .child(
                    // Sync icon — down-arrow + base line, matches
                    // the C# remote-delta Path.
                    svg()
                        .path("icons/sync.svg")
                        .w(px(12.0))
                        .h(px(12.0))
                        .text_color(ink_dim),
                )
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
                .gap(px(6.0))
                .text_color(ink_muted)
                .child(
                    // Cube icon — matches the C# `StatusBarView`
                    // model Path (12 × 12, stroke=currentColor).
                    svg()
                        .path("icons/model.svg")
                        .w(px(12.0))
                        .h(px(12.0))
                        .text_color(ink_dim),
                )
                .child(m)
        });

        let tokens_segment = snapshot.as_ref().filter(|s| s.tokens_used > 0).map(|s| {
            let used = codescope_core::format_tokens(s.tokens_used);
            let mut row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.0))
                .child(
                    // Tokens / lines icon — matches the C# token
                    // Path (three horizontal lines).
                    svg()
                        .path("icons/tokens.svg")
                        .w(px(12.0))
                        .h(px(12.0))
                        .text_color(ink_dim),
                )
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
                    .gap(px(6.0))
                    .text_color(ink_dim)
                    .child(
                        // Clock icon — matches the C# duration
                        // Path (circle + minute hand).
                        svg()
                            .path("icons/clock.svg")
                            .w(px(12.0))
                            .h(px(12.0))
                            .text_color(ink_dim),
                    )
                    .child(codescope_core::ClaudeTranscriptTail::format_duration(d))
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
                .child(
                    // Worktree (folder) icon — a small folder glyph
                    // signals the workspace-summary cluster. The C#
                    // build leans on the small-caps "N worktrees"
                    // label alone; in the dense Rust port the icon
                    // makes the cluster scannable.
                    svg()
                        .path("icons/worktree.svg")
                        .w(px(12.0))
                        .h(px(12.0))
                        .text_color(ink_dim),
                )
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

        // ─── Settings gear button ────────────────────────────────
        //
        // Sits just-left of the bell in the status bar's right cluster.
        // Left-click opens the Settings dialog directly — same entry
        // point as the Ctrl+Shift+, chord and the command palette's
        // "Open settings" row. The gear deliberately has *no*
        // right-click menu; the discoverable "Settings…" row lives
        // in the sidebar project context menu instead (see
        // `Sidebar::render_project_menu`), which is the more natural
        // home alongside the rest of the project-scoped actions.
        let gear_btn = div()
            .id("status-gear-btn")
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
                cx.listener(|this, _, window, cx| {
                    cx.stop_propagation();
                    this.open_settings_dialog(window, cx);
                }),
            )
            // 12 × 12 gear SVG; `text_color` drives the `currentColor`
            // stroke so the tint tracks `text_faint`. Same sizing as
            // the branch / sync / clock segments in the left cluster.
            .child(
                svg()
                    .path("icons/settings.svg")
                    .w(px(12.0))
                    .h(px(12.0))
                    .text_color(ink_muted),
            );

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
            // Bell SVG — same path the C# `StatusBarView` Bell
            // Path uses (14 × 14 viewBox, stroke=currentColor). The
            // svg element picks up `text_color`, so the bell tints
            // to match the surrounding muted ink without any extra
            // wiring.
            .child(
                svg()
                    .path("icons/bell.svg")
                    .w(px(14.0))
                    .h(px(14.0))
                    .text_color(ink_muted),
            )
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
            // Gear + bell share the final cluster — same separator
            // discipline as the rest of the bar (separators sit between
            // clusters, not inside them), so the two icons read as a
            // single "actions" group on the right.
            clusters.push(vec![
                gear_btn.into_any_element(),
                bell_btn.into_any_element(),
            ]);
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
                if let Some(snap) = self.telemetry_for(sid)
                    && matches!(
                        snap.state,
                        codescope_core::SessionState::Busy
                            | codescope_core::SessionState::PendingToolUse
                    ) {
                        busy.insert(canon);
                    }
            }
        }
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_session_paths(busy, active, cx);
        });
    }

    /// Canonicalised worktree path of the currently focused tab (the
    /// active tab in the focused group), or `None` when the focused
    /// group is empty or the tab has no working directory. Unlike the
    /// `push_sidebar_session_paths` sets, this does *not* require an
    /// adopted agent session — a plain shell tab still has a worktree
    /// context worth highlighting (issue #248).
    pub(crate) fn focused_tab_worktree_path(&self) -> Option<String> {
        let group = self.groups.get(self.focused_group)?;
        let tab = group.tabs.get(group.active_tab)?;
        let wd = tab.working_directory.as_ref()?;
        let canon = codescope_core::path_canon::canonicalize_path(&wd.to_string_lossy());
        (!canon.is_empty()).then_some(canon)
    }

    /// Push the focused tab's worktree path to the sidebar so the
    /// matching project/worktree row gets the accent-tinted "active
    /// context" highlight (issue #248). Called from `activate_tab` (the
    /// universal tab-activation funnel) and from `close_tab` when the
    /// last tab closes and there's no longer a focused tab.
    fn push_sidebar_active_context(&self, cx: &mut Context<Self>) {
        let path = self.focused_tab_worktree_path();
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.set_active_context(path, cx);
        });
    }

    /// Recompute the OS-level taskbar / dock overlay from the
    /// current agent rollup. Mirrors C#
    /// `MainViewModel.RecomputeTaskbarBadge` — busy / agent counts
    /// flow from the same telemetry source the status-bar segment
    /// already reads. The badge driver no-ops when the state hasn't
    /// changed, so calling this from a 250 ms busy-poll is cheap.
    fn refresh_taskbar_badge(&mut self) {
        let (busy, idle) = self.agent_rollup_counts();
        let agents = busy + idle;
        self.taskbar_badge.apply(busy, agents);
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

    /// Snapshot the updater state slot and surface any state-machine
    /// transitions as toasts / notifications. Called every render
    /// tick — cheap because the snapshot is just an `Arc::read +
    /// clone` and the per-state matching is constant time.
    ///
    /// Idempotency: each transition is guarded by `last_announced_update`
    /// so a state that lingers across multiple frames does not retoast.
    fn surface_update_state(&mut self, cx: &mut Context<Self>) {
        let snapshot = crate::update::snapshot(&self.update_state);
        match snapshot {
            crate::update::UpdateStatus::Available(info) => {
                if self.last_announced_update.as_deref() == Some(info.tag.as_str()) {
                    return;
                }
                self.last_announced_update = Some(info.tag.clone());

                let action_kind = if cfg!(target_os = "macos") {
                    ToastActionKind::OpenReleasesPage
                } else {
                    ToastActionKind::ApplyUpdate(info.clone())
                };

                self.push_action_toast(
                    ToastKind::Info,
                    format!("CodeScope {} is available", info.tag),
                    Some(
                        format!("Current version: v{}", env!("CARGO_PKG_VERSION"))
                            .into(),
                    ),
                    ToastAction {
                        label: "Update".into(),
                        kind: action_kind,
                    },
                    cx,
                );

                self.notifications.push(
                    crate::notifications::NotificationKind::UpdateAvailable,
                    format!("Update available — {}", info.tag),
                    "Click the update toast or check the releases page.",
                    None,
                );
            }
            crate::update::UpdateStatus::Downloading { received, total } => {
                let detail = match total {
                    Some(total) if total > 0 => Some(
                        format!("{} / {}", fmt_bytes(received), fmt_bytes(total)).into(),
                    ),
                    _ => Some(fmt_bytes(received).into()),
                };
                self.set_progress_toast("Downloading update…", detail, cx);
            }
            crate::update::UpdateStatus::Installing => {
                self.set_progress_toast("Installing update…", Some("Almost done.".into()), cx);
            }
            crate::update::UpdateStatus::Ready(info) => {
                if let Some(id) = self.update_progress_toast_id.take() {
                    self.toasts.retain(|t| t.id != id);
                }
                let sentinel = format!("{}-ready", info.tag);
                if self.last_announced_update.as_deref() == Some(sentinel.as_str()) {
                    return;
                }
                self.last_announced_update = Some(sentinel);
                self.push_action_toast(
                    ToastKind::Ok,
                    "Update installed",
                    Some("Restart CodeScope to activate.".into()),
                    ToastAction {
                        label: "Restart".into(),
                        kind: ToastActionKind::RestartForUpdate,
                    },
                    cx,
                );
            }
            crate::update::UpdateStatus::Failed { message } => {
                if let Some(id) = self.update_progress_toast_id.take() {
                    self.toasts.retain(|t| t.id != id);
                }
                // Sentinel keyed on the message so a *different* failure
                // surfaces a new toast. `last_announced_update` is not
                // cleared on dismiss, so an *identical* message stays
                // de-duped for the rest of the session — including after
                // the user dismisses the toast. That's deliberate:
                // re-toasting the same error every 3h poll would be spam.
                // A genuinely new error (different message) still shows.
                let sentinel = format!("failed:{message}");
                if self.last_announced_update.as_deref() == Some(sentinel.as_str()) {
                    return;
                }
                self.last_announced_update = Some(sentinel);
                self.push_toast(
                    ToastKind::Err,
                    "Update failed",
                    Some(message.into()),
                    cx,
                );
            }
            _ => {}
        }
    }

    /// Ensure the live progress toast exists and rewrite its title/detail.
    /// Pushes a persistent (non-expiring, action-less) toast on first call
    /// and reuses it afterwards so per-frame progress updates mutate in
    /// place instead of stacking new toasts.
    fn set_progress_toast(
        &mut self,
        title: impl Into<SharedString>,
        detail: Option<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let title = title.into();
        // Once we've pushed a progress toast for this install, only ever
        // update it in place — never re-push. If the user dismissed it
        // (×) mid-download, `find` returns None and we respect that
        // rather than re-spawning the toast every frame. The id is reset
        // to None on Ready / Failed (via `.take()`), so the next install
        // starts fresh.
        if let Some(id) = self.update_progress_toast_id {
            if let Some(toast) = self.toasts.iter_mut().find(|t| t.id == id) {
                toast.title = title;
                toast.detail = detail;
                cx.notify();
            }
            return;
        }
        // First progress frame of this install — push the persistent toast.
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts.push_front(Toast {
            id,
            kind: ToastKind::Info,
            title,
            detail,
            expires_at: None,
            action: None,
        });
        self.update_progress_toast_id = Some(id);
        cx.notify();
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
            expires_at: Some(Instant::now() + lifetime),
            action: None,
        });
        while self.toasts.len() > TOAST_VISIBLE_CAP {
            self.toasts.pop_back();
        }
        cx.notify();
    }

    /// Push a persistent toast that carries an action button. Auto-
    /// dismiss is disabled; the user dismisses by clicking the action
    /// or the × on the toast.
    ///
    /// Cap-eviction respects persistence: a flurry of regular toasts
    /// won't drop the persistent one off the back. If we're still over
    /// cap after removing all expirables (extremely unlikely — would mean
    /// >TOAST_VISIBLE_CAP open action prompts at once) the oldest
    /// persistent wins.
    pub(crate) fn push_action_toast(
        &mut self,
        kind: ToastKind,
        title: impl Into<SharedString>,
        detail: Option<SharedString>,
        action: ToastAction,
        cx: &mut Context<Self>,
    ) {
        let id = self.next_toast_id;
        self.next_toast_id += 1;
        self.toasts.push_front(Toast {
            id,
            kind,
            title: title.into(),
            detail,
            expires_at: None,
            action: Some(action),
        });
        while self.toasts.len() > TOAST_VISIBLE_CAP {
            let drop_idx = self
                .toasts
                .iter()
                .rposition(|t| t.action.is_none())
                .or_else(|| self.toasts.len().checked_sub(1));
            match drop_idx {
                Some(i) => {
                    self.toasts.remove(i);
                }
                None => break,
            }
        }
        cx.notify();
    }

    /// Handle a click on a toast action button. Routes per
    /// `ToastActionKind`; dismisses the source toast after the action
    /// fires.
    pub(crate) fn dispatch_toast_action(
        &mut self,
        toast_id: u64,
        kind: ToastActionKind,
        cx: &mut Context<Self>,
    ) {
        self.toasts.retain(|t| t.id != toast_id);
        match kind {
            ToastActionKind::ApplyUpdate(info) => {
                crate::update::start_install(self.update_state.clone(), info);
            }
            ToastActionKind::RestartForUpdate => {
                // Graceful quit rather than process::exit(0): runs
                // gpui's normal shutdown so the user can relaunch into
                // the freshly-swapped binary. layout.json / projects.json
                // are already flushed synchronously on mutation; pending
                // window geometry is debounced and treated as droppable
                // at shutdown by design (see the window-save debounce
                // loop in AppShell::new).
                cx.quit();
            }
            ToastActionKind::OpenReleasesPage => {
                if let Err(err) = crate::update::open_releases_page() {
                    self.push_toast(
                        ToastKind::Err,
                        "Could not open browser",
                        Some(format!("{err:#}").into()),
                        cx,
                    );
                }
            }
        }
        cx.notify();
    }

    /// Walk every adopted-agent tab, diff its current telemetry state
    /// against the last-observed state, and push a bell-notification
    /// entry on the same transitions C# `PushActivityNotification`
    /// covers:
    ///
    /// - `* → PendingToolUse` (where previous state ≠ `PendingToolUse`)
    ///   → `SessionWaiting` "Needs attention" / "Agent paused on a tool
    ///   prompt."
    /// - `(Busy | PendingToolUse) → Idle` → `SessionReady` "Ready" /
    ///   "Turn complete[ · <turn-duration>]".
    ///
    /// Suppresses the notification when the tab is the one the user is
    /// currently looking at (focused group's active tab) — bell noise
    /// for the front tab adds nothing the user can't already see.
    ///
    /// Driven from `start_telemetry_poll` after every tail poll so the
    /// 250 ms cadence already in place doubles as the activity-watch
    /// cadence — no separate timer needed.
    fn apply_activity_notifications(&mut self, cx: &mut Context<Self>) {
        // Identify the currently-focused tab so we can skip its
        // notifications. `(group_id, tab_id)` keeps the comparison
        // stable across tab moves / index shifts.
        let focused_tab_key: Option<(u64, u64)> = self
            .groups
            .get(self.focused_group)
            .and_then(|g| g.tabs.get(g.active_tab).map(|t| (g.id, t.id)));

        struct Pending {
            kind: crate::notifications::NotificationKind,
            title: SharedString,
            detail: SharedString,
            session_title: SharedString,
        }
        // (session_id, tab_title, detail) — fired on the background
        // executor below as OS toasts. Separate from `pending` because
        // the OS toast intentionally does *not* honour the
        // focused-tab suppression (the gate is "is the user looking
        // at the app", not "which tab is in front"), matching the C#
        // build's split.
        let mut pending_toasts: Vec<(String, String, String)> = Vec::new();
        let mut pending: Vec<Pending> = Vec::new();
        // Only records *changed* (sid, state) pairs so the steady-state
        // tick — every tab same as last time — stays allocation-free.
        // The String clone of the session id only happens on a real
        // transition, not on the 4× per second no-op poll.
        let mut state_updates: Vec<(String, codescope_core::SessionState)> = Vec::new();

        for group in &self.groups {
            for tab in &group.tabs {
                let Some(sid) = tab.adopted_session_id.as_deref() else { continue };
                let Some(snap) = self.telemetry_for(sid) else { continue };
                let prev = self
                    .last_session_state
                    .get(sid)
                    .copied()
                    .unwrap_or(codescope_core::SessionState::Unknown);
                if prev == snap.state {
                    continue;
                }
                // State changed since last tick — record the update so
                // the second pass can mutate `last_session_state`
                // (can't write to it inside this loop without giving up
                // the immutable `&self.groups` borrow).
                state_updates.push((sid.to_string(), snap.state));

                // OS-level "turn complete" toast: fires on
                // `(Busy | PendingToolUse) → Idle` regardless of which
                // tab is currently focused. The gate is "user is not
                // looking at the app window" — there's no point
                // pinging the OS if the user is already staring at
                // CodeScope. Matches the C# `IdleNotifier` placement
                // *before* the SelectedTab suppression check.
                if !self.window_active_cached
                    && matches!(
                        (prev, snap.state),
                        (
                            codescope_core::SessionState::Busy
                                | codescope_core::SessionState::PendingToolUse,
                            codescope_core::SessionState::Idle
                        )
                    )
                {
                    let detail = match snap.last_turn_duration {
                        Some(d) => format!(
                            "Turn complete · {}",
                            codescope_core::ClaudeTranscriptTail::format_duration(d)
                        ),
                        None => "Turn complete.".to_string(),
                    };
                    pending_toasts.push((
                        sid.to_string(),
                        tab.title.to_string(),
                        detail,
                    ));
                }

                let is_focused = focused_tab_key
                    .map(|(g, t)| g == group.id && t == tab.id)
                    .unwrap_or(false);
                if is_focused {
                    continue;
                }
                if let Some((kind, title, detail)) =
                    classify_activity_transition(prev, &snap)
                {
                    pending.push(Pending {
                        kind,
                        title: title.into(),
                        detail: detail.into(),
                        session_title: tab.title.clone(),
                    });
                }
            }
        }

        // De-dupe + spawn the OS-toast show() calls on the background
        // executor. notify-rust's `show()` can block for tens of ms on
        // Windows (COM marshalling) and macOS (NSUserNotification
        // round-trip) — keep it off the gpui main loop. Errors are
        // swallowed inside `fire_os_notification` since the toast is
        // a best-effort surface.
        for (sid, title, detail) in pending_toasts {
            if !self.idle_notifier.should_fire(&sid) {
                continue;
            }
            cx.background_executor()
                .spawn(async move {
                    crate::idle_notifier::fire_os_notification(&title, &detail);
                })
                .detach();
        }

        for (sid, state) in state_updates {
            self.last_session_state.insert(sid, state);
        }
        // Reap entries whose tail no longer exists (session closed or
        // rotated). Keeps the map bounded and resets first-observation
        // semantics for a re-adopted session.
        self.last_session_state
            .retain(|sid, _| self.telemetry_tails.contains_key(sid));

        for p in pending {
            self.push_notification(p.kind, p.title, p.detail, Some(p.session_title), cx);
        }
    }

    /// Push a persistent notification entry.  Unlike toasts these
    /// accumulate in the ring buffer until the user clears them or the
    /// ring reaches its cap (50).  Returns the id of the new entry.
    ///
    /// The bell button wires this up for session events;
    /// callers can also call it directly for generic system events.
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

    /// Move a tab to a (possibly different) group at the given insert
    /// position. Triggered by `on_drop` on a group's strip section
    /// after the user drags a tab. The terminal entity is moved
    /// unchanged — no teardown / respawn — so the pty keeps running
    /// and any agent (claude, …) keeps its session.
    ///
    /// `target_index` is an insert slot in `0..=target.tabs.len()`
    /// (with `usize::MAX` accepted as "append"). For same-group
    /// reorders the index is interpreted against the *post-removal*
    /// vec, matching the C# `ObservableCollection.Move` semantics.
    ///
    /// Looking up by id (not index) keeps us robust to concurrent
    /// list mutations between drag-start and drop. No-op when source /
    /// target group can't be resolved, or when the tab id isn't found
    /// in the source group.
    fn move_tab_to_group(
        &mut self,
        source_group_id: u64,
        source_tab_id: u64,
        target_group_id: u64,
        target_index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
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

        // Same-group reorder path. Adjust the insert index for the
        // removal of `tab_pos`: if the cursor is past the dragged
        // tab's slot, dropping at index N means inserting at N-1 in
        // the post-removal vec. Mirrors C#
        // `ObservableCollection.Move(currentIdx, targetIdx)`.
        if source_group_id == target_group_id {
            let len = self.groups[source_idx].tabs.len();
            let raw_target = target_index.min(len);
            // Clamp the insert slot to `[0, len-1]` for a same-group
            // move: removing the source first means there's one
            // fewer slot than the strip's gap count.
            let mut insert_at = if raw_target > tab_pos {
                raw_target - 1
            } else {
                raw_target
            };
            if insert_at >= len {
                insert_at = len - 1;
            }
            if insert_at == tab_pos {
                return;
            }
            let tab = self.groups[source_idx].tabs.remove(tab_pos);
            self.groups[source_idx].tabs.insert(insert_at, tab);
            self.groups[source_idx].active_tab = insert_at;
            self.activate_tab(source_idx, insert_at, window, cx);
            self.save_layout();
            return;
        }

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
        let target_tabs_len = self.groups[target_idx].tabs.len();
        let insert_at = target_index.min(target_tabs_len);
        self.groups[target_idx].tabs.insert(insert_at, tab);
        self.groups[target_idx].active_tab = insert_at;

        // Collapse the source group if it's empty and we have
        // siblings. After this the target_idx may shift; re-resolve.
        if source_now_empty && self.groups.len() > 1 {
            self.groups.remove(source_idx);
            if source_idx < self.group_weights.len() {
                self.group_weights.remove(source_idx);
            }
            // Same rescale as `close_tab` / `close_focused_group`:
            // taffy's "flex factor < 1" branch turns a sub-1.0
            // weight sum into a partially-empty workspace, so renormalise
            // to keep the sum at `len()` after the removal.
            normalise_group_weights(&mut self.group_weights);
        }

        // Activate the moved tab in its new home so keyboard focus
        // follows the user's intent.
        let final_target_idx = self
            .groups
            .iter()
            .position(|g| g.id == target_group_id)
            .unwrap_or(0);
        let final_active = self.groups[final_target_idx]
            .tabs
            .iter()
            .position(|t| t.id == source_tab_id)
            .unwrap_or_else(|| self.groups[final_target_idx].tabs.len().saturating_sub(1));
        self.activate_tab(final_target_idx, final_active, window, cx);
        self.save_layout();
    }

    /// Resolve the in-flight tab drag's cursor X to a `(target_group,
    /// insert_slot)` and update [`Self::tab_drag_hover`] so the drop
    /// indicator and the eventual `on_drop` index stay in sync.
    ///
    /// Shared by the strip *and* the pane drop targets — both elements
    /// share the same column-relative X bounds within a group, so the
    /// same math works for either attachment point. Issue #239 widened
    /// the drop hit-zone from "just the tab strip" to "the entire
    /// workspace column"; the cursor still resolves to a slot on the
    /// strip's tab rects because that's where the indicator paints.
    fn update_tab_drop_hover_for_group(
        &mut self,
        target_group_id: u64,
        event: &gpui::DragMoveEvent<TabDragData>,
        cx: &mut Context<Self>,
    ) {
        let cursor_x: f32 = event.event.position.x.into();
        let bounds_left: f32 = event.bounds.origin.x.into();
        let rel_x = cursor_x - bounds_left;
        // Same fallback the indicator uses: this-frame rects first
        // (already populated on subsequent frames), previous-frame
        // copy second. Drag-move events can fire mid-frame before the
        // canvas prepaint callbacks have run.
        let source = self
            .tab_rects
            .get(&target_group_id)
            .filter(|v| !v.is_empty())
            .or_else(|| self.prev_tab_rects.get(&target_group_id));
        let rects: Vec<codescope_core::TabRect> = source
            .map(|v| {
                v.iter()
                    .map(|(_, b)| {
                        let left: f32 = b.origin.x.into();
                        let right: f32 = (b.origin.x + b.size.width).into();
                        codescope_core::TabRect {
                            left: left - bounds_left,
                            right: right - bounds_left,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let drop_index = codescope_core::compute_drop_index(rel_x, &rects);
        let next = Some(TabDropHover {
            group_id: target_group_id,
            drop_index,
        });
        if self.tab_drag_hover != next {
            self.tab_drag_hover = next;
            cx.notify();
        }
    }

    /// Complete a tab drop on `target_group_id`, using the most
    /// recently resolved [`TabDropHover`] for that group as the insert
    /// slot (or appending when no hover ever fired — e.g. a drop with
    /// no preceding `on_drag_move` because the cursor was over a
    /// child element that swallowed the event).
    fn handle_tab_drop_on_group(
        &mut self,
        target_group_id: u64,
        payload: &TabDragData,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let drop_index = self
            .tab_drag_hover
            .filter(|h| h.group_id == target_group_id)
            .map(|h| h.drop_index)
            .unwrap_or(usize::MAX);
        self.tab_drag_hover = None;
        self.move_tab_to_group(
            payload.source_group_id,
            payload.source_tab_id,
            target_group_id,
            drop_index,
            window,
            cx,
        );
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
            // Focused an empty group — no active tab, so clear the
            // sidebar's active-context wash (#248).
            self.push_sidebar_active_context(cx);
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
        // ─── App-level chord set (Ctrl+Shift universal) ─────────
        //
        // Every app chord here requires Ctrl+Shift. Reason: coding
        // agents inside the terminal lean heavily on plain Ctrl+letter
        // (Ctrl+W = backward-kill-word, Ctrl+P = previous history,
        // Ctrl+T = transpose, Ctrl+B = backward-char, Ctrl+, often
        // mapped by tooling, …). Plain Ctrl+letter is left alone so
        // it falls through to the terminal; Ctrl+Shift+letter is
        // unambiguous "app, not terminal".
        //
        // Chord table — kept in sync with
        // `codescope_terminal::view::is_app_level_shortcut`:
        //
        //   Ctrl+Shift+T            new tab
        //   Ctrl+Shift+W            close tab / collapse empty group
        //   Ctrl+Tab                next tab     (shift already encodes "prev")
        //   Ctrl+Shift+Tab          prev tab
        //   Ctrl+Shift+1..9         focus tab N
        //   Ctrl+Shift+B            toggle sidebar
        //   Ctrl+Shift+,            settings dialog
        //   Ctrl+Shift+P            command palette (toggle)
        //   Ctrl+Shift+O            overview pane (toggle)
        //   Ctrl+Shift+D            diff viewer (toggle)
        //   Ctrl+Shift+\            split right
        //   Ctrl+Shift+G            open active tab's remote in browser
        //   Ctrl+Shift+R            open active tab's PR in browser
        //   Alt+Left / Alt+Right    cycle focus group
        //   Alt+1..9                focus group N
        //
        // The legacy C# WPF build binds the plain Ctrl variants
        // because its hosted terminal swallows everything, but the
        // Rust port routes terminal-bound keys back to the PTY, so
        // the agent-shell conflicts surfaced. The Ctrl+Shift remap
        // is a Rust-port-only adjustment documented in
        // `docs/HANDOFF.md` (Keyboard chords).
        match key {
            // Ctrl+Shift+P — command palette toggle.
            "p" if mods.shift => {
                cx.stop_propagation();
                if self.command_palette.is_some() {
                    self.close_command_palette(cx);
                } else {
                    self.open_command_palette(window, cx);
                }
            }
            // Ctrl+Shift+, opens the Settings dialog. Accepts both
            // keystroke shapes gpui surfaces: bare `","` with
            // `mods.shift` set (non-US layouts, most non-Windows
            // platforms) and the US-Windows folded glyph `"<"` with
            // shift cleared (the Windows adapter folds shifted
            // punctuation the same way it folds shifted digits to
            // `!@#$%^&*(` — see `keystroke_digit_index`). Without the
            // second arm the chord silently doesn't fire on the most
            // common Windows install.
            "," if mods.shift => {
                cx.stop_propagation();
                self.open_settings_dialog(window, cx);
            }
            "<" if !mods.shift => {
                cx.stop_propagation();
                self.open_settings_dialog(window, cx);
            }
            // Ctrl+Shift+T — new tab. Plain Ctrl+T stays with the
            // terminal (readline transpose-char).
            "t" if mods.shift => {
                cx.stop_propagation();
                self.spawn_tab(window, cx);
            }
            // Ctrl+Shift+W — close active tab (or collapse an empty
            // group). Plain Ctrl+W stays with the terminal —
            // readline binds it to backward-kill-word.
            "w" if mods.shift => {
                cx.stop_propagation();
                let g = self.focused_group;
                let group = self.focused_group();
                if group.tabs.is_empty() {
                    // Empty focused group — collapse so the user
                    // can undo an accidental split right. No-op
                    // when this is the only group.
                    if self.groups.len() > 1 {
                        self.close_focused_group(window, cx);
                    }
                } else {
                    let t = group.active_tab;
                    self.close_tab(g, t, window, cx);
                }
            }
            // Ctrl+Shift+\ — split the focused group to the right.
            // On US layouts gpui's Windows adapter folds Shift+\
            // into "|" with shift consumed; accept both shapes.
            "\\" if mods.shift => {
                cx.stop_propagation();
                self.split_right(window, cx);
            }
            "|" => {
                cx.stop_propagation();
                self.split_right(window, cx);
            }
            // Ctrl+Shift+B — toggle sidebar visibility.
            "b" if mods.shift => {
                cx.stop_propagation();
                self.toggle_sidebar(cx);
            }
            // Ctrl+Shift+O — toggle the Overview panel.
            "o" if mods.shift => {
                cx.stop_propagation();
                let next = !self.show_overview;
                self.set_show_overview(next, cx);
            }
            // Ctrl+Shift+D — toggle the diff viewer for the focused
            // tab's worktree. Plain Ctrl+D stays with the terminal
            // (EOF / readline delete-char — agents and shells rely
            // on it).
            "d" if mods.shift => {
                cx.stop_propagation();
                self.toggle_diff_viewer(cx);
            }
            // Ctrl+Shift+G — open the active tab's worktree origin
            // remote in the browser. Plain Ctrl+G is "abort" /
            // history-cancel in many shells, so we keep it on the
            // shifted form to stay out of the terminal.
            "g" if mods.shift => {
                cx.stop_propagation();
                self.open_active_tab_remote_in_browser(cx);
            }
            // Ctrl+Shift+R — open the active tab's cached PR URL in
            // the browser. Plain Ctrl+R is readline reverse-history-
            // search and is heavily used inside coding agents, so
            // this chord stays on Ctrl+Shift.
            "r" if mods.shift => {
                cx.stop_propagation();
                self.open_active_tab_pr_in_browser(cx);
            }
            // Ctrl+Tab / Ctrl+Shift+Tab — cycle tabs forwards /
            // backwards. Shift is intrinsic to the prev-tab chord
            // so these stay on plain Ctrl; no shell binds Ctrl+Tab.
            "tab" if !mods.shift => {
                cx.stop_propagation();
                self.next_tab(window, cx);
            }
            "tab" if mods.shift => {
                cx.stop_propagation();
                self.prev_tab(window, cx);
            }
            // Ctrl+Shift+1..9 — focus tab N. Plain Ctrl+1..9 stays
            // with the terminal. gpui's Windows adapter folds
            // Shift+digit into !@#$%^&*( and clears `mods.shift`;
            // `keystroke_digit_index` accepts both shapes.
            d if d.len() == 1 => {
                if let Some(idx) = keystroke_digit_index(d, mods.shift) {
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

/// Resolve a 1-based digit-tab keystroke into a 0-based tab index.
///
/// Accepts both shapes the gpui keyboard adapters surface for
/// Ctrl+Shift+digit:
///
/// - Bare digit `"1".."9"` with `shift_set == true` — non-US layouts
///   and most non-Windows platforms.
/// - US-layout shifted glyph `"!@#$%^&*("` with `shift_set == false`,
///   because gpui's Windows adapter folds the shifted character into
///   the key string and clears the shift modifier (see
///   `gpui::platform::windows::keyboard::get_keystroke_key`).
///
/// Returns `None` for "0" / ")" (Ctrl+Shift+0 is unbound, matching
/// the legacy C# build) and for any non-digit key.
fn keystroke_digit_index(key: &str, shift_set: bool) -> Option<usize> {
    if shift_set
        && let Some(c) = key.chars().next()
        && let Some(n) = c.to_digit(10)
        && (1..=9).contains(&n)
    {
        return Some((n as usize) - 1);
    }
    let n: u32 = match key {
        "!" => 1,
        "@" => 2,
        "#" => 3,
        "$" => 4,
        "%" => 5,
        "^" => 6,
        "&" => 7,
        "*" => 8,
        "(" => 9,
        _ => return None,
    };
    if shift_set {
        // The adapter cleared shift for the US shifted-glyph form.
        // If shift is still set, the keystroke is exotic (AltGr,
        // dead key) — refuse to bind to avoid surprises.
        return None;
    }
    Some((n as usize) - 1)
}

impl Focusable for AppShell {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for AppShell {
    fn render(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        // Snapshot the OS-level "is the user looking at us" flag so
        // the background telemetry poll task can read it without a
        // `&Window` borrow. Updated every frame — at the 250 ms
        // telemetry cadence, one frame of staleness is invisible.
        self.window_active_cached = window.is_window_active();
        // Surface any updater state-machine transitions before the
        // heavy render work — pushes toasts / bell entries when the
        // poller flips us into Available / Ready / Failed.
        self.surface_update_state(cx);
        let theme = self.theme.clone();
        // Tab-drag rect cache: swap the just-built map into
        // `prev_tab_rects` so the in-flight render can still answer
        // "where is tab N right now" while the new frame's canvas
        // prepaint callbacks repopulate `tab_rects` for the *next*
        // frame. Without the swap, the drop indicator and
        // on_drag_move resolver would read an empty cache during the
        // render pass that follows the clear (canvas callbacks fire
        // at prepaint time, after the render closure returns).
        std::mem::swap(&mut self.tab_rects, &mut self.prev_tab_rects);
        self.tab_rects.clear();
        // Drag-cancel safety net: when no drag is active any
        // lingering hover state (cursor left the strip, OS dropped
        // the drag elsewhere, …) would otherwise keep painting a
        // stale drop indicator. gpui doesn't surface an
        // `on_drag_end` hook on the drop target, so we re-check
        // `cx.has_active_drag()` each frame and clear once the drag
        // is no longer in flight.
        if self.tab_drag_hover.is_some() && !cx.has_active_drag() {
            self.tab_drag_hover = None;
        }
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
        let close_hover_bg = theme::danger();
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
            cx.listener(|this, _, window, cx| {
                append_window_diag(&this.paths, "maximize_clicked_pre", window);
                let diag_paths = this.paths.clone();
                window.defer(cx, move |window, _| {
                    crate::win32_titlebar::toggle_maximize(window);
                    append_window_diag(&diag_paths, "maximize_clicked_post", window);
                });
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
            // Flex filler so the version slug pushes to the right edge
            // of the sidebar column (mirrors `Grid.Column="3"` in
            // `MainWindow.xaml`'s brand grid).
            .child(div().flex_grow())
            .child(
                // Version slug — `Fig.Font.Mono` @ `FontSize="10"` in
                // `MainWindow.xaml`, right-aligned (`Grid.Column="3"`),
                // `Text.Faint` foreground.
                div()
                    .font(theme::font_mono())
                    .text_size(px(10.0))
                    .text_color(theme::text_faint())
                    .truncate()
                    .child(VERSION_DISPLAY),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    this.handle_titlebar_press(event, window, cx);
                }),
            );

        // While the Overview (or the diff viewer) is visible the
        // per-group tab strip is structurally meaningless — its only
        // target (the group grid below) is hidden. Render an empty
        // placeholder so the caption row keeps the same layout
        // footprint but doesn't surface stale tab affordances.
        let tab_strip_inline = if self.show_overview || self.diff_viewer.is_some() {
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

        // `projects_empty` here is independent of the
        // `render_status_bar` local of the same name — recomputed so
        // the work-area swap stays self-contained.
        let projects_empty = self
            .sidebar
            .read(cx)
            .projects()
            .projects
            .is_empty();

        // Work area swap: when the Overview panel is up, hide the
        // group strip + terminal grid entirely and render the
        // Overview in their place. The sidebar + status bar stay
        // anchored on either side / below so the user can dismiss
        // via the same sidebar button. Mirrors the C# build's
        // `IsOverviewVisible` DataTrigger swap in `MainWindow.xaml`.
        let work_area: gpui::AnyElement = if self.diff_viewer.is_some() {
            // Diff viewer wins the slot — `set_show_overview(true)`
            // closes it, so the two can't be up at the same time.
            self.render_diff_viewer(&theme, cx).into_any_element()
        } else if self.show_overview {
            self.render_overview(&theme, cx).into_any_element()
        } else if projects_empty {
            // First-run hero takes over the work area whenever no
            // projects are registered. Mirrors the C# `Sidebar.IsEmpty`
            // DataTrigger that collapses `WorkspaceLayer` in
            // `MainWindow.xaml` and shows `EmptyStateView` instead.
            // Overview still wins over empty-state above so the user
            // can dismiss the hero by flipping into Overview (then
            // back) — same precedence the C# build uses.
            crate::empty_state::render(&theme, cx)
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
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, window, cx| {
                if this.splitter_drag.is_some() {
                    this.update_splitter_drag(event.position.x, cx);
                }
                if this.sidebar_drag.is_some() {
                    this.update_sidebar_drag(event.position.x, cx);
                }
                if this.titlebar_press.is_some() {
                    this.update_titlebar_drag(event, window, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.end_splitter_drag(cx);
                    this.end_sidebar_drag(cx);
                    this.titlebar_press = None;
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
            .children(self.render_command_palette(window, &theme, cx))
            .children(self.render_settings_dialog(window, &theme, cx))
            .children(self.render_rename_dialog(window, &theme, cx))
            .children(self.render_confirm_dialog(window, &theme, cx))
    }
}

impl AppShell {
    /// Render the palette overlay when one is open. Returned as an
    /// `Option<AnyElement>` so the caller can `.children(...)` it
    /// directly into the root layout — same pattern as the toast /
    /// notifications popover renders.
    fn render_command_palette(
        &self,
        window: &mut Window,
        theme: &Arc<Theme>,
        cx: &mut Context<Self>,
    ) -> Option<gpui::AnyElement> {
        let state = self.command_palette.as_ref()?;
        Some(crate::command_palette::render_palette(
            state,
            window,
            theme,
            self.text_blink_phase,
            cx,
        ))
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

// ─── Command palette ────────────────────────────────────────────────
//
// Ctrl+Shift+P opens the palette modal. Action assembly +
// dispatch live here so we have direct access to every AppShell
// method we route to (spawn_tab_in, apply_settings, toggle_sidebar,
// sidebar updates). The state struct and the render function live in
// `crate::command_palette` — this block is just the glue.

impl AppShell {
    /// Accessor for the optional palette state — exposed so the
    /// key-handler in `crate::command_palette` can mutate the query /
    /// selection without us threading a `&mut Option` through every
    /// frame.
    pub(crate) fn command_palette_mut(
        &mut self,
    ) -> Option<&mut crate::command_palette::CommandPaletteState> {
        self.command_palette.as_mut()
    }

    /// Open the palette. Idempotent — re-pressing Ctrl+Shift+P while the
    /// palette is open closes it (the chord toggles, mirroring the
    /// sidebar / overview chords). Builds the action list from the
    /// current sidebar / settings snapshot the same way C#
    /// `BuildPaletteActions` does on every open.
    pub fn open_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_palette.is_some() {
            return;
        }
        let actions = self.build_palette_actions(cx);
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window);
        let state =
            crate::command_palette::CommandPaletteState::new(actions, focus_handle);
        self.command_palette = Some(state);
        cx.notify();
    }

    /// Close without dispatching. Drops the state; the focus handle is
    /// dropped with it so a follow-up key press routes to the AppShell
    /// root handler again.
    pub fn close_command_palette(&mut self, cx: &mut Context<Self>) {
        if self.command_palette.take().is_some() {
            cx.notify();
        }
    }

    /// Activate the row at index `row_idx` *within the filtered list*.
    /// Bound from the row's mouse-down so a single click runs the
    /// action even if the user hadn't navigated to that row first. We
    /// move the highlight and then immediately dispatch through the
    /// same path Enter takes so the click and keyboard arms remain
    /// behaviourally identical.
    pub fn activate_palette_row(
        &mut self,
        row_idx: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(state) = self.command_palette.as_mut()
            && row_idx < state.filtered.len()
        {
            state.selected = row_idx;
        }
        self.submit_command_palette(window, cx);
    }

    /// Dispatch the currently selected action and close the palette.
    /// Mirrors C# `CommandPaletteDialog.Commit`.
    pub fn submit_command_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let action = match self.command_palette.as_ref().and_then(|s| s.selected_action()) {
            Some(a) => a.clone(),
            None => {
                // Nothing to do — just close, same as Esc.
                self.close_command_palette(cx);
                return;
            }
        };
        // Close first so the action runs against a "clean" frame and
        // any toasts the action surfaces are visible underneath the
        // dropped overlay.
        self.close_command_palette(cx);
        self.dispatch_palette_action(action, window, cx);
    }

    /// Run a palette action. Each variant maps to an existing
    /// AppShell / Sidebar entry point — no new behaviour, the palette
    /// is a thin keyboard-driven front-end over the same methods the
    /// menus and shortcuts already use.
    fn dispatch_palette_action(
        &mut self,
        action: crate::command_palette::PaletteAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::command_palette::{BuiltInCommand, PaletteActionKind};
        match action.kind {
            PaletteActionKind::Project { sidebar_index, .. } => {
                self.sidebar.update(cx, |sidebar, cx| {
                    sidebar.select(sidebar_index, cx);
                });
            }
            PaletteActionKind::Worktree { working_directory, title, .. } => {
                // Focus-or-open semantics, matching the worktree-row
                // click default — `force_new: false` lets the existing
                // tab-match logic in `spawn_tab_in`'s callers do the
                // right thing. We go through the same path the
                // sidebar's OpenSession event hits.
                self.open_or_focus_session(working_directory, title, None, false, window, cx);
            }
            PaletteActionKind::Agent { id, command, display_name } => {
                // Start a fresh tab running the agent. `auto_type` is
                // the command name; the shell will resolve it via PATH
                // (claude / codex / opencode / etc — same UX as the
                // sidebar's "New Claude session" rows). `id` is the
                // profile id (already on the palette action variant)
                // and gets forwarded directly to the spawn so the
                // persisted `Session.agent_id` is the user's actual
                // pick even for custom profiles where `id != command`.
                let cwd = self.active_project_path(cx).map(std::path::PathBuf::from);
                let title: SharedString = display_name.clone().into();
                self.spawn_palette_agent_tab(
                    cwd,
                    title,
                    id.clone().into(),
                    command.into(),
                    window,
                    cx,
                );
            }
            PaletteActionKind::Theme { id, display_name } => {
                // Live-apply: rewrite settings.theme, persist, and
                // re-resolve through `apply_settings` so the chrome
                // and the sidebar repaint immediately.
                let mut next = (*self.settings).clone();
                next.theme = id.clone();
                if let Err(err) = next.save(&self.paths) {
                    eprintln!("warning: failed to persist theme change: {err:#}");
                }
                self.apply_settings(next, cx);
                self.push_toast(
                    ToastKind::Ok,
                    SharedString::from(format!("Theme: {display_name}")),
                    None,
                    cx,
                );
            }
            PaletteActionKind::Command(cmd) => match cmd {
                BuiltInCommand::ToggleOverview => {
                    // Toggle the overview pane. Mirrors the Ctrl+Shift+O
                    // chord (and the sidebar footer "Overview" button)
                    // — same `set_show_overview` entry point so the
                    // palette never has to duplicate the panel-flip
                    // bookkeeping.
                    let next = !self.show_overview;
                    self.set_show_overview(next, cx);
                }
                BuiltInCommand::ToggleDiffViewer => {
                    // Same entry point as the Ctrl+Shift+D chord and
                    // the worktree menu's "View changes" row.
                    self.toggle_diff_viewer(cx);
                }
                BuiltInCommand::ToggleSidebar => {
                    self.toggle_sidebar(cx);
                }
                BuiltInCommand::NewProject => {
                    self.open_new_project_dialog(window, cx);
                }
                BuiltInCommand::NewSession => {
                    self.spawn_tab(window, cx);
                }
                BuiltInCommand::OpenSettings => {
                    // Open the in-app Settings dialog — same entry
                    // point Ctrl+Shift+, takes (see `on_key_down`). The
                    // earlier behaviour shelled out to whatever app
                    // owns `.json` so the user could hand-edit
                    // `settings.json`; the Rust port has a proper
                    // dialog now (ADR-0018) and the palette should
                    // mirror the keyboard shortcut's destination.
                    self.open_settings_dialog(window, cx);
                }
                BuiltInCommand::ReloadTheme => {
                    // Re-resolve the theme from the current settings
                    // value. If the user has hand-edited
                    // `settings.json` to a different theme name, this
                    // picks the change up without a restart.
                    let settings_clone = (*self.settings).clone();
                    self.apply_settings(settings_clone, cx);
                    self.push_toast(
                        ToastKind::Ok,
                        SharedString::from("Theme reloaded"),
                        None,
                        cx,
                    );
                }
                BuiltInCommand::OpenRemoteInBrowser => {
                    self.open_active_tab_remote_in_browser(cx);
                }
                BuiltInCommand::OpenPullRequestInBrowser => {
                    self.open_active_tab_pr_in_browser(cx);
                }
            },
        }
    }

    /// Build the palette's action list from the current sidebar /
    /// settings snapshot. Captured at open time — re-runs only on a
    /// fresh open, not on every keystroke. Mirrors C#
    /// `MainViewModel.BuildPaletteActions`.
    fn build_palette_actions(
        &self,
        cx: &Context<Self>,
    ) -> Vec<crate::command_palette::PaletteAction> {
        use crate::command_palette::{
            BuiltInCommand, PaletteAction, PaletteActionKind, PaletteGroup,
        };
        let mut out: Vec<PaletteAction> = Vec::new();

        // Static built-in commands — always available, regardless of
        // sidebar state. We leave `subtitle = None`: the renderer
        // already shows `cmd.hint()` as the right-aligned chord text
        // for `PaletteActionKind::Command` rows, so duplicating it in
        // the subtitle would paint the same chord twice on the same
        // row. The chord still feeds the fuzzy scorer because the
        // renderer's hint path is independent of search input — the
        // search target is `PaletteAction::display`, and a user
        // typing the chord (e.g. "Ctrl+Shift+B") still finds Toggle sidebar
        // via the title fragment.
        for cmd in [
            BuiltInCommand::NewSession,
            BuiltInCommand::ToggleSidebar,
            BuiltInCommand::ToggleOverview,
            BuiltInCommand::ToggleDiffViewer,
            BuiltInCommand::NewProject,
            BuiltInCommand::OpenSettings,
            BuiltInCommand::ReloadTheme,
            BuiltInCommand::OpenRemoteInBrowser,
            BuiltInCommand::OpenPullRequestInBrowser,
        ] {
            out.push(PaletteAction {
                kind: PaletteActionKind::Command(cmd),
                title: cmd.title().into(),
                subtitle: None,
                group: PaletteGroup::Commands,
            });
        }

        // Themes — one row per built-in. Live-applied on activate.
        for theme in codescope_core::theme::builtin::all() {
            out.push(PaletteAction {
                kind: PaletteActionKind::Theme {
                    id: theme.name.clone(),
                    display_name: theme.display_name.clone(),
                },
                title: format!("Theme: {}", theme.display_name).into(),
                subtitle: Some(theme.name.clone().into()),
                group: PaletteGroup::Themes,
            });
        }

        // Agents — one row per registered profile. Spawns a new tab in
        // the active project's directory and auto-types the agent's
        // command.
        for agent in self.agent_registry.get_all() {
            out.push(PaletteAction {
                kind: PaletteActionKind::Agent {
                    id: agent.id.clone(),
                    display_name: agent.display_name.clone(),
                    command: agent.command.clone(),
                },
                title: format!("Agent: {}", agent.display_name).into(),
                subtitle: Some(agent.command.clone().into()),
                group: PaletteGroup::Agents,
            });
        }

        // Projects + their worktrees — one row each. Project row
        // selects in the sidebar; worktree row opens / focuses a
        // session.
        let sidebar = self.sidebar.read(cx);
        for (project_idx, project) in sidebar.projects().projects.iter().enumerate() {
            out.push(PaletteAction {
                kind: PaletteActionKind::Project {
                    sidebar_index: project_idx,
                    name: project.name.clone(),
                },
                title: format!("Project: {}", project.name).into(),
                subtitle: Some(project.path.clone().into()),
                group: PaletteGroup::Projects,
            });

            // `Project::worktrees` already includes the primary tree
            // as a `is_primary: true` row (seeded in `Project::new`),
            // so a single loop covers both the primary and any
            // non-primary worktrees the user has created.
            for wt in &project.worktrees {
                let branch = wt
                    .branch
                    .clone()
                    .unwrap_or_else(|| project.default_branch.clone());
                out.push(PaletteAction {
                    kind: PaletteActionKind::Worktree {
                        working_directory: std::path::PathBuf::from(&wt.path),
                        title: format!("{} · {}", project.name, branch).into(),
                        branch: branch.clone(),
                        project_name: project.name.clone(),
                    },
                    title: format!("Open: {} · {}", project.name, branch).into(),
                    subtitle: Some(wt.path.clone().into()),
                    group: PaletteGroup::Worktrees,
                });
            }
        }

        out
    }

    /// Open or focus a session for `working_directory`. Mirrors the
    /// sidebar's `OpenSession` event handler the AppShell already has
    /// wired up via `cx.subscribe_in` — we route through the same code
    /// path so palette dispatch and a worktree-row click produce
    /// identical results.
    fn open_or_focus_session(
        &mut self,
        working_directory: std::path::PathBuf,
        title: SharedString,
        auto_type: Option<SharedString>,
        force_new: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !force_new {
            // Walk every group's tabs for a working-directory match.
            // If we find one, activate it instead of spawning.
            for g_idx in 0..self.groups.len() {
                for t_idx in 0..self.groups[g_idx].tabs.len() {
                    if self.groups[g_idx].tabs[t_idx].working_directory.as_deref()
                        == Some(working_directory.as_path())
                    {
                        self.activate_tab(g_idx, t_idx, window, cx);
                        return;
                    }
                }
            }
        }
        // Palette / "open or focus" fresh spawn — resolve the
        // default agent here so the new row is persisted with the
        // right id (mirrors the OpenSession handler's Tier 3).
        let (effective_agent_id, effective_auto_type) = match auto_type.clone() {
            Some(at) => (None, Some(at)),
            None => match default_agent_launch_for(&self.settings) {
                Some((id, at)) => (Some(id), at),
                None => (None, None),
            },
        };
        self.spawn_tab_in(
            Some(working_directory),
            Some(title),
            effective_auto_type,
            effective_agent_id,
            None,
            window,
            cx,
        );
    }

    /// Spawn a new tab pinned to a working directory and auto-type a
    /// command into it — used by the palette's Agent action. Wrapped
    /// to keep the dispatch arm thin.
    ///
    /// `agent_id` is the profile id the palette dispatcher pulled
    /// straight off `PaletteActionKind::Agent.id`. Threading it
    /// through here (instead of re-deriving from `auto_type` via
    /// `agent_id_from_auto_type`) keeps custom profiles where
    /// `id != command` round-trippable — without this the spawn
    /// would persist `agent_id: None` for any user-defined agent
    /// whose command isn't one of the built-in names, and `reopen_
    /// session` would fall back to the default agent instead of
    /// resuming the row the user actually picked.
    fn spawn_palette_agent_tab(
        &mut self,
        working_directory: Option<std::path::PathBuf>,
        title: SharedString,
        agent_id: SharedString,
        auto_type: SharedString,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.spawn_tab_in(
            working_directory,
            Some(title),
            Some(auto_type),
            Some(agent_id),
            None,
            window,
            cx,
        );
    }

    /// Active project's working directory, if any. Used by the agent
    /// action to pick `cwd` for the spawn. Returns `None` when no
    /// project is selected — the spawn falls back to the default cwd
    /// (whatever `spawn_tab_in` resolves from settings).
    fn active_project_path(&self, cx: &Context<Self>) -> Option<String> {
        self.sidebar
            .read(cx)
            .active_project()
            .map(|p| p.path.clone())
    }

    /// Active tab's working directory as a `String`. Used by the
    /// "Open remote / PR in browser" surfaces (tab menu, palette,
    /// Ctrl+Shift+G / +R) to look the path up in the sidebar's
    /// `git_status` / `pr_urls` caches. Returns `None` when the
    /// active tab is a plain shell with no project context.
    fn active_tab_working_dir(&self) -> Option<String> {
        let g = self.groups.get(self.focused_group)?;
        let t = g.tabs.get(g.active_tab)?;
        t.working_directory
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
    }

    /// Open the active tab's worktree origin remote in the user's
    /// default browser. Mirrors the sidebar worktree menu's
    /// "Open remote in browser" row — and shares the same helper, so
    /// the URL resolution / shell-execute path is exactly one code
    /// path. No-ops when the active tab has no working directory the
    /// sidebar tracks as a worktree.
    fn open_active_tab_remote_in_browser(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.active_tab_working_dir() else {
            return;
        };
        self.sidebar.update(cx, |sidebar, cx| {
            if !sidebar.path_is_tracked_worktree(&path) {
                return;
            }
            sidebar.open_remote_in_browser_for_path(path, cx);
        });
    }

    /// Open the active tab's cached PR URL in the user's default
    /// browser. Same gating as the sidebar worktree menu's
    /// "Open PR in browser" row — a missing / stale-branch / no-open-PR
    /// cache entry no-ops.
    fn open_active_tab_pr_in_browser(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.active_tab_working_dir() else {
            return;
        };
        self.sidebar.update(cx, |sidebar, cx| {
            sidebar.open_pr_in_browser_for_path(path, cx);
        });
    }
}

impl AppShell {
    /// Build one group's tab strip section + body pane. Returned as a
    /// pair so `render` can interleave dividers between adjacent
    /// groups while keeping the strip and the pane below it in the
    /// same column.

    /// Handle a left-press on any of the title-bar drag regions
    /// (brand mark, strip-left padding, per-group trailing whitespace).
    ///
    /// A double-click (gpui's `click_count >= 2`, derived from the OS
    /// double-click time + spatial tolerance) toggles maximize/restore.
    ///
    /// A single press starts the window drag. The split below is forced
    /// by a gpui quirk: it does **not** dispatch mouse-*move* events
    /// while the cursor is over the title bar (it treats a
    /// `WindowControlArea` region as platform-owned), so a
    /// move-threshold can't reliably detect a drag there — only the
    /// press is dependable.
    ///
    /// - **Windowed:** start the OS drag immediately on the press.
    ///   `start_drag`'s modal move loop then tracks the cursor natively
    ///   in every direction, and a click with no drag is a harmless
    ///   no-op (nothing to restore).
    /// - **Maximized:** `start_drag` restores eagerly, so starting on
    ///   the press would restore on a bare click. Instead *arm* a drag
    ///   and let [`AppShell::update_titlebar_drag`] start it once the
    ///   cursor moves down into the content area — the natural
    ///   un-maximize gesture, and the one place moves actually fire.
    ///
    /// Non-Windows uses `start_window_move` (no eager restore), so it
    /// can start on the press in every case.
    #[allow(unused_variables)]
    fn handle_titlebar_press(
        &mut self,
        event: &gpui::MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Double-click detection. gpui's `click_count` honours the OS
        // double-click time + spatial tolerance and is authoritative
        // *except* on the Windows windowed path: starting the drag on the
        // press posts a synthetic `WM_NCLBUTTONDOWN(LPARAM(0))` that
        // corrupts gpui's `ClickState`, so the real second click reads as
        // count 1. Only there do we fall back to our own time+space
        // check. We deliberately do NOT apply it on non-Windows (no drag
        // echo) or maximized Windows (the press doesn't start a drag, so
        // `click_count` stays correct) — a hard-coded threshold there
        // would only risk false positives that diverge from the user's OS
        // double-click settings.
        let now = std::time::Instant::now();
        let prev = self.last_titlebar_down.replace((now, event.position));
        #[cfg(target_os = "windows")]
        let own_double = !window.is_maximized()
            && prev.is_some_and(|(t, p)| {
                now.duration_since(t) < std::time::Duration::from_millis(500)
                    && (event.position.x - p.x).abs() < px(6.0)
                    && (event.position.y - p.y).abs() < px(6.0)
            });
        #[cfg(not(target_os = "windows"))]
        let own_double = false;
        if event.click_count >= 2 || own_double {
            self.titlebar_press = None;
            self.last_titlebar_down = None; // don't let a third click re-toggle
            #[cfg(target_os = "windows")]
            window.defer(cx, |window, _| {
                crate::win32_titlebar::toggle_maximize(window);
            });
            #[cfg(not(target_os = "windows"))]
            window.zoom_window();
            return;
        }

        #[cfg(target_os = "windows")]
        if window.is_maximized() {
            self.titlebar_press = Some(event.position);
        } else {
            self.titlebar_press = None;
            crate::win32_titlebar::start_drag(window);
        }

        #[cfg(not(target_os = "windows"))]
        {
            self.titlebar_press = None;
            window.start_window_move();
        }
    }

    /// Window-level mouse-move hook that starts a *maximized* window's
    /// restore-and-drag once the cursor leaves the title bar. Only an
    /// armed press on a maximized window reaches here with
    /// `titlebar_press` set (a windowed press already started its drag in
    /// [`AppShell::handle_titlebar_press`]); the move is delivered once
    /// the cursor crosses into the content area, where — unlike over the
    /// title bar — gpui does dispatch mouse-moves. By that point the
    /// cursor has clearly left the caption, so any movement counts; the
    /// pressed-button guard keeps a stray armed press from turning a
    /// plain hover into a drag.
    #[allow(unused_variables)]
    fn update_titlebar_drag(
        &mut self,
        event: &gpui::MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        if self.titlebar_press.take().is_none() {
            return;
        }
        // `start_drag` is safe to call directly (not via `window.defer`):
        // its `ReleaseCapture()` can synchronously emit
        // `WM_CAPTURECHANGED`, but gpui doesn't handle that message, and
        // the modal move loop only begins once the posted
        // `WM_NCLBUTTONDOWN` is pumped after this listener returns.
        #[cfg(target_os = "windows")]
        crate::win32_titlebar::start_drag(window);
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
            // Drag payload — stable ids only. The preview view
            // captures its own copy of the title in the on_drag
            // closure, so the payload doesn't need to carry it.
            let drag_payload = TabDragData {
                source_group_id: group_id,
                source_tab_id: tab_id,
            };
            let title_for_drag = title.clone();
            let theme_for_preview = theme_for_drag.clone();
            // Bounds-capture canvas — stashes each tab's window-space
            // rect into `tab_rects[group_id]` every frame so
            // `on_drag_move` on the strip can resolve a cursor X to a
            // drop slot via `compute_drop_index`. Same pattern as
            // `bell_bounds`; the canvas is `absolute` + `size_full`
            // inside the tab so it doesn't perturb layout.
            let bounds_entity = cx.entity();
            let bounds_group_id = group_id;
            let bounds_tab_id = tab_id;
            let bounds_canvas = gpui::canvas(
                move |bounds, _window, cx| {
                    bounds_entity.update(cx, |this, _| {
                        let entry = this
                            .tab_rects
                            .entry(bounds_group_id)
                            .or_default();
                        entry.push((bounds_tab_id, bounds));
                    });
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full();
            div()
                .id(("tab", tab_id))
                .h_full()
                .relative()
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
                .child(bounds_canvas)
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
                    // `flex_none` so the 8 px status dot never gets
                    // squished horizontally when the tab title is long
                    // enough to push the flex container against the
                    // close button — without this the dot collapses
                    // to a flat sliver and the busy/idle colour stops
                    // reading at a glance.
                    div()
                        .flex_none()
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
        // Drop indicator — 3 px-wide accent bar at the resolved drop
        // slot for an in-flight tab drag over *this* strip. Mirrors
        // the design HTML's `.drop-indicator` (vertical bar with a
        // soft accent halo). Absolute-positioned inside the strip;
        // X is derived from the per-tab `tab_rects` cache so the
        // indicator snaps to the gap between tab midpoints exactly
        // the way `compute_drop_index` computes it.
        let drop_indicator: Option<gpui::AnyElement> = match self.tab_drag_hover {
            Some(hover) if hover.group_id == group_id => {
                // Prefer this frame's freshly-stored rects but fall
                // back to the previous frame's copy: render runs
                // before canvas prepaint, so on the *first* frame
                // after a layout change `tab_rects` is empty here
                // and only `prev_tab_rects` carries usable data.
                let rects = self
                    .tab_rects
                    .get(&group_id)
                    .filter(|v| !v.is_empty())
                    .or_else(|| self.prev_tab_rects.get(&group_id));
                let strip_origin_x: f32 = rects
                    .and_then(|v| v.first())
                    .map(|(_, b)| b.origin.x.into())
                    .unwrap_or(0.0);
                // 3 px-wide bar: subtract half its width so the bar
                // is centred on the boundary line regardless of
                // whether we're rendering the append slot, the
                // before-first slot, or a between-tabs slot.
                const BAR_HALF_W: f32 = 1.5;
                let indicator_x: f32 = if let Some(rs) = rects.filter(|v| !v.is_empty()) {
                    let idx = hover.drop_index.min(rs.len());
                    let boundary: f32 = if idx >= rs.len() {
                        let (_, b) = rs.last().unwrap();
                        (b.origin.x + b.size.width).into()
                    } else {
                        let (_, b) = &rs[idx];
                        b.origin.x.into()
                    };
                    boundary - strip_origin_x - BAR_HALF_W
                } else {
                    0.0
                };
                Some(
                    div()
                        .absolute()
                        .left(px(indicator_x.max(0.0)))
                        .top(px(4.0))
                        .bottom(px(4.0))
                        .w(px(3.0))
                        .bg(accent)
                        .rounded_sm()
                        .shadow(vec![gpui::BoxShadow {
                            color: gpui::hsla(accent.h, accent.s, accent.l, 0.6),
                            offset: gpui::point(px(0.0), px(0.0)),
                            blur_radius: px(10.0),
                            spread_radius: px(0.0),
                        }])
                        .into_any_element(),
                )
            }
            _ => None,
        };

        let mut strip = div()
            .id(("group-strip", group_id))
            .h_full()
            .relative()
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
            // Track the in-flight tab drag's cursor over this strip
            // so we can render the drop indicator at the right slot.
            // The pane below this strip shares the same X bounds and
            // wires the same handlers (issue #239: drop anywhere in
            // the group's column lands the tab in this group's bar).
            .on_drag_move(cx.listener(
                move |this, event: &gpui::DragMoveEvent<TabDragData>, _window, cx| {
                    this.update_tab_drop_hover_for_group(target_group_id, event, cx);
                },
            ))
            .on_drop(
                cx.listener(move |this, payload: &TabDragData, window, cx| {
                    this.handle_tab_drop_on_group(target_group_id, payload, window, cx);
                }),
            );
        if let Some(indicator) = drop_indicator {
            strip = strip.child(indicator);
        }
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
        // The pane mirrors the strip's tab drop handlers so the user
        // can release a dragged tab anywhere in the group's column,
        // not just the narrow tab strip (issue #239). The pane shares
        // the strip's X bounds (same flex cell), so the helper's
        // bounds-relative math resolves cursor X to the same slot it
        // would on the strip — the indicator still paints in the
        // strip while the cursor hovers below.
        let pane_target_group_id = group_id;
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
            .on_drag_move(cx.listener(
                move |this, event: &gpui::DragMoveEvent<TabDragData>, _window, cx| {
                    this.update_tab_drop_hover_for_group(pane_target_group_id, event, cx);
                },
            ))
            .on_drop(
                cx.listener(move |this, payload: &TabDragData, window, cx| {
                    this.handle_tab_drop_on_group(pane_target_group_id, payload, window, cx);
                }),
            )
            .child(body_inner);
        pane.style().flex_grow = Some(gmeta.weight);
        pane.style().flex_basis = Some(gpui::Length::Definite(px(0.0).into()));

        (strip, pane)
    }
}

// ─── Window-state extraction ────────────────────────────────────────

/// Process-global handle to `state_dir/window-diag.log`, kept open
/// across calls so each [`append_window_diag`] tick is a single
/// `WriteFile` syscall instead of `CreateFile` → `WriteFile` →
/// `CloseHandle`. Matters during interactive drag-resize where the
/// observer fires at frame rate: on Windows with antivirus filter
/// drivers a `CreateFile` round-trip is routinely 1-5 ms, enough to
/// introduce visible jank *and* alter the timing of the maximise-race
/// we're trying to catch — diagnostic I/O is supposed to observe the
/// system, not perturb it. (Copilot review on PR #222.)
///
/// `parking_lot::Mutex` because contention is non-existent (only the
/// UI thread writes) and the std `Mutex` poison-on-panic semantics
/// add noise to a best-effort logger. `Option<File>` because the
/// open may legitimately fail (state_dir not writable, disk full,
/// etc.) and we want to retry on the next tick rather than caching
/// the failure forever.
static WINDOW_DIAG_FILE: std::sync::OnceLock<parking_lot::Mutex<Option<std::fs::File>>> =
    std::sync::OnceLock::new();

/// Append one diagnostic line to `state_dir/window-diag.log`. Used to
/// chase an intermittent Windows bug where the maximised window ends
/// up positioned ~75 px below the monitor's work-area top (and runs
/// off the bottom of the screen). The bug is live-race-only — it
/// doesn't reproduce by loading a saved `window.json` — so the only
/// way to catch it is to keep a continuous tape of bounds-observer
/// ticks + caption-button clicks. The log line embeds a full Win32
/// snapshot from [`crate::win32_titlebar::diag_snapshot`] on Windows
/// so we can correlate `WindowBounds` (what gpui thinks) with the
/// actual HWND rect, `WINDOWPLACEMENT`, and monitor work-area.
///
/// Holds a process-global file handle ([`WINDOW_DIAG_FILE`]) to avoid
/// re-opening the file on every tick — see that static's doc for the
/// "diagnostic I/O must not perturb the race" rationale.
///
/// I/O errors are silently dropped — this is best-effort
/// instrumentation, not load-bearing state. Callers are the bounds
/// observer (fires on every resize tick) and the maximize
/// caption-button click handler.
fn append_window_diag(paths: &AppPaths, event: &str, window: &Window) {
    use std::io::Write as _;
    let bounds = window.window_bounds();
    let viewport = window.viewport_size();
    let scale = window.scale_factor();
    let is_max = window.is_maximized();
    let bounds_kind = match bounds {
        WindowBounds::Windowed(_) => "Windowed",
        WindowBounds::Maximized(_) => "Maximized",
        WindowBounds::Fullscreen(_) => "Fullscreen",
    };
    let (bx, by, bw, bh) = match bounds {
        WindowBounds::Windowed(b) | WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (
            f32::from(b.origin.x),
            f32::from(b.origin.y),
            f32::from(b.size.width),
            f32::from(b.size.height),
        ),
    };
    let vp_w = f32::from(viewport.width);
    let vp_h = f32::from(viewport.height);

    // Win32 fragment is emitted with the same key set on every
    // branch (success / open-call failure / non-Windows) so a grep
    // for `hwnd_rect=` or `rcWork=` matches every row, not just the
    // success path. The earlier `win32=unavailable` / `win32=n/a`
    // placeholders had asymmetric keys and broke field-level greps
    // — Copilot review on PR #222.
    const WIN32_MISSING: &str =
        "hwnd_rect=? zoomed=? showCmd=? rcNormal=? ptMaxPos=? flags=? rcMonitor=? rcWork=? dpi=?";
    #[cfg(target_os = "windows")]
    let win32 = crate::win32_titlebar::diag_snapshot(window)
        .unwrap_or_else(|| WIN32_MISSING.to_string());
    #[cfg(not(target_os = "windows"))]
    let win32 = {
        let _ = window;
        WIN32_MISSING.to_string()
    };

    let ts = now_iso8601();
    let line = format!(
        "{ts} event={event} bounds={bounds_kind}(({bx:.1},{by:.1},{bw:.1},{bh:.1})) \
         viewport=({vp_w:.1},{vp_h:.1}) scale={scale} is_maximized={is_max} {win32}\n"
    );

    let cell = WINDOW_DIAG_FILE.get_or_init(|| parking_lot::Mutex::new(None));
    let mut slot = cell.lock();
    if slot.is_none() {
        let path = paths.state_dir.join("window-diag.log");
        match std::fs::OpenOptions::new().create(true).append(true).open(&path) {
            Ok(f) => *slot = Some(f),
            Err(_) => return,
        }
    }
    if let Some(file) = slot.as_mut() {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Format a byte count as a compact human string (e.g. "12.4 MB").
fn fmt_bytes(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let b = bytes as f64;
    if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if b >= KB {
        format!("{:.0} KB", b / KB)
    } else {
        format!("{bytes} B")
    }
}

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

fn cursor_shape_from_setting(
    shape: codescope_core::CursorShape,
) -> codescope_terminal::CursorShape {
    use codescope_core::CursorShape as Setting;
    use codescope_terminal::CursorShape as Renderer;
    match shape {
        Setting::Block => Renderer::Block,
        Setting::Beam => Renderer::Beam,
        Setting::Underline => Renderer::Underline,
        Setting::HollowBlock => Renderer::HollowBlock,
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

/// Paired `(agent_id, auto_type)` resolver — the agent id half is
/// what gets persisted on the `Session` row at spawn time so a later
/// `reopen_session` can resolve the same agent profile back out of
/// the registry, instead of falling through to a plain shell because
/// the closed row carries `agent_id: None`. The auto_type half is the
/// command string the host auto-types into the freshly-spawned pty
/// to launch the agent inline (same value
/// `default_agent_auto_type_for` returns on its own — this helper is
/// the union of the two pieces of information that live on the same
/// `AgentProfile`).
///
/// Returns `None` only when the registry has no default agent at all
/// (empty list and `get_default()` is `None`). Returns
/// `Some((id, None))` when a default agent exists but its `command`
/// is empty / whitespace — preserves the id so reopen can name the
/// profile, but skips auto-typing to avoid feeding a leading-space
/// argv to the pty (same `Option` semantics
/// `build_new_session_auto_type` already documents).
fn default_agent_launch_for(
    settings: &Settings,
) -> Option<(SharedString, Option<SharedString>)> {
    let registry = codescope_core::AgentRegistry::from_settings(settings);
    let profile = registry.get_default()?;
    let id: SharedString = profile.id.clone().into();
    let auto_type = codescope_core::build_new_session_auto_type(profile).map(SharedString::from);
    Some((id, auto_type))
}

/// Pure transition classifier for the bell notification — extracted so
/// the small state machine can be unit-tested without an `AppShell`.
///
/// Returns `Some((kind, title, detail))` when the `prev → snap.state`
/// transition warrants a bell entry, `None` otherwise. The set of
/// firing transitions mirrors C#
/// `MainViewModel.PushActivityNotification`.
fn classify_activity_transition(
    prev: codescope_core::SessionState,
    snap: &codescope_core::TelemetrySnapshot,
) -> Option<(crate::notifications::NotificationKind, &'static str, String)> {
    use codescope_core::SessionState::*;
    if prev == snap.state {
        return None;
    }
    match (prev, snap.state) {
        (p, PendingToolUse) if p != PendingToolUse => Some((
            crate::notifications::NotificationKind::SessionWaiting,
            "Needs attention",
            "Agent paused on a tool prompt.".to_string(),
        )),
        (Busy | PendingToolUse, Idle) => {
            let detail = match snap.last_turn_duration {
                Some(d) => format!(
                    "Turn complete · {}",
                    codescope_core::ClaudeTranscriptTail::format_duration(d)
                ),
                None => "Turn complete.".to_string(),
            };
            Some((
                crate::notifications::NotificationKind::SessionReady,
                "Ready",
                detail,
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── normalise_group_weights ───────────────────────────────────
    //
    // Guards against the taffy "flex factor < 1" footgun: once a
    // splitter drag has pushed weights into the sub-unit range,
    // removing a column without rescaling leaves the workspace half
    // empty. Every removal path (`close_tab` / `close_focused_group` /
    // `move_tab_to_group`) feeds the surviving slice through this
    // helper, so its contract is load-bearing.

    #[test]
    fn normalise_group_weights_pins_lone_survivor_to_one() {
        // Heaviest drag the UI allows: weight `2.5` against a peer that
        // gets closed. The peer is dropped before this call, so the
        // survivor stays alone at `2.5`. Rescaling normalises the
        // single weight back to `1.0` (mean of one element) so the
        // single flex item gets the standard `flex-grow: 1` treatment
        // rather than taffy's truncated free-space distribution.
        // Epsilon-compare — `2.5 * (1.0 / 2.5)` isn't exactly `1.0` in
        // f32 (scale rounds).
        let mut weights = vec![2.5_f32];
        normalise_group_weights(&mut weights);
        assert_eq!(weights.len(), 1);
        assert!((weights[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn normalise_group_weights_pulls_lone_sub_one_survivor_up() {
        // Reproduces the bug report. Pre-fix the survivor stayed at
        // `0.5`, taffy fed it `0.5 * free_space`, and half the
        // workspace stayed blank.
        let mut weights = vec![0.5_f32];
        normalise_group_weights(&mut weights);
        assert_eq!(weights.len(), 1);
        assert!((weights[0] - 1.0).abs() < 1e-5);
    }

    #[test]
    fn normalise_group_weights_rescales_two_survivors_preserving_ratio() {
        // Weights `[0.4, 0.2]` (sum 0.6 < 1.0) — would trigger the
        // same truncation. After rescale the sum is 2.0 (slice length)
        // and the 2:1 ratio between the two survivors is intact.
        let mut weights = vec![0.4_f32, 0.2_f32];
        normalise_group_weights(&mut weights);
        let sum: f32 = weights.iter().copied().sum();
        assert!((sum - 2.0).abs() < 1e-5, "sum after rescale: {sum}");
        // 2:1 ratio preserved.
        assert!((weights[0] / weights[1] - 2.0).abs() < 1e-5);
    }

    #[test]
    fn normalise_group_weights_falls_back_when_sum_is_zero_or_invalid() {
        // Pathological inputs (corrupt layout.json, NaN, all-zero
        // sanitisation gap) round-trip to all-ones so the layout still
        // has a sane sum to feed the flex layer.
        let mut zeros = vec![0.0_f32, 0.0_f32];
        normalise_group_weights(&mut zeros);
        assert_eq!(zeros, vec![1.0_f32, 1.0_f32]);

        let mut nans = vec![f32::NAN, 1.0];
        normalise_group_weights(&mut nans);
        assert_eq!(nans, vec![1.0_f32, 1.0_f32]);
    }

    #[test]
    fn normalise_group_weights_no_op_on_empty_slice() {
        let mut weights: Vec<f32> = Vec::new();
        normalise_group_weights(&mut weights);
        assert!(weights.is_empty());
    }

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

    // ─── keystroke_digit_index ─────────────────────────────────────
    //
    // Covers both shapes the gpui keyboard adapters produce for
    // Ctrl+Shift+digit: bare-digit + shift, and the US-layout
    // shifted-glyph form with shift consumed by the adapter.

    #[test]
    fn keystroke_digit_bare_digit_with_shift_resolves() {
        for d in '1'..='9' {
            let key = d.to_string();
            let idx = keystroke_digit_index(&key, true);
            assert_eq!(
                idx,
                Some((d.to_digit(10).unwrap() as usize) - 1),
                "digit {d}",
            );
        }
    }

    #[test]
    fn keystroke_digit_shifted_glyph_resolves_on_us_layout() {
        // gpui's Windows adapter folds Shift+1..9 into !@#$%^&*( and
        // clears `mods.shift`.
        let cases = [
            ("!", 0),
            ("@", 1),
            ("#", 2),
            ("$", 3),
            ("%", 4),
            ("^", 5),
            ("&", 6),
            ("*", 7),
            ("(", 8),
        ];
        for (glyph, expected) in cases {
            assert_eq!(
                keystroke_digit_index(glyph, false),
                Some(expected),
                "glyph {glyph}",
            );
        }
    }

    #[test]
    fn keystroke_digit_zero_is_unbound() {
        // Ctrl+Shift+0 / Ctrl+) intentionally unbound — mirrors the
        // legacy C# build's "no Ctrl+0" convention.
        assert_eq!(keystroke_digit_index("0", true), None);
        assert_eq!(keystroke_digit_index(")", false), None);
    }

    #[test]
    fn keystroke_digit_plain_digit_without_shift_is_unbound() {
        // Plain Ctrl+1..9 must not match — the universal Ctrl+Shift
        // remap explicitly leaves them to the terminal.
        for d in '1'..='9' {
            let key = d.to_string();
            assert_eq!(keystroke_digit_index(&key, false), None, "digit {d}");
        }
    }

    #[test]
    fn keystroke_digit_non_digit_keys_return_none() {
        assert_eq!(keystroke_digit_index("a", true), None);
        assert_eq!(keystroke_digit_index("a", false), None);
        assert_eq!(keystroke_digit_index("", true), None);
    }

    // ─── default_agent_launch_for ──────────────────────────────────
    //
    // Ctrl+Shift+T / the "+ new tab" button + the worktree
    // double-click handler all route through this helper to look up
    // the user's preferred agent CLI from the registry. The tests
    // below pin the contract `on_key_down` for Ctrl+Shift+T relies
    // on: a vanilla Settings yields the Claude profile; flipping
    // `default_agent` picks the matching one; an empty `agents`
    // override falls back to built-ins. The auto_type half of the
    // returned pair is what gets typed into the freshly-spawned pty
    // — the id half is what gets persisted on the `Session` row so
    // `reopen_session` can resolve the same agent profile later.

    #[test]
    fn default_agent_launch_for_default_settings_returns_claude() {
        let settings = Settings::default();
        let (id, cmd) = default_agent_launch_for(&settings).expect("default present");
        assert_eq!(id.as_ref(), "claude");
        assert_eq!(cmd.as_deref().map(|s| s.as_ref()), Some("claude"));
    }

    #[test]
    fn default_agent_launch_for_honours_default_agent_setting() {
        // User changed `settings.default_agent` to Codex — the helper
        // must follow, otherwise Ctrl+Shift+T would silently keep
        // spawning Claude after the user reconfigured their default.
        let settings = Settings {
            default_agent: "codex".into(),
            ..Settings::default()
        };
        let (id, cmd) = default_agent_launch_for(&settings).expect("default present");
        assert_eq!(id.as_ref(), "codex");
        let cmd = cmd.expect("codex profile has a command");
        assert!(
            cmd.as_ref() == "codex" || cmd.as_ref().starts_with("codex "),
            "expected codex command, got {cmd:?}",
        );
    }

    #[test]
    fn default_agent_launch_for_joins_new_session_args() {
        // A user-defined profile with new-session args should serialise
        // as `<command> <arg1> <arg2>...` so the terminal gets a single
        // ready-to-run line.
        let settings = Settings {
            agents: vec![codescope_core::AgentProfile {
                id: "custom".into(),
                display_name: "Custom".into(),
                command: "my-cli".into(),
                resume_args: vec![],
                new_session_args: vec!["--init".into(), "fresh".into()],
                session_id_flag: None,
                resume_by_id_args: vec![],
                is_default: true,
                icon: None,
                context_window_tokens: 0,
            }],
            ..Settings::default()
        };
        let (id, cmd) = default_agent_launch_for(&settings).expect("default present");
        assert_eq!(id.as_ref(), "custom");
        assert_eq!(cmd.as_deref().map(|s| s.as_ref()), Some("my-cli --init fresh"));
    }

    #[test]
    fn default_agent_launch_for_empty_agents_falls_back_to_built_ins() {
        // Empty `agents` overrides → `from_settings` re-seeds the
        // built-in agent set, so the helper still returns `Some`
        // (Claude). This pins the contract Ctrl+Shift+T relies on: a
        // fresh / empty profile list never strands the user without
        // an agent.
        let settings = Settings {
            agents: vec![],
            ..Settings::default()
        };
        assert!(default_agent_launch_for(&settings).is_some());
    }

    // ─── classify_activity_transition ──────────────────────────────
    //
    // Locks the C# `PushActivityNotification` state machine: which
    // transitions fire which `NotificationKind`, with the correct
    // detail string (including turn-duration formatting).

    fn snap_with_state(state: codescope_core::SessionState) -> codescope_core::TelemetrySnapshot {
        codescope_core::TelemetrySnapshot {
            model: None,
            tokens_used: 0,
            context_pct: None,
            turn_count: 0,
            last_turn_duration: None,
            state,
        }
    }

    #[test]
    fn classify_activity_transition_returns_none_when_state_unchanged() {
        let snap = snap_with_state(codescope_core::SessionState::Busy);
        assert!(classify_activity_transition(codescope_core::SessionState::Busy, &snap).is_none());
    }

    #[test]
    fn classify_activity_transition_fires_waiting_on_entry_to_pending_tool_use() {
        let snap = snap_with_state(codescope_core::SessionState::PendingToolUse);
        for prev in [
            codescope_core::SessionState::Unknown,
            codescope_core::SessionState::Idle,
            codescope_core::SessionState::Busy,
        ] {
            let out = classify_activity_transition(prev, &snap);
            let (kind, title, detail) = out.expect("should fire SessionWaiting");
            assert_eq!(kind, crate::notifications::NotificationKind::SessionWaiting);
            assert_eq!(title, "Needs attention");
            assert_eq!(detail, "Agent paused on a tool prompt.");
        }
    }

    #[test]
    fn classify_activity_transition_does_not_re_fire_pending_to_pending() {
        let snap = snap_with_state(codescope_core::SessionState::PendingToolUse);
        // Same state ⇒ None (state-unchanged guard covers it).
        assert!(
            classify_activity_transition(codescope_core::SessionState::PendingToolUse, &snap)
                .is_none()
        );
    }

    #[test]
    fn classify_activity_transition_fires_ready_on_busy_to_idle() {
        let snap = snap_with_state(codescope_core::SessionState::Idle);
        let (kind, title, detail) =
            classify_activity_transition(codescope_core::SessionState::Busy, &snap)
                .expect("should fire SessionReady");
        assert_eq!(kind, crate::notifications::NotificationKind::SessionReady);
        assert_eq!(title, "Ready");
        assert_eq!(detail, "Turn complete.");
    }

    #[test]
    fn classify_activity_transition_fires_ready_on_pending_to_idle() {
        let snap = snap_with_state(codescope_core::SessionState::Idle);
        let out = classify_activity_transition(
            codescope_core::SessionState::PendingToolUse,
            &snap,
        );
        let (kind, _, _) = out.expect("should fire SessionReady");
        assert_eq!(kind, crate::notifications::NotificationKind::SessionReady);
    }

    #[test]
    fn classify_activity_transition_appends_turn_duration_when_available() {
        let mut snap = snap_with_state(codescope_core::SessionState::Idle);
        snap.last_turn_duration = Some(std::time::Duration::from_secs(75));
        let (_, _, detail) =
            classify_activity_transition(codescope_core::SessionState::Busy, &snap).unwrap();
        // 75 s → "1m 15s" per `ClaudeTranscriptTail::format_duration`.
        assert_eq!(detail, "Turn complete · 1m 15s");
    }

    #[test]
    fn classify_activity_transition_unknown_to_idle_or_busy_is_silent() {
        // First observation in Idle / Busy must NOT fire — only
        // PendingToolUse on first observation is loud (matches C#).
        for st in [
            codescope_core::SessionState::Idle,
            codescope_core::SessionState::Busy,
        ] {
            let snap = snap_with_state(st);
            assert!(
                classify_activity_transition(codescope_core::SessionState::Unknown, &snap)
                    .is_none(),
                "Unknown → {:?} should be silent",
                st
            );
        }
    }

    #[test]
    fn classify_activity_transition_idle_to_busy_is_silent() {
        let snap = snap_with_state(codescope_core::SessionState::Busy);
        assert!(
            classify_activity_transition(codescope_core::SessionState::Idle, &snap).is_none()
        );
    }
}
