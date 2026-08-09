//! Win32-direct titlebar actions for the custom (`appears_transparent`)
//! caption row.
//!
//! gpui's `WindowControlArea` is supposed to wire this up via the
//! `on_hit_test_window_control` callback — Windows asks the callback
//! during `WM_NCHITTEST`, gets back `HTMAXBUTTON` / `HTCAPTION` /
//! etc., posts `WM_NCLBUTTONDOWN`, and gpui's NC-mouse-up handler
//! does the right native toggle. In practice that path doesn't fire
//! reliably for our windows (the user reported broken min/max/close
//! and broken drag), so we bypass it: read the HWND off the gpui
//! `Window` via `raw-window-handle` and send the proper Win32
//! messages from our own `on_mouse_down` listeners.
//!
//! This is the same approach Chrome, VS Code, and Windows Terminal
//! use for their custom title bars — `ReleaseCapture()` + send
//! `WM_NCLBUTTONDOWN` for drag, `WM_SYSCOMMAND` with `SC_RESTORE` /
//! `SC_MAXIMIZE` / `SC_MINIMIZE` / `SC_CLOSE` for the buttons.
//! That way we hit the *actual* OS path and inherit Windows-native
//! snap layouts, double-click-to-toggle, and cursor handling.

#![cfg(target_os = "windows")]

use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetWindowPlacement, GetWindowRect, HTCAPTION, IsIconic, IsZoomed, PostMessageW,
    SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE, SW_SHOWNORMAL, SetWindowPlacement,
    WINDOWPLACEMENT, WM_NCLBUTTONDOWN, WM_SYSCOMMAND,
};
use windows::core::HSTRING;

/// Best-effort HWND extraction. Returns `None` when the window is
/// in a state where the platform handle isn't available — caller
/// silently no-ops in that case.
fn hwnd(window: &Window) -> Option<HWND> {
    let handle = HasWindowHandle::window_handle(window).ok()?;
    match handle.as_raw() {
        RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut _)),
        _ => None,
    }
}

/// Start a window drag from a press on the title bar. Mirrors the
/// `ReleaseCapture` + `WM_NCLBUTTONDOWN(HTCAPTION)` pattern
/// Microsoft documents for custom-titlebar apps, but uses
/// `PostMessageW` instead of `SendMessageW`.
///
/// **Why post, not send.** `SendMessageW(WM_NCLBUTTONDOWN, HTCAPTION)`
/// from the same thread that owns the window is *synchronous*: it
/// calls our `WndProc` directly, which `DefWindowProc` handles by
/// entering a modal move loop. Inside that loop Windows pumps
/// queued messages — including gpui's own foreground tasks (toast
/// timer, dirty poll, settings reload, terminal snapshot drains).
/// Those tasks call `app.borrow_mut()`, but the App is **already
/// borrowed** by the dispatch chain that's currently running our
/// `on_mouse_down` listener. Result: `RefCell already borrowed`
/// panic on every titlebar drag.
///
/// `PostMessageW` queues the message and returns immediately. The
/// modal move loop kicks in on the next outer message pump
/// (after the dispatch chain unwinds and the App borrow is
/// released), so gpui events fired *during* the drag find a clean
/// borrow state.
pub fn start_drag(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        // Releasing capture is fine to call synchronously — it's a
        // simple state flip with no nested message pumping.
        let _ = ReleaseCapture();

        // Maximized → restore-then-drag dance.
        //
        // Default Windows behaviour: drag the title bar of a
        // maximized window and the OS auto-restores it under the
        // cursor so you can keep dragging. That hand-off lives in
        // `DefWindowProc`'s `WM_NCLBUTTONDOWN(HTCAPTION)` handler and
        // only fires when the message is delivered SYNCHRONOUSLY via
        // `SendMessage`. We have to `PostMessage` (see the doc on
        // this fn for the re-entrance reason), so the modal move
        // loop starts on a maximized window that never restores —
        // the user sees their cursor "stuck" and no drag happens.
        //
        // Fix: when we detect the window is maximized, repoint the
        // restore rect under the cursor (preserving horizontal ratio)
        // and post `SC_RESTORE` before the `NCLBUTTONDOWN`. Both posts
        // are queued ahead of the modal loop entry, so by the time
        // the loop starts, the window is the right size and the
        // cursor is on the title bar.
        if IsZoomed(hwnd).as_bool() {
            reposition_for_restore_under_cursor(hwnd);
            let _ = PostMessageW(
                Some(hwnd),
                WM_SYSCOMMAND,
                WPARAM(SC_RESTORE as usize),
                LPARAM(0),
            );
        }

        let _ = PostMessageW(
            Some(hwnd),
            WM_NCLBUTTONDOWN,
            WPARAM(HTCAPTION as usize),
            LPARAM(0),
        );
    }
}

/// Helper for `start_drag`: when the user grabs the title bar of a
/// maximized window, update `WINDOWPLACEMENT.rcNormalPosition` so the
/// restored window lands under the cursor instead of at its
/// previously-saved windowed position. Preserves the cursor's
/// horizontal ratio across the maximize → restore transition the
/// way native Windows does, so a drag from the right edge of a
/// maximized window keeps the cursor near the right edge of the
/// restored window (not at the centre and not off the title bar).
///
/// SAFETY: caller holds the HWND for the lifetime of this call.
/// All Win32 calls take pointers to locals we own. Errors are
/// best-effort — a failure leaves the placement struct untouched and
/// the subsequent `SC_RESTORE` falls back to the OS's saved
/// rcNormalPosition, which is the previous (slightly worse) behaviour
/// but still leaves the window draggable.
unsafe fn reposition_for_restore_under_cursor(hwnd: HWND) {
    unsafe {
        let mut cursor = POINT::default();
        if GetCursorPos(&mut cursor).is_err() {
            return;
        }

        let mut current = RECT::default();
        if GetWindowRect(hwnd, &mut current).is_err() {
            return;
        }
        let maximized_width = (current.right - current.left).max(1);
        let cursor_ratio_x =
            ((cursor.x - current.left) as f64 / maximized_width as f64).clamp(0.0, 1.0);

        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        if GetWindowPlacement(hwnd, &mut placement).is_err() {
            return;
        }

        let normal = placement.rcNormalPosition;
        let restored_width = (normal.right - normal.left).max(1);
        let restored_height = (normal.bottom - normal.top).max(1);

        // Centre the cursor horizontally at the same ratio of the
        // restored width; place the title bar so the cursor is a
        // few px into it (matches the native hand-off offset).
        //
        // DPI-scale the offset: the app manifest is PerMonitorV2, so
        // window coordinates are physical pixels at the monitor's DPI.
        // A hard-coded 15 lands ~7.5 logical px on a 200 % display,
        // which puts the cursor in/above the chrome border instead of
        // on the title bar. `GetDpiForWindow` reports 96 at 100 %,
        // 192 at 200 %, etc.; we scale the 15 px design value by
        // `dpi/96` so the offset stays roughly 15 logical px on every
        // monitor. Falls back to 96 (1.0 scale) on the rare 0 return.
        let dpi = GetDpiForWindow(hwnd);
        let dpi_scale = if dpi == 0 { 1.0 } else { dpi as f64 / 96.0 };
        let title_offset = (15.0 * dpi_scale) as i32;
        let new_left = cursor.x - (cursor_ratio_x * restored_width as f64) as i32;
        let new_top = cursor.y - title_offset;
        placement.rcNormalPosition = RECT {
            left: new_left,
            top: new_top,
            right: new_left + restored_width,
            bottom: new_top + restored_height,
        };
        let _ = SetWindowPlacement(hwnd, &placement);
    }
}

/// True while the window is minimized (`SW_SHOWMINIMIZED`).
///
/// gpui has no equivalent accessor: a minimized window reports
/// `WindowBounds::Windowed(<restore bounds>)` and `is_maximized() ==
/// false`, which is indistinguishable from a user restore-down. The
/// window-state persister needs the difference — see the caller in
/// `app.rs` (issue #279).
///
/// Returns `false` when the HWND can't be resolved; the caller then
/// behaves exactly as it did before this guard existed.
pub fn is_minimized(window: &Window) -> bool {
    let Some(hwnd) = hwnd(window) else {
        return false;
    };
    unsafe { IsIconic(hwnd).as_bool() }
}

/// Toggle maximize ↔ restore via `WM_SYSCOMMAND`. `IsZoomed` is the
/// canonical "is this window maximized?" check; we post
/// `SC_RESTORE` when it is and `SC_MAXIMIZE` otherwise. Posts so
/// the message is processed *after* our dispatch chain unwinds —
/// see `start_drag` for the full reason.
pub fn toggle_maximize(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        let cmd = if IsZoomed(hwnd).as_bool() {
            SC_RESTORE
        } else {
            SC_MAXIMIZE
        };
        let _ = PostMessageW(
            Some(hwnd),
            WM_SYSCOMMAND,
            WPARAM(cmd as usize),
            LPARAM(0),
        );
    }
}

/// Post `SC_MINIMIZE`. Equivalent to `Window::minimize_window` but
/// goes through the same `WM_SYSCOMMAND` channel as the rest of the
/// caption controls so taskbar / animation behavior is consistent.
pub fn minimize(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        let _ = PostMessageW(
            Some(hwnd),
            WM_SYSCOMMAND,
            WPARAM(SC_MINIMIZE as usize),
            LPARAM(0),
        );
    }
}

/// One-line snapshot of every Win32 piece of state that determines
/// where a maximised window ends up — emitted from the bounds-change
/// observer + caption-button click handlers so we can grep
/// `window-diag.log` for the moment the geometry went bad.
///
/// Returns `None` only when the HWND can't be resolved (window has
/// already been dropped); every other field is best-effort and falls
/// back to a `?` placeholder if the underlying Win32 call fails so the
/// log line still parses cleanly.
///
/// Format is intentionally one line, key=value pairs separated by
/// spaces, so it can be piped through `grep`/`awk` and copied into a
/// PR/issue body without losing structure.
pub fn diag_snapshot(window: &Window) -> Option<String> {
    let hwnd = hwnd(window)?;
    unsafe {
        let mut rect = RECT::default();
        let win_rect = GetWindowRect(hwnd, &mut rect)
            .map(|_| format!("({},{},{},{})", rect.left, rect.top, rect.right, rect.bottom))
            .unwrap_or_else(|_| "?".to_string());

        let zoomed = IsZoomed(hwnd).as_bool();

        let mut placement = WINDOWPLACEMENT {
            length: std::mem::size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        let placement_str = if GetWindowPlacement(hwnd, &mut placement).is_ok() {
            let n = placement.rcNormalPosition;
            format!(
                "showCmd={} rcNormal=({},{},{},{}) ptMaxPos=({},{}) flags={:#x}",
                placement.showCmd,
                n.left, n.top, n.right, n.bottom,
                placement.ptMaxPosition.x, placement.ptMaxPosition.y,
                placement.flags.0,
            )
        } else {
            "showCmd=? rcNormal=? ptMaxPos=? flags=?".to_string()
        };

        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let monitor_str = if GetMonitorInfoW(monitor, &mut mi).as_bool() {
            let m = mi.rcMonitor;
            let w = mi.rcWork;
            format!(
                "rcMonitor=({},{},{},{}) rcWork=({},{},{},{})",
                m.left, m.top, m.right, m.bottom,
                w.left, w.top, w.right, w.bottom,
            )
        } else {
            "rcMonitor=? rcWork=?".to_string()
        };

        let dpi = GetDpiForWindow(hwnd);

        Some(format!(
            "hwnd_rect={win_rect} zoomed={zoomed} {placement_str} {monitor_str} dpi={dpi}"
        ))
    }
}

/// Post `SC_CLOSE`. Equivalent to `Window::remove_window`.
pub fn close(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        let _ = PostMessageW(
            Some(hwnd),
            WM_SYSCOMMAND,
            WPARAM(SC_CLOSE as usize),
            LPARAM(0),
        );
    }
}

/// Open a URL via `ShellExecuteW` ("open" verb). This is the
/// shell-injection-safe path: `cmd /C start <url>` would let
/// metacharacters in the URL (`&`, `|`, …) be interpreted as
/// command separators by `cmd.exe`. ShellExecuteW takes the URL
/// as an opaque wide string and routes it through the registered
/// protocol handler without going through a shell — same model
/// `start` would use under the hood, minus the cmd.exe parsing.
///
/// **Threaded dispatch.** ShellExecuteW is synchronous and — for
/// `http(s)://` URLs that resolve to an already-running browser —
/// resolves the handler via DDE / out-of-proc COM. That path pumps
/// messages on the calling thread while it waits, which would
/// re-enter gpui's `WndProc` and trip `RefCell already borrowed`
/// because we're calling from inside an `on_mouse_down` listener
/// (the App is already borrowed by the dispatch chain that ran us).
/// Symptom: clicking "Open PR in browser" / "Open remote in browser"
/// from a sidebar context menu crashes the app — same root cause as
/// the titlebar `SendMessageW` bug `start_drag` documents in detail.
///
/// Mitigation: spawn a one-shot detached OS thread that calls
/// `CoInitializeEx(APARTMENTTHREADED)` + `ShellExecuteW`. The UI
/// thread returns immediately, the dispatch chain unwinds, and the
/// inner message pump (if any) runs in a thread with no gpui state
/// to borrow. Empty URLs short-circuit so a stale-cache slip-through
/// doesn't fire ShellExecuteW with an empty argument.
pub fn shell_open_url(url: &str) {
    // Defensive: ShellExecuteW with an empty lpFile is documented as
    // erroring out, but it's still a synchronous call that can pump
    // messages before returning. Hide the round-trip entirely when we
    // have nothing to open — callers up the stack should already be
    // gating on this but a misgated row should fail silent, not hard.
    if url.is_empty() {
        return;
    }
    let url_owned = url.to_owned();
    std::thread::Builder::new()
        .name("shell-open-url".into())
        .spawn(move || {
            // SAFETY: this thread is single-purpose and exits after
            // the ShellExecuteW call. CoInitializeEx is required for
            // some shell protocol handlers (notably the browser DDE
            // path) to resolve; we pair it with CoUninitialize so the
            // apartment is torn down cleanly. Failure of either call
            // is non-fatal — `ShellExecuteW` still attempts the open
            // and Windows will fall back to the registry handler.
            unsafe {
                let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
                let url_wide = HSTRING::from(url_owned.as_str());
                let verb_wide = HSTRING::from("open");
                let _ = ShellExecuteW(
                    None,
                    &verb_wide,
                    &url_wide,
                    None,
                    None,
                    SW_SHOWNORMAL,
                );
                if hr.is_ok() {
                    CoUninitialize();
                }
            }
        })
        .ok();
}
