//! Multiplatform taskbar / dock badge — mirrors C#
//! `TaskbarBadgeService.cs` (`legacy:CodeScope.Ui/Services/TaskbarBadgeService.cs`).
//!
//! Visual contract (must match the C# `Apply` method):
//!
//! - `agent_tab_count == 0` → no overlay (cleared).
//! - `busy_count == 0 && agent_tab_count > 0` → green dot
//!   (`Signal.Ok` = `#FF4BD87B`). No digit.
//! - `busy_count >= 1` → red disc (`Signal.Warn` = `#FFFF5A5A`)
//!   with the busy count rendered as a white digit. `busy_count > 9`
//!   collapses to `9+`.
//!
//! Platform support matrix:
//!
//! - **Windows** — `ITaskbarList3::SetOverlayIcon` with an in-process
//!   16×16 HICON. Full visual parity.
//! - **macOS** — `NSApp.dockTile.setBadgeLabel:` for the digit text.
//!   The OS draws a red pill behind it; we can't tint it green for
//!   idle so we render *no* label when idle (matches "no overlay"
//!   from the C# spec, just for a different reason).
//! - **Linux** — Unity LauncherEntry DBus API
//!   (`com.canonical.Unity.LauncherEntry`). Sets a numeric count;
//!   supported by KDE Plasma, GNOME with the Dash-to-Dock extension,
//!   Cinnamon, elementary OS, and others. Colour is not controllable,
//!   so idle (`busy_count == 0`) hides the badge entirely.
//!
//! Currently stubbed on macOS and Linux — the structure is in place
//! so a follow-up PR can drop in the real implementations without
//! touching the call sites. The Windows path is the must-have for
//! this PR.

use gpui::Window;

/// Compute the badge text shown over the red disc.
///
/// Pure helper, fully unit-testable — the platform-specific code
/// just consumes the returned `Option<String>`. `None` means "no
/// digit", which the Windows path renders as a flat green dot when
/// `agent_tab_count > 0`, or as a cleared overlay when not.
///
/// - `0` → `None` (idle: green dot, no text).
/// - `1..=9` → `Some("1")`..`Some("9")`.
/// - `>= 10` → `Some("9+")`.
pub fn format_badge_text(busy: u32) -> Option<String> {
    match busy {
        0 => None,
        1..=9 => Some(busy.to_string()),
        _ => Some("9+".to_string()),
    }
}

/// Snapshot of the last applied state — used to suppress redundant
/// `apply` calls so a 250 ms telemetry tick doesn't repaint the
/// taskbar overlay every cycle. The C# build relies on WPF's
/// `TaskbarItemInfo.Overlay` doing the equality check itself; on
/// Windows the `ITaskbarList3` interop doesn't, so we cache.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct BadgeState {
    busy: u32,
    agents: u32,
}

/// Public façade. Construct once on window creation, stash on
/// [`crate::app::AppShell`], call `apply` from the telemetry-poll
/// callback that already runs on every tab status change.
///
/// On Windows the underlying `ITaskbarList3` proxy is held behind an
/// `Rc<RefCell<...>>` so the deferred-init `cx.spawn` task can wire
/// it up *without* going through the AppShell entity (no nested
/// `this.update` borrow). See [`Self::init_handle`] +
/// [`TaskbarBadgeInit::run`] (scheduled from
/// `AppShell::schedule_taskbar_badge_init`).
pub struct TaskbarBadge {
    last: Option<BadgeState>,
    #[cfg(target_os = "windows")]
    inner: windows_impl::WindowsBadge,
}

/// Opaque "init this from a spawn body" handle.
///
/// On Windows it carries a shared `Rc` to the lazy COM slot; on
/// other platforms it's empty. Cloned out of [`TaskbarBadge::init_handle`]
/// and passed into a foreground-executor task that awaits a short
/// timer (escaping the AppShell entity's construction borrow) before
/// calling [`Self::run`]. **Must** be called from outside any
/// `cx.update(...)` / `cx.new(...)` closure on Windows.
#[derive(Clone)]
pub struct TaskbarBadgeInit {
    #[cfg(target_os = "windows")]
    inner: windows_impl::WindowsBadgeInit,
}

impl TaskbarBadgeInit {
    /// Perform the deferred platform-specific bring-up. On Windows
    /// this is the `CoInitializeEx` + `CoCreateInstance(TaskbarList)`
    /// pair that must not run while the AppShell `RefCell` is
    /// borrowed (the COM proxy bring-up pumps the Win32 message loop
    /// and re-enters the gpui window proc).
    pub fn run(self) {
        #[cfg(target_os = "windows")]
        {
            self.inner.run();
        }
    }
}

impl TaskbarBadge {
    /// Build a new badge attached to `window`.
    ///
    /// **Important:** this is a cheap constructor that only captures
    /// the HWND. The Windows COM init (`CoInitializeEx` +
    /// `CoCreateInstance(TaskbarList)`) is deferred — call
    /// [`Self::init_handle`] right after construction, schedule the
    /// returned handle on the foreground executor with a short timer,
    /// then invoke [`TaskbarBadgeInit::run`] from outside any entity
    /// `cx.update` borrow.
    ///
    /// This split exists because the shell-side COM bring-up pumps
    /// the Win32 message loop on the STA thread, which re-enters the
    /// gpui window proc. If we did that while the AppShell entity's
    /// `RefCell` was still borrowed (i.e. inside
    /// `cx.new(|cx| AppShell::new(...))`), the re-entry would trip
    /// `RefCell already borrowed` at
    /// `gpui::async_context::update_entity`. See the dev-handoff for
    /// the crash from session 38 PR #149.
    pub fn new(window: &Window) -> Self {
        Self {
            last: None,
            #[cfg(target_os = "windows")]
            inner: windows_impl::WindowsBadge::new(window),
        }
    }

    /// Get an init handle to run from a deferred `cx.spawn` body.
    /// See [`TaskbarBadgeInit`] for the constraints.
    pub fn init_handle(&self) -> TaskbarBadgeInit {
        TaskbarBadgeInit {
            #[cfg(target_os = "windows")]
            inner: self.inner.init_handle(),
        }
    }

    /// Apply a new state. No-op when the state is identical to the
    /// last *successfully* applied state, so polling-loop callers
    /// don't have to gate on their own change detection.
    ///
    /// Safe to call before [`TaskbarBadgeInit::run`] has completed —
    /// the Windows path silently returns "not applied" when the COM
    /// proxy slot is still empty, and `last` is **not** cached in
    /// that case so the next telemetry tick retries painting (per
    /// Copilot review on PR #153 — without that, the very first
    /// apply pre-init would cache `last` and the badge would never
    /// appear until the state changed again, e.g. a tab becoming
    /// busy).
    pub fn apply(&mut self, busy_count: u32, agent_tab_count: u32) {
        let next = BadgeState {
            busy: busy_count,
            agents: agent_tab_count,
        };
        if self.last == Some(next) {
            return;
        }

        let applied = {
            #[cfg(target_os = "windows")]
            {
                self.inner.apply(busy_count, agent_tab_count)
            }
            #[cfg(target_os = "macos")]
            {
                macos_impl::apply(busy_count, agent_tab_count)
            }
            #[cfg(target_os = "linux")]
            {
                linux_impl::apply(busy_count, agent_tab_count)
            }
            // Other platforms: stay quiet — no badge concept. Treat
            // as "applied" so we don't busy-poll forever.
            #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
            {
                let _ = (busy_count, agent_tab_count);
                true
            }
        };

        if applied {
            self.last = Some(next);
        }
        // else: leave `self.last` unchanged so the next call with
        // the same state still hits the platform `apply` and gets a
        // chance to paint once the deferred init has populated the
        // COM slot.
    }

    /// Force-clear the overlay. Called from `AppShell::drop` for a
    /// graceful teardown — best-effort: a hard abort / OS-level
    /// kill skips destructors, so we rely on Windows itself to
    /// release the overlay when the owning HWND is destroyed.
    pub fn clear(&mut self) {
        self.last = Some(BadgeState { busy: 0, agents: 0 });
        #[cfg(target_os = "windows")]
        {
            self.inner.clear();
        }
        #[cfg(target_os = "macos")]
        {
            macos_impl::clear();
        }
        #[cfg(target_os = "linux")]
        {
            linux_impl::clear();
        }
    }
}

// ─── Windows ───────────────────────────────────────────────────────
#[cfg(target_os = "windows")]
mod windows_impl {
    use gpui::Window;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateCompatibleDC, CreateDIBSection,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, HGDIOBJ,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows::Win32::UI::Shell::{ITaskbarList3, TaskbarList};
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, DestroyIcon, HICON, ICONINFO,
    };
    use windows::core::PCWSTR;

    /// `HICON(null)` clears the overlay — the documented "no icon"
    /// sentinel for `ITaskbarList3::SetOverlayIcon`.
    fn null_hicon() -> HICON {
        HICON(std::ptr::null_mut())
    }

    /// Convert a Rust string to a NUL-terminated UTF-16 buffer, used
    /// as the `pszDescription` argument to `SetOverlayIcon`. The
    /// buffer must outlive the call — caller holds it on the stack.
    fn to_wide_nul(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Shared, mutable slot holding the lazily-constructed
    /// `ITaskbarList3` COM proxy. Wrapped in `Rc<RefCell<...>>` so
    /// the deferred `cx.spawn` init body can write into it without
    /// having to go through a `this.update(...)` closure on the
    /// AppShell entity — that would re-enter the entity's RefCell
    /// and panic during construction. `ITaskbarList3` is `!Send`,
    /// matching `Rc`'s thread affinity.
    pub(super) type SharedTaskbar = std::rc::Rc<std::cell::RefCell<TaskbarSlot>>;

    pub(super) struct TaskbarSlot {
        pub(super) taskbar: Option<ITaskbarList3>,
        /// `true` once we've attempted `CoCreateInstance` — used to
        /// avoid retrying if the shell rejects us (very old Windows /
        /// WinPE / shell-less containers).
        pub(super) init_attempted: bool,
    }

    /// Owns the lazy COM slot and an HWND snapshot. The HWND can be
    /// `None` if `raw-window-handle` couldn't extract it; the slot
    /// can hold `None` if either the deferred init hasn't run yet or
    /// `CoCreateInstance` failed. `apply` silently no-ops in both
    /// cases.
    pub(super) struct WindowsBadge {
        hwnd: Option<HWND>,
        slot: SharedTaskbar,
    }

    /// Init-side handle. Holds a clone of the shared COM slot so the
    /// foreground-executor task spawned from `AppShell::new` can
    /// populate it after a short timer (outside the construction
    /// borrow). See [`super::TaskbarBadgeInit`].
    #[derive(Clone)]
    pub(super) struct WindowsBadgeInit {
        slot: SharedTaskbar,
    }

    impl WindowsBadgeInit {
        pub(super) fn run(self) {
            // Re-check inside the borrow so a second `run` (e.g. if
            // someone calls it twice) is cheap and idempotent.
            {
                let slot = self.slot.borrow();
                if slot.init_attempted {
                    return;
                }
            }
            // COM init is per-thread. The gpui main thread is where
            // we were constructed; any later `apply` runs on the same
            // thread because TaskbarBadge is not Send.
            // `COINIT_APARTMENTTHREADED` matches gpui's own
            // `OleInitialize` call (which initialises STA) and the
            // Windows shell's expectations for ITaskbarList3, so this
            // returns S_FALSE (already initialised in compatible
            // mode) — never RPC_E_CHANGED_MODE.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
            // `CoCreateInstance(TaskbarList)` synchronously pumps the
            // Win32 message loop while it brings up the shell proxy
            // (this is the call whose pump-during-construction tripped
            // the `RefCell already borrowed` panic in session 38).
            // Running it here, on a foreground-executor task body that
            // has awaited a timer, guarantees no gpui entity borrow is
            // currently live, so the re-entrant window-proc dispatch
            // gets its own fresh `borrow_mut`, executes, and returns.
            let taskbar: Option<ITaskbarList3> = unsafe {
                CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER).ok()
            };
            // Calling HrInit() is required per MSDN before any other
            // ITaskbarList3 method. Quietly ignore failure — older
            // Windows builds before Win7 don't expose this interface.
            if let Some(ref tb) = taskbar {
                unsafe {
                    let _ = tb.HrInit();
                }
            }
            let mut slot = self.slot.borrow_mut();
            slot.taskbar = taskbar;
            slot.init_attempted = true;
        }
    }

    impl WindowsBadge {
        pub(super) fn new(window: &Window) -> Self {
            // Capture the HWND eagerly — this is a pure
            // `raw-window-handle` read with no Windows-shell side
            // effects. The COM init is deferred to
            // `WindowsBadgeInit::run` so we don't pump the message
            // loop (and re-enter gpui's already-borrowed RefCell)
            // while the AppShell entity is still being constructed.
            let hwnd = extract_hwnd(window);
            let slot = std::rc::Rc::new(std::cell::RefCell::new(TaskbarSlot {
                taskbar: None,
                init_attempted: false,
            }));
            Self { hwnd, slot }
        }

        pub(super) fn init_handle(&self) -> WindowsBadgeInit {
            WindowsBadgeInit {
                slot: self.slot.clone(),
            }
        }

        /// Returns `true` if the call actually reached the shell —
        /// i.e. the HWND was extractable *and* the deferred COM init
        /// has completed. Caller uses this to decide whether to
        /// cache the requested state; returning `false` keeps the
        /// cache un-updated so the next telemetry tick retries
        /// painting once the COM slot is ready.
        pub(super) fn apply(&mut self, busy: u32, agents: u32) -> bool {
            // Do *not* lazy-init from here — `apply` is called from
            // inside a `this.update(cx, ...)` borrow on the telemetry
            // poll path, and `CoCreateInstance(TaskbarList)` pumps
            // the Win32 message loop while building the shell proxy.
            // The pump would re-enter the gpui window proc and trip
            // a nested `RefCell` borrow on the AppShell entity. If
            // the deferred init handle hasn't run yet (very first
            // telemetry tick before the foreground spawn body fires)
            // we silently no-op; the next tick will paint the badge.
            let Some(hwnd) = self.hwnd else {
                // HWND extraction failed at boot — this is permanent,
                // so report "applied" to short-circuit the retry
                // loop. The caller will just stop calling us once
                // state stabilises.
                return true;
            };
            let slot = self.slot.borrow();
            let Some(ref tb) = slot.taskbar else {
                // Deferred init still pending (or `CoCreateInstance`
                // failed). Report "not applied" so the cache doesn't
                // get poisoned with a state we never actually
                // painted.
                return false;
            };
            if agents == 0 {
                unsafe {
                    let _ = tb.SetOverlayIcon(hwnd, null_hicon(), PCWSTR::null());
                }
                return true;
            }
            let digit = super::format_badge_text(busy);
            let (r, g, b) = if busy == 0 {
                (0x4B, 0xD8, 0x7B) // Signal.Ok
            } else {
                (0xFF, 0x5A, 0x5A) // Signal.Warn
            };
            // Screen-reader / hover-tooltip description. Mirrors the
            // C# `TaskbarItemInfo.Description` strings — "All agents
            // idle" for the green dot, "<n> agents working" / "1
            // agent working" for the red disc.
            let description = if busy == 0 {
                "All agents idle".to_string()
            } else if busy == 1 {
                "1 agent working".to_string()
            } else {
                format!("{busy} agents working")
            };
            let desc_wide = to_wide_nul(&description);
            if let Some(icon) = build_badge_icon(r, g, b, digit.as_deref()) {
                unsafe {
                    let _ = tb.SetOverlayIcon(hwnd, icon, PCWSTR(desc_wide.as_ptr()));
                    // SetOverlayIcon copies the icon contents, so we
                    // can free our handle immediately. Skipping this
                    // would leak a kernel object per state change.
                    let _ = DestroyIcon(icon);
                }
            }
            true
        }

        pub(super) fn clear(&mut self) {
            let Some(hwnd) = self.hwnd else {
                return;
            };
            let slot = self.slot.borrow();
            let Some(ref tb) = slot.taskbar else {
                return;
            };
            unsafe {
                let _ = tb.SetOverlayIcon(hwnd, null_hicon(), PCWSTR::null());
            }
        }
    }

    fn extract_hwnd(window: &Window) -> Option<HWND> {
        let handle = HasWindowHandle::window_handle(window).ok()?;
        match handle.as_raw() {
            RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
            _ => None,
        }
    }

    /// Build a 16×16 HICON with a filled disc in the given colour
    /// and (optionally) a centred white digit drawn from a
    /// hand-rolled 3×5 pixel font. The mask bitmap is all-zero
    /// (fully opaque alpha is encoded in the 32-bit colour DIB).
    ///
    /// Returns `None` if any GDI call fails — caller no-ops.
    fn build_badge_icon(r: u8, g: u8, b: u8, digit: Option<&str>) -> Option<HICON> {
        const SIZE: i32 = 16;
        let mut pixels = [0u32; (SIZE * SIZE) as usize];
        draw_disc(&mut pixels, SIZE as usize, r, g, b);
        if let Some(text) = digit {
            draw_digit_text(&mut pixels, SIZE as usize, text);
        }
        unsafe { make_hicon(SIZE, &pixels) }
    }

    /// Paint a centred filled disc into a BGRA buffer (Windows DIB
    /// premultiplied BGRA). Adds a 1 px ~40% black contrast ring on
    /// the outermost pixel band — mirrors the C# build's
    /// `DrawEllipse(null, new Pen(ring, 1), …)` call, which keeps
    /// the badge legible on light taskbar themes (white / Aero /
    /// macOS-light). Without the ring a `#FF5A5A` red disc bleeds
    /// into the taskbar background on light Win11 themes.
    fn draw_disc(pixels: &mut [u32], size: usize, r: u8, g: u8, b: u8) {
        let cx = (size as f32 - 1.0) / 2.0;
        let cy = (size as f32 - 1.0) / 2.0;
        let radius = (size as f32 / 2.0) - 0.5;
        // Ring darkens the outermost ~1 px of the disc to ~40% black —
        // 0x66 = 102/255 ~= 40%, matching the WPF `Color.FromArgb(102,
        // 0, 0, 0)` in `TaskbarBadgeService.BuildOverlay`.
        let ring_alpha = 0x66u16;
        for y in 0..size {
            for x in 0..size {
                let dx = x as f32 - cx;
                let dy = y as f32 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let alpha = if dist <= radius - 1.0 {
                    255u8
                } else if dist <= radius {
                    // Single-sample edge AA: scale alpha by how far
                    // into the last pixel ring we are. Cheap but
                    // visibly nicer than a hard binary disc at 16 px.
                    let t = radius - dist;
                    (t.clamp(0.0, 1.0) * 255.0) as u8
                } else {
                    0
                };
                if alpha == 0 {
                    continue;
                }
                // Blend the fill colour over the ring tint for the
                // outer 1 px so the rim reads as a darker version of
                // the fill — same look as WPF's stroke-after-fill.
                let ring_strength = if dist >= radius - 1.0 && dist <= radius {
                    ((1.0 - (radius - dist).clamp(0.0, 1.0)) * ring_alpha as f32) as u16
                } else {
                    0
                };
                let blend = |c: u8| -> u8 {
                    // Linear blend: out = c * (1 - ring_strength/255)
                    let inv = 255u16 - ring_strength;
                    ((c as u16 * inv) / 255) as u8
                };
                let fr = blend(r);
                let fg = blend(g);
                let fb = blend(b);
                // Premultiply BGRA — `SetDIBits` for 32 bpp icons
                // expects alpha-premultiplied colour channels.
                let pr = ((fr as u16 * alpha as u16) / 255) as u8;
                let pg = ((fg as u16 * alpha as u16) / 255) as u8;
                let pb = ((fb as u16 * alpha as u16) / 255) as u8;
                pixels[y * size + x] =
                    (alpha as u32) << 24 | (pr as u32) << 16 | (pg as u32) << 8 | (pb as u32);
            }
        }
    }

    /// Stamp the digit text in white. Uses a tiny 3×5 pixel font for
    /// digits and renders a smaller superscript-style 3×3 plus sign
    /// when the input ends in `+` — mirrors the C# build's
    /// "em-size-10 digit + em-size-6 plus, offset up and right"
    /// layout from `TaskbarBadgeService.BuildOverlay`. Glyphs ship
    /// inline so we don't depend on system font lookup (which has
    /// historically broken when "Segoe UI Variable" wasn't
    /// installed). White-on-red is the only legible 16-px combo at
    /// taskbar scale anyway.
    fn draw_digit_text(pixels: &mut [u32], size: usize, text: &str) {
        const GLYPH_W: usize = 3;
        const GLYPH_H: usize = 5;
        const GLYPH_SPACING: usize = 1;
        // Detect the "9+" cap form: a single base digit followed by
        // a superscript plus that sits in the upper-right corner.
        let (base, has_plus) = if let Some(stripped) = text.strip_suffix('+') {
            (stripped, true)
        } else {
            (text, false)
        };

        // Centre the base digit alone, then stamp the small plus on
        // top of it. The C# build does the same — it shifts the
        // digit left by 1 px when there's a `+` so the pair reads
        // visually centred.
        let base_w = base.chars().count() * GLYPH_W
            + base.chars().count().saturating_sub(1) * GLYPH_SPACING;
        let mut base_x = size.saturating_sub(base_w) / 2;
        if has_plus && base_x > 0 {
            base_x -= 1;
        }
        let base_y = size.saturating_sub(GLYPH_H) / 2;

        for (i, ch) in base.chars().enumerate() {
            let bitmap = glyph_for(ch);
            let ox = base_x + i * (GLYPH_W + GLYPH_SPACING);
            for gy in 0..GLYPH_H {
                for gx in 0..GLYPH_W {
                    if (bitmap[gy] >> (GLYPH_W - 1 - gx)) & 1 == 1 {
                        let x = ox + gx;
                        let y = base_y + gy;
                        if x < size && y < size {
                            // Pure opaque white pixel.
                            pixels[y * size + x] = 0xFFFFFFFF;
                        }
                    }
                }
            }
        }

        if has_plus {
            // 3×3 "+" glyph, top-right-anchored, drawn as a
            // superscript so it visually distinguishes "9+" from
            // a plain "9".
            let plus: [u8; 3] = [0b010, 0b111, 0b010];
            let px = size.saturating_sub(4); // 1 px right padding
            let py = 1;
            for gy in 0..3 {
                for gx in 0..3 {
                    if (plus[gy] >> (2 - gx)) & 1 == 1 {
                        let x = px + gx;
                        let y = py + gy;
                        if x < size && y < size {
                            pixels[y * size + x] = 0xFFFFFFFF;
                        }
                    }
                }
            }
        }
    }

    /// 3×5 pixel glyph table for the digits we care about plus `+`.
    /// Each row is a bitmask, MSB = leftmost pixel.
    fn glyph_for(ch: char) -> [u8; 5] {
        // 0b_xxx_ — 3-bit rows.
        match ch {
            '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
            '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
            '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
            '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
            '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
            '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
            '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
            '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
            '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
            '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
            '+' => [0b000, 0b010, 0b111, 0b010, 0b000],
            _ => [0; 5],
        }
    }

    /// Construct an HICON from a 32-bpp BGRA pixel buffer. Returns
    /// `None` if any GDI call fails.
    ///
    /// # Safety
    /// `pixels` must be exactly `size * size` u32 entries; the
    /// caller guarantees this by passing a stack array sized at the
    /// compile-time `SIZE` constant.
    unsafe fn make_hicon(size: i32, pixels: &[u32]) -> Option<HICON> {
        // Rust 2024 requires explicit `unsafe { }` even inside an
        // `unsafe fn` — wrap the whole block.
        unsafe {
            let mut bi: BITMAPINFO = std::mem::zeroed();
            bi.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
            bi.bmiHeader.biWidth = size;
            // Negative height = top-down DIB. Without this the icon
            // renders upside-down because Windows defaults to bottom-up.
            bi.bmiHeader.biHeight = -size;
            bi.bmiHeader.biPlanes = 1;
            bi.bmiHeader.biBitCount = 32;
            bi.bmiHeader.biCompression = BI_RGB.0;

            let screen_dc = CreateCompatibleDC(None);
            if screen_dc.is_invalid() {
                return None;
            }
            let mut bits_ptr: *mut core::ffi::c_void = std::ptr::null_mut();
            let color_bitmap = CreateDIBSection(
                Some(screen_dc),
                &bi,
                DIB_RGB_COLORS,
                &mut bits_ptr,
                None,
                0,
            )
            .ok();
            let Some(color_bitmap) = color_bitmap else {
                let _ = DeleteDC(screen_dc);
                return None;
            };
            if bits_ptr.is_null() {
                let _ = DeleteObject(HGDIOBJ(color_bitmap.0));
                let _ = DeleteDC(screen_dc);
                return None;
            }
            // Copy our prepared BGRA buffer into the DIB.
            std::ptr::copy_nonoverlapping(
                pixels.as_ptr() as *const u8,
                bits_ptr as *mut u8,
                pixels.len() * 4,
            );
            let _ = DeleteDC(screen_dc);

            // Mask bitmap is monochrome and unused when the colour DIB
            // carries its own alpha — but `CreateIconIndirect` still
            // requires a non-null `hbmMask`. Pass an all-zero 16×16
            // monochrome bitmap (1 bit per pixel, 2 bytes per row).
            let mask_bits = [0u8; 32];
            let mask_bitmap =
                CreateBitmap(size, size, 1, 1, Some(mask_bits.as_ptr() as *const _));
            if mask_bitmap.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(color_bitmap.0));
                return None;
            }

            let icon_info = ICONINFO {
                fIcon: true.into(),
                xHotspot: 0,
                yHotspot: 0,
                hbmMask: mask_bitmap,
                hbmColor: color_bitmap,
            };
            let hicon = CreateIconIndirect(&icon_info).ok();
            // CreateIconIndirect copies the DDB contents; we own and
            // must free our two source bitmaps.
            let _ = DeleteObject(HGDIOBJ(color_bitmap.0));
            let _ = DeleteObject(HGDIOBJ(mask_bitmap.0));
            hicon
        }
    }
}

// ─── macOS ────────────────────────────────────────────────────────
//
// Stubbed for now. Future implementation: use `objc2` (would need to
// be added to `[target.'cfg(target_os = "macos")'.dependencies]`) to
// call `[[NSApplication sharedApplication] dockTile] setBadgeLabel:`
// with the digit string for busy state; `setBadgeLabel:nil` for
// cleared. The system pill is red regardless of count, so this
// matches the C# "red disc with digit" path for busy and the
// "cleared overlay" path for idle / no agents.
#[cfg(target_os = "macos")]
mod macos_impl {
    /// Returns `true` once an implementation lands; the current stub
    /// reports `true` so the de-dup cache in `TaskbarBadge::apply`
    /// doesn't spin retrying.
    pub(super) fn apply(_busy: u32, _agents: u32) -> bool {
        // TODO: NSApp.dockTile.setBadgeLabel:
        true
    }
    pub(super) fn clear() {
        // TODO: setBadgeLabel:nil
    }
}

// ─── Linux ────────────────────────────────────────────────────────
//
// Stubbed for now. Future implementation: emit
// `com.canonical.Unity.LauncherEntry.Update` signal on the session
// bus (via `zbus`) with `count` + `count-visible` properties keyed
// off the desktop-file path. Idle (`busy == 0`) hides the badge
// entirely because the Unity protocol cannot tint it green; only
// the busy digit is shown.
#[cfg(target_os = "linux")]
mod linux_impl {
    /// Returns `true` once an implementation lands; the current stub
    /// reports `true` so the de-dup cache in `TaskbarBadge::apply`
    /// doesn't spin retrying.
    pub(super) fn apply(_busy: u32, _agents: u32) -> bool {
        // TODO: Unity LauncherEntry DBus signal
        true
    }
    pub(super) fn clear() {
        // TODO: count-visible = false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_busy_renders_no_digit() {
        assert_eq!(format_badge_text(0), None);
    }

    #[test]
    fn single_digit_busy_renders_digit() {
        for n in 1..=9 {
            assert_eq!(format_badge_text(n), Some(n.to_string()));
        }
    }

    #[test]
    fn double_digit_busy_caps_at_nine_plus() {
        assert_eq!(format_badge_text(10), Some("9+".to_string()));
        assert_eq!(format_badge_text(42), Some("9+".to_string()));
        assert_eq!(format_badge_text(u32::MAX), Some("9+".to_string()));
    }
}
