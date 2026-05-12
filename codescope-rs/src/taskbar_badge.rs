//! Multiplatform taskbar / dock badge — mirrors C#
//! `TaskbarBadgeService.cs` (`src/CodeScope.Ui/Services/TaskbarBadgeService.cs`).
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

#![allow(dead_code)]

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
pub struct TaskbarBadge {
    last: Option<BadgeState>,
    #[cfg(target_os = "windows")]
    inner: windows_impl::WindowsBadge,
}

impl TaskbarBadge {
    /// Build a new badge attached to `window`. On Windows the
    /// underlying `ITaskbarList3` COM object is created lazily on
    /// first `apply` so a fast crash during startup doesn't leak a
    /// half-initialised taskbar entry.
    pub fn new(window: &Window) -> Self {
        Self {
            last: None,
            #[cfg(target_os = "windows")]
            inner: windows_impl::WindowsBadge::new(window),
        }
    }

    /// Apply a new state. No-op when the state is identical to the
    /// last applied state, so polling-loop callers don't have to
    /// gate on their own change detection.
    pub fn apply(&mut self, busy_count: u32, agent_tab_count: u32) {
        let next = BadgeState {
            busy: busy_count,
            agents: agent_tab_count,
        };
        if self.last == Some(next) {
            return;
        }
        self.last = Some(next);

        #[cfg(target_os = "windows")]
        {
            self.inner.apply(busy_count, agent_tab_count);
        }
        #[cfg(target_os = "macos")]
        {
            macos_impl::apply(busy_count, agent_tab_count);
        }
        #[cfg(target_os = "linux")]
        {
            linux_impl::apply(busy_count, agent_tab_count);
        }
        // Other platforms: stay quiet — no badge concept.
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        {
            let _ = (busy_count, agent_tab_count);
        }
    }

    /// Force-clear the overlay. Called on shell drop so a fresh
    /// launch doesn't inherit a stale taskbar dot when the previous
    /// process died without unwinding.
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

    /// Owns the COM-initialised `ITaskbarList3` pointer and an HWND
    /// snapshot. Both can be `None` if extraction failed at boot —
    /// caller silently no-ops in that case.
    pub(super) struct WindowsBadge {
        hwnd: Option<HWND>,
        taskbar: Option<ITaskbarList3>,
    }

    impl WindowsBadge {
        pub(super) fn new(window: &Window) -> Self {
            let hwnd = extract_hwnd(window);
            // COM init is per-thread. The gpui main thread is where we
            // were constructed; any later `apply` runs on the same
            // thread because TaskbarBadge is not Send. `COINIT_APARTMENT
            // THREADED` matches the Windows shell's expectations for
            // ITaskbarList3 and is a no-op (returns S_FALSE) if COM
            // was already initialised elsewhere on this thread.
            unsafe {
                let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            }
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
            Self { hwnd, taskbar }
        }

        pub(super) fn apply(&mut self, busy: u32, agents: u32) {
            let (Some(hwnd), Some(ref tb)) = (self.hwnd, self.taskbar.as_ref()) else {
                return;
            };
            if agents == 0 {
                unsafe {
                    let _ = tb.SetOverlayIcon(hwnd, null_hicon(), PCWSTR::null());
                }
                return;
            }
            let digit = super::format_badge_text(busy);
            let (r, g, b) = if busy == 0 {
                (0x4B, 0xD8, 0x7B) // Signal.Ok
            } else {
                (0xFF, 0x5A, 0x5A) // Signal.Warn
            };
            if let Some(icon) = build_badge_icon(r, g, b, digit.as_deref()) {
                unsafe {
                    let _ = tb.SetOverlayIcon(hwnd, icon, PCWSTR::null());
                    // SetOverlayIcon copies the icon contents, so we
                    // can free our handle immediately. Skipping this
                    // would leak a kernel object per state change.
                    let _ = DestroyIcon(icon);
                }
            }
        }

        pub(super) fn clear(&mut self) {
            let (Some(hwnd), Some(ref tb)) = (self.hwnd, self.taskbar.as_ref()) else {
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
    /// premultiplied BGRA — but since we use opaque alpha = 0xFF and
    /// full opacity colours, premultiplication is a no-op for the disc
    /// pixels). Edge pixels get a coarse alpha gradient so the disc
    /// doesn't look painfully aliased on the taskbar.
    fn draw_disc(pixels: &mut [u32], size: usize, r: u8, g: u8, b: u8) {
        let cx = (size as f32 - 1.0) / 2.0;
        let cy = (size as f32 - 1.0) / 2.0;
        let radius = (size as f32 / 2.0) - 0.5;
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
                // Premultiply BGRA — `SetDIBits` for 32 bpp icons
                // expects alpha-premultiplied colour channels.
                let pr = ((r as u16 * alpha as u16) / 255) as u8;
                let pg = ((g as u16 * alpha as u16) / 255) as u8;
                let pb = ((b as u16 * alpha as u16) / 255) as u8;
                pixels[y * size + x] =
                    (alpha as u32) << 24 | (pr as u32) << 16 | (pg as u32) << 8 | (pb as u32);
            }
        }
    }

    /// Stamp the digit text (e.g. "1".."9" or "9+") in white,
    /// centred in the 16×16 frame. Uses a tiny 3×5 pixel font;
    /// glyphs ship inline so we don't depend on any system font
    /// lookup (which historically has caused regressions when WPF
    /// "Segoe UI Variable" isn't installed). White-on-red is the
    /// only legible 16-px combo at taskbar scale anyway.
    fn draw_digit_text(pixels: &mut [u32], size: usize, text: &str) {
        // Glyph width = 3 px; 1 px gap between glyphs.
        let glyph_w = 3usize;
        let glyph_h = 5usize;
        let gap = 1usize;
        let total_w = text.chars().count() * glyph_w + text.chars().count().saturating_sub(1) * gap;
        let start_x = (size - total_w) / 2;
        let start_y = (size - glyph_h) / 2;

        for (i, ch) in text.chars().enumerate() {
            let bitmap = glyph_for(ch);
            let ox = start_x + i * (glyph_w + gap);
            for gy in 0..glyph_h {
                for gx in 0..glyph_w {
                    if (bitmap[gy] >> (glyph_w - 1 - gx)) & 1 == 1 {
                        let x = ox + gx;
                        let y = start_y + gy;
                        if x < size && y < size {
                            // Pure opaque white pixel.
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
    pub(super) fn apply(_busy: u32, _agents: u32) {
        // TODO: NSApp.dockTile.setBadgeLabel:
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
    pub(super) fn apply(_busy: u32, _agents: u32) {
        // TODO: Unity LauncherEntry DBus signal
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
