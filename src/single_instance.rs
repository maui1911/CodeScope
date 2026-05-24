//! Single-instance enforcement — one running CodeScope per user.
//!
//! Mirrors the C# `App.OnStartup` named-mutex guard (retired build,
//! `legacy/v0.2.6-final` `src/CodeScope.App/App.xaml.cs`): create a
//! named mutex at boot; if it already exists another instance owns it,
//! so we inform the user and exit cleanly without standing up a second
//! window. See issue #247.
//!
//! The mutex name comes from [`AppPaths::single_instance_mutex`], which
//! already encodes the `CODESCOPE_DEV=1` split
//! (`Global\CodeScope.SingleInstance` vs `…SingleInstance.Dev`) so a
//! dev build coexists with the installed app rather than blocking it.
//!
//! Scope: this is the Windows regression called out in #247. On other
//! platforms [`acquire`] is a no-op that always reports `First`, so
//! Linux/macOS launches are unaffected until a cross-platform guard is
//! designed.

/// RAII holder for the single-instance lock. Hold it for the process
/// lifetime (bind it in `main`); dropping it closes the OS handle. The
/// lock also evaporates when the process exits — the kernel closes
/// every handle on termination — so an early `return` from `main`
/// without an explicit drop is equally safe.
pub struct SingleInstance {
    #[cfg(target_os = "windows")]
    handle: windows::Win32::Foundation::HANDLE,
}

/// Outcome of trying to claim the single-instance lock.
pub enum Acquire {
    /// We are the first instance; keep the [`SingleInstance`] alive for
    /// the process lifetime.
    First(SingleInstance),
    /// Another instance already owns the lock — caller should inform
    /// the user (see [`notify_already_running`]) and exit.
    AlreadyRunning,
}

#[cfg(target_os = "windows")]
pub fn acquire(mutex_name: &str) -> Acquire {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::core::HSTRING;

    let name = HSTRING::from(mutex_name);
    // SAFETY: `name` outlives the call. `CreateMutexW` either returns a
    // handle we own or an error; we never dereference a raw pointer.
    let handle = unsafe { CreateMutexW(None, true, &name) };
    match handle {
        Ok(h) => {
            // `GetLastError` is only meaningful immediately after the
            // call: `CreateMutexW` sets it to `ERROR_ALREADY_EXISTS`
            // (and returns a handle to the *existing* mutex) when one
            // is already open under this name.
            // SAFETY: querying thread-local last-error; no pointers.
            let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
            if already {
                // We hold a second handle to the existing mutex; close
                // it so we don't leak, then report the running instance.
                // SAFETY: `h` is a valid handle returned just above.
                unsafe {
                    let _ = CloseHandle(h);
                }
                Acquire::AlreadyRunning
            } else {
                Acquire::First(SingleInstance { handle: h })
            }
        }
        // Couldn't create the mutex at all (e.g. a transient OS error).
        // Fail open: let the launch proceed rather than lock the user
        // out of their own app. Worst case is the pre-#247 behaviour
        // (a possible second instance), never a startup that refuses to
        // run. An invalid handle makes the `Drop` close a no-op.
        Err(_) => Acquire::First(SingleInstance {
            handle: HANDLE::default(),
        }),
    }
}

/// Tell the user an instance is already running. Matches the C# build's
/// information dialog; window activation is intentionally left to the
/// user (the C# build did the same), so this is a plain modal.
#[cfg(target_os = "windows")]
pub fn notify_already_running() {
    use windows::Win32::UI::WindowsAndMessaging::{
        MB_ICONINFORMATION, MB_OK, MB_SETFOREGROUND, MessageBoxW,
    };
    use windows::core::HSTRING;

    let title = HSTRING::from("CodeScope is already running");
    let body = HSTRING::from(
        "Another instance of CodeScope is already running.\n\n\
         Switch to the existing window instead.",
    );
    // SAFETY: both strings outlive the call; `None` owner = top-level
    // message box. The return value (which button) is irrelevant for a
    // single-button OK dialog.
    unsafe {
        let _ = MessageBoxW(
            None,
            &body,
            &title,
            MB_OK | MB_ICONINFORMATION | MB_SETFOREGROUND,
        );
    }
}

#[cfg(target_os = "windows")]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        use windows::Win32::Foundation::CloseHandle;
        if !self.handle.is_invalid() {
            // SAFETY: `handle` came from `CreateMutexW` and is closed
            // exactly once (here), on process shutdown.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

#[cfg(not(target_os = "windows"))]
pub fn acquire(_mutex_name: &str) -> Acquire {
    // No cross-platform single-instance guard yet — #247 scopes the
    // regression to Windows. Always report `First` so dev launches on
    // Linux/macOS are unaffected.
    Acquire::First(SingleInstance {})
}

#[cfg(not(target_os = "windows"))]
pub fn notify_already_running() {}
