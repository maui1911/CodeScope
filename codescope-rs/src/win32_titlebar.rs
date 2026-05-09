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
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::ReleaseCapture;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    HTCAPTION, IsZoomed, SC_CLOSE, SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE, SW_SHOWNORMAL,
    SendMessageW, WM_NCLBUTTONDOWN, WM_SYSCOMMAND,
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
/// `ReleaseCapture` + `SendMessage(WM_NCLBUTTONDOWN, HTCAPTION, …)`
/// pattern Microsoft documents for custom-titlebar apps.
pub fn start_drag(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        // Required: tell the OS we're handing off mouse capture so
        // the subsequent `WM_NCLBUTTONDOWN` actually starts a move
        // loop instead of just being delivered.
        let _ = ReleaseCapture();
        SendMessageW(
            hwnd,
            WM_NCLBUTTONDOWN,
            Some(WPARAM(HTCAPTION as usize)),
            Some(LPARAM(0)),
        );
    }
}

/// Toggle maximize ↔ restore. `IsZoomed` is the canonical Win32
/// "is this window maximized?" check; we send `SC_RESTORE` when it
/// is and `SC_MAXIMIZE` otherwise. Posts as `WM_SYSCOMMAND` so the
/// OS owns the animation + snap-layout integration instead of us
/// poking `ShowWindow` directly.
pub fn toggle_maximize(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        let cmd = if IsZoomed(hwnd).as_bool() {
            SC_RESTORE
        } else {
            SC_MAXIMIZE
        };
        SendMessageW(
            hwnd,
            WM_SYSCOMMAND,
            Some(WPARAM(cmd as usize)),
            Some(LPARAM(0)),
        );
    }
}

/// Send a `SC_MINIMIZE`. Equivalent to `Window::minimize_window` but
/// goes through the same `WM_SYSCOMMAND` channel as the rest of the
/// caption controls so taskbar / animation behavior is consistent.
pub fn minimize(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        SendMessageW(
            hwnd,
            WM_SYSCOMMAND,
            Some(WPARAM(SC_MINIMIZE as usize)),
            Some(LPARAM(0)),
        );
    }
}

/// Send a `SC_CLOSE`. Equivalent to `Window::remove_window`.
pub fn close(window: &Window) {
    let Some(hwnd) = hwnd(window) else { return };
    unsafe {
        SendMessageW(
            hwnd,
            WM_SYSCOMMAND,
            Some(WPARAM(SC_CLOSE as usize)),
            Some(LPARAM(0)),
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
pub fn shell_open_url(url: &str) {
    let url_wide = HSTRING::from(url);
    let verb_wide = HSTRING::from("open");
    unsafe {
        let _ = ShellExecuteW(
            None,
            &verb_wide,
            &url_wide,
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}
