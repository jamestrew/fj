//! Pager redirection. When stdout is a TTY (and the user hasn't opted out),
//! `maybe_start()` spawns `$FJ_PAGER` / `$PAGER` / `less -FRX` and dup2's our
//! stdout to its stdin. The returned guard waits on the child on drop, so all
//! output gets flushed before the process exits.
//!
//! The dup2 strategy means the rest of fj keeps using plain `println!` /
//! `print!` without knowing it's piped. `less -FRX` exits immediately if the
//! content fits one screen, so short output is unaffected.

use std::io::IsTerminal;
use std::os::fd::IntoRawFd;
use std::os::unix::io::AsRawFd;
use std::process::{Child, Command, Stdio};

pub struct PagerGuard {
    /// Saved copy of the original stdout fd so we can restore it before
    /// waiting on the child (otherwise the child would block reading from a
    /// stdin pipe we still hold open).
    saved_stdout: Option<i32>,
    child: Option<Child>,
}

impl Drop for PagerGuard {
    fn drop(&mut self) {
        // Make sure anything buffered in stdout reaches the pager before we
        // close our write end.
        use std::io::Write;
        let _ = std::io::stdout().flush();

        if let Some(saved) = self.saved_stdout.take() {
            unsafe {
                libc::dup2(saved, libc::STDOUT_FILENO);
                libc::close(saved);
            }
        }
        if let Some(mut c) = self.child.take() {
            let _ = c.wait();
        }
    }
}

/// If conditions are right, redirect this process's stdout to a pager. Returns
/// `None` if paging is disabled, stdout isn't a TTY, or spawning the pager
/// failed.
///
/// The returned guard must outlive all `println!`/`print!` calls produced by
/// the current command.
pub fn maybe_start() -> Option<PagerGuard> {
    if std::env::var_os("FJ_NO_PAGER").is_some() {
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

    // Save the current stdout so the guard can restore it.
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

    // Take the child's stdin pipe and dup it onto our stdout.
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
    // We dup'd the pipe fd onto stdout. Drop the ChildStdin handle without
    // closing its underlying fd (it's now stdout's fd, owned by the process).
    let _ = stdin.into_raw_fd();

    Some(PagerGuard {
        saved_stdout: Some(saved),
        child: Some(child),
    })
}
