//! Pager redirection. When stdout is a TTY (and the user hasn't opted out),
//! `maybe_start()` spawns `$FJ_PAGER` / `$PAGER` / `less -FRX` and redirects
//! our stdout into its stdin. The returned guard waits on the child on drop,
//! so all output gets flushed before the process exits.
//!
//! Implemented for Unix via `libc::dup2`. The Windows build provides a stub
//! that returns `None` from `maybe_start`, so `println!` writes directly to
//! the terminal as usual. (Implementing the Windows version would mean
//! duplicating the stdout HANDLE via `SetStdHandle`; not done yet.)

#[cfg(unix)]
mod imp {
    use std::io::IsTerminal;
    use std::os::fd::IntoRawFd;
    use std::os::unix::io::AsRawFd;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicI32, Ordering};

    /// The live guard's saved stdout fd and pager pid, mirrored here so the
    /// SIGINT backstop in `crate::interrupt` can put the terminal back when it
    /// force-exits without the guard's `Drop` ever running. `-1` is "none".
    ///
    /// Whichever of `abandon` and `Drop` gets there first swaps these out, so
    /// the restore happens exactly once.
    static LIVE_SAVED_STDOUT: AtomicI32 = AtomicI32::new(-1);
    static LIVE_PAGER_PID: AtomicI32 = AtomicI32::new(-1);

    /// Restore stdout and stop the pager, for the interrupt path only. SIGTERM
    /// rather than SIGKILL: `less` restores the terminal modes it changed only
    /// if it is allowed to shut down normally.
    pub fn abandon() {
        let saved = LIVE_SAVED_STDOUT.swap(-1, Ordering::SeqCst);
        let pid = LIVE_PAGER_PID.swap(-1, Ordering::SeqCst);
        if saved >= 0 {
            unsafe {
                libc::dup2(saved, libc::STDOUT_FILENO);
                libc::close(saved);
            }
        }
        if pid > 0 {
            unsafe { libc::kill(pid, libc::SIGTERM) };
        }
    }

    pub struct PagerGuard {
        /// Saved copy of the original stdout fd so we can restore it before
        /// waiting on the child (otherwise the child would block reading from
        /// a stdin pipe we still hold open).
        saved_stdout: Option<i32>,
        child: Option<Child>,
    }

    impl Drop for PagerGuard {
        fn drop(&mut self) {
            use std::io::Write;
            let _ = std::io::stdout().flush();

            // Take the restore duty away from the interrupt backstop: from here
            // on this Drop owns it.
            LIVE_SAVED_STDOUT.store(-1, Ordering::SeqCst);
            LIVE_PAGER_PID.store(-1, Ordering::SeqCst);

            if let Some(saved) = self.saved_stdout.take() {
                unsafe {
                    libc::dup2(saved, libc::STDOUT_FILENO);
                    libc::close(saved);
                }
            }
            if let Some(mut c) = self.child.take() {
                // The command is done writing and the user is scrolling now, so
                // Ctrl+C belongs to the pager. `less` absorbs it deliberately;
                // the backstop must not turn that into an exit.
                let _foreground = crate::interrupt::ForegroundChild::enter();
                let _ = c.wait();
            }
        }
    }

    pub fn maybe_start(force_disabled: bool) -> Option<PagerGuard> {
        if force_disabled || std::env::var_os("FJ_NO_PAGER").is_some() {
            return None;
        }
        if !std::io::stdout().is_terminal() {
            return None;
        }
        let pager_cmd = std::env::var("FJ_PAGER")
            .ok()
            .or_else(|| std::env::var("PAGER").ok())
            .unwrap_or_else(|| "less -FRX".into());
        let pager_cmd = pager_cmd.trim().to_string();
        if pager_cmd.is_empty() || pager_cmd == "cat" {
            return None;
        }
        let mut parts = pager_cmd.split_whitespace();
        let program = parts.next()?;
        let args: Vec<&str> = parts.collect();

        let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
        if saved < 0 {
            return None;
        }

        let mut child = match Command::new(program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => {
                unsafe { libc::close(saved) };
                return None;
            }
        };

        let Some(stdin) = child.stdin.take() else {
            unsafe { libc::close(saved) };
            let _ = child.kill();
            let _ = child.wait();
            return None;
        };
        let pipe_fd = stdin.as_raw_fd();
        let result = unsafe { libc::dup2(pipe_fd, libc::STDOUT_FILENO) };
        if result < 0 {
            unsafe { libc::close(saved) };
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        let _ = stdin.into_raw_fd();

        LIVE_SAVED_STDOUT.store(saved, Ordering::SeqCst);
        LIVE_PAGER_PID.store(child.id() as i32, Ordering::SeqCst);

        Some(PagerGuard {
            saved_stdout: Some(saved),
            child: Some(child),
        })
    }
}

#[cfg(not(unix))]
mod imp {
    /// Stub guard for non-Unix platforms. Holds no state.
    pub struct PagerGuard;

    /// Paging is not implemented on this platform; always returns `None` so
    /// stdout stays connected to the terminal directly. `--no-pager` and
    /// `FJ_NO_PAGER` are accepted (and respected by being a no-op) for
    /// compatibility with shared scripts.
    pub fn maybe_start(_force_disabled: bool) -> Option<PagerGuard> {
        None
    }

    /// No pager to abandon on this platform.
    pub fn abandon() {}
}

pub use imp::abandon;
pub use imp::maybe_start;
#[allow(unused_imports)]
pub use imp::PagerGuard;
