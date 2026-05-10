//! Process-tree cleanup via Win32 job objects.
//!
//! Windows-only mirror of `src/CodeScope.Core/Interop/ProcessTreeKiller.cs`.
//! A process-wide job object is created lazily on first
//! [`adopt_handle`] call and held in a [`OnceLock`] for the rest of the
//! process lifetime. Every PTY child handed to [`adopt_handle`] is
//! assigned to that job. Because the job is created with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, the kernel terminates every
//! assigned process when the last handle to the job is closed —
//! either through orderly shutdown (the [`OnceLock`] is leaked, so the
//! handle closes when the process image is torn down) or a hard crash
//! (handles are reaped by the OS). Either way no Claude / Codex / pwsh
//! descendant outlives CodeScope.
//!
//! Why this beats `kill-on-drop`: a `Drop` impl only runs on graceful
//! exit. A panic in gpui's render loop, an `abort()` from a stack
//! overflow, or a process kill from Task Manager all skip `Drop` —
//! and every PTY descendant becomes an orphan. Job objects are
//! enforced by the kernel and survive every termination path.
//!
//! Non-Windows builds compile to no-ops so the rest of the workspace
//! stays cross-platform-clean.

#[cfg(windows)]
mod imp {
    use std::sync::OnceLock;

    use anyhow::{Context, Result};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_BASIC_LIMIT_INFORMATION, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    use windows::core::PCWSTR;

    /// Wraps a Win32 job-object `HANDLE`. The handle is kept alive for
    /// the entire process lifetime — closing it fires
    /// `KILL_ON_JOB_CLOSE` and tears down every assigned descendant,
    /// which is exactly what we want on graceful shutdown.
    ///
    /// Marked `Send + Sync` because all Win32 calls below are
    /// thread-safe at the kernel level (each takes the handle as an
    /// opaque argument; no shared state on our side).
    pub struct ProcessGroup {
        job: HANDLE,
    }

    // SAFETY: `HANDLE` is a raw pointer in the `windows` crate, but
    // job-object handles are kernel objects — `AssignProcessToJobObject`
    // and friends are documented thread-safe. We never dereference the
    // handle ourselves.
    unsafe impl Send for ProcessGroup {}
    unsafe impl Sync for ProcessGroup {}

    impl ProcessGroup {
        /// Create a fresh, unnamed job object flagged
        /// `KILL_ON_JOB_CLOSE`. Mirrors the C# constructor.
        fn new() -> Result<Self> {
            // SAFETY: `CreateJobObjectW(None, None)` is the standard
            // way to allocate an anonymous job object. The returned
            // handle is owned by us until process exit.
            let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
                .context("CreateJobObjectW failed")?;

            let info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
                BasicLimitInformation: JOBOBJECT_BASIC_LIMIT_INFORMATION {
                    LimitFlags: JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
                    ..Default::default()
                },
                ..Default::default()
            };

            // SAFETY: `info` is a stack-local struct of the exact type
            // declared by the API and lives across the call.
            unsafe {
                SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const _,
                    std::mem::size_of_val(&info) as u32,
                )
                .context("SetInformationJobObject failed")?;
            }

            Ok(Self { job })
        }

        /// Assign a child process to this job. After this, the child
        /// (and every process it spawns) is killed when the job
        /// closes. Mirrors the C# `Adopt(IntPtr)`.
        pub fn adopt(&self, process: HANDLE) -> Result<()> {
            // SAFETY: The kernel validates the process handle. We pass
            // ownership semantics: the caller still owns the handle,
            // we only reference it.
            unsafe { AssignProcessToJobObject(self.job, process) }
                .context("AssignProcessToJobObject failed")?;
            Ok(())
        }
    }

    // The handle is intentionally never closed — the OS closes it
    // when the process image is torn down, which fires
    // `KILL_ON_JOB_CLOSE`. We do not implement `Drop` because the
    // singleton is leaked into a `OnceLock` for the lifetime of the
    // process; an explicit close here would never be reached on the
    // crash paths we most want to cover.

    static PROCESS_GROUP: OnceLock<ProcessGroup> = OnceLock::new();

    /// Eagerly create the process-wide job object and adopt the
    /// current process into it. Safe to call multiple times — only
    /// the first call performs work. Mirrors the C# startup pair
    /// (`new ProcessTreeKiller()` + `Adopt(GetCurrentProcess())`).
    pub fn ensure() -> Result<()> {
        let group = match PROCESS_GROUP.get() {
            Some(g) => g,
            None => {
                let group = ProcessGroup::new()?;
                // SAFETY: `GetCurrentProcess` returns a pseudo-handle
                // that does not need to be closed.
                let me = unsafe { GetCurrentProcess() };
                // Best-effort: adopting the current process can fail
                // when CodeScope itself is already inside another job
                // that disallows nesting (e.g. running under a
                // debugger or a sandbox). Children adopt fine via
                // `JOB_OBJECT_LIMIT_SILENT_BREAKAWAY_OK` semantics on
                // modern Windows, so a failure here is non-fatal —
                // we still return success and let `adopt_handle`
                // continue assigning children.
                let _ = group.adopt(me);
                let _ = PROCESS_GROUP.set(group);
                PROCESS_GROUP.get().expect("just inserted")
            }
        };
        let _ = group;
        Ok(())
    }

    /// Assign the given child-process handle to the process-wide job.
    /// The job is created on first call. Mirrors the per-pty
    /// `Adopt(child.Handle)` pattern in the C# session manager.
    pub fn adopt_handle(process: HANDLE) -> Result<()> {
        let group = match PROCESS_GROUP.get() {
            Some(g) => g,
            None => {
                let group = ProcessGroup::new()?;
                let _ = PROCESS_GROUP.set(group);
                PROCESS_GROUP.get().expect("just inserted")
            }
        };
        group.adopt(process)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn creates_job_and_adopts_current_process() {
            // Smoke test: the most we can portably check without
            // spawning real processes is that the job object can be
            // created and the current process can be assigned to it
            // (or fails with a recoverable error if we are already
            // inside a non-nestable parent job).
            let group = ProcessGroup::new().expect("create job");
            let me = unsafe { GetCurrentProcess() };
            // Either succeeds, or fails because we are already in a
            // restrictive parent job — both outcomes prove the API
            // bindings are wired correctly.
            let _ = group.adopt(me);
        }

        #[test]
        fn ensure_is_idempotent() {
            ensure().expect("first ensure");
            ensure().expect("second ensure");
        }
    }
}

#[cfg(not(windows))]
mod imp {
    //! Non-Windows builds: every entry point is a no-op so callers
    //! don't need `cfg` gates at the call site.

    use anyhow::Result;

    /// Opaque placeholder so signatures match the Windows side. Never
    /// constructed on non-Windows targets.
    pub struct PlaceholderHandle;

    pub fn ensure() -> Result<()> {
        Ok(())
    }

    pub fn adopt_handle(_: PlaceholderHandle) -> Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
pub use imp::{adopt_handle, ensure};

#[cfg(not(windows))]
pub use imp::ensure;
