//! Making Ctrl+C actually stop `fj`.
//!
//! `cli::run` races the command future against `tokio::signal::ctrl_c()`. That
//! is the graceful path: the select drops the command, `run` returns
//! `Err("interrupted")`, and the pager guard's `Drop` puts the terminal back on
//! the way out.
//!
//! It only works while the command future is still pollable, and several code
//! paths park the `block_on` thread inside a synchronous syscall for an
//! unbounded time:
//!
//! * the OS keychain, reached from `Client::connect` by nearly every
//!   authenticated command. `SecKeychainFindGenericPassword` blocks forever
//!   whenever macOS wants to present an authorization dialog it cannot present:
//!   over ssh, in CI, from a git hook, or from an ad-hoc signed dev build.
//! * the synchronous stdin reads behind `--body -`, `--input -`,
//!   `--with-token`, and the interactive Fjord Account prompts.
//!
//! While the thread sits in `read(2)` the select never gets polled again, and
//! tokio has already replaced SIGINT's default disposition, so the signal no
//! longer terminates the process either. Ctrl+C becomes completely inert: not
//! slow, not delayed, simply ignored for as long as the syscall lasts.
//!
//! [`arm`] installs a backstop for that case. It is a spawned async task, so it
//! runs on a runtime worker rather than on the wedged `block_on` thread. It
//! waits for SIGINT, gives the graceful path [`GRACE`] to win, and if the
//! process is still alive after that, restores the terminal itself and exits.
//!
//! The one thing that must never be force-exited out from under the user is a
//! child process holding the terminal's foreground process group: an `$EDITOR`,
//! a `git clone` prompting for a passphrase, a pager being scrolled. Those
//! receive the same SIGINT directly and get to decide what it means (`less`
//! deliberately absorbs it). [`ForegroundChild`] marks those windows and the
//! backstop stands down while one is open.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// How long the graceful path gets to unwind before the backstop concludes it
/// is wedged. Generous on purpose: when unwinding is possible at all it only
/// needs the `block_on` task to be polled once, which is microseconds.
const GRACE: Duration = Duration::from_millis(250);

/// 128 + SIGINT, the shell convention for "terminated by Ctrl+C".
pub const EXIT_INTERRUPTED: i32 = 130;

/// The error `cli::run` returns when Ctrl+C wins the race against the command.
/// Typed so `main` can exit with [`EXIT_INTERRUPTED`] on the graceful path
/// too, the same status the backstop uses.
#[derive(Debug, thiserror::Error)]
#[error("interrupted")]
pub struct Interrupted;

/// Nesting depth of child processes that currently own the terminal.
static FOREGROUND_CHILDREN: AtomicUsize = AtomicUsize::new(0);

/// Marks a window during which a child process owns the terminal's foreground
/// process group, so SIGINT is that child's to interpret and the backstop in
/// [`arm`] must keep its hands off.
///
/// Hold one across any `Command::status()` that inherits our stdio, and across
/// the pager wait.
pub struct ForegroundChild(());

impl ForegroundChild {
    #[must_use]
    pub fn enter() -> Self {
        FOREGROUND_CHILDREN.fetch_add(1, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for ForegroundChild {
    fn drop(&mut self) {
        FOREGROUND_CHILDREN.fetch_sub(1, Ordering::SeqCst);
    }
}

fn foreground_child_active() -> bool {
    FOREGROUND_CHILDREN.load(Ordering::SeqCst) > 0
}

/// What the backstop decided to do about one SIGINT.
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    /// Somebody else owns this signal; leave the process alone.
    StandDown,
    /// The graceful path had its chance and did not take it.
    ForceExit,
}

/// The backstop's policy for a single SIGINT.
///
/// The foreground check is repeated after the grace window because the two can
/// interleave: the SIGINT that reached us may be the one that just killed a
/// child, and by the time the window closes the command may have moved on to
/// spawning another (an `$EDITOR` after a `git` call, say). Re-checking costs
/// nothing and turns that race into a stand-down rather than a kill.
///
/// `active` is injected so the policy can be exercised without touching the
/// process-global counter, which the test harness shares across threads.
async fn decide_with(active: impl Fn() -> bool) -> Decision {
    if active() {
        return Decision::StandDown;
    }
    tokio::time::sleep(GRACE).await;
    if active() {
        return Decision::StandDown;
    }
    Decision::ForceExit
}

/// Spawn the SIGINT backstop. Call once, early in `cli::run`, from inside the
/// runtime.
///
/// This is a plain async task, not `spawn_blocking`: dropping a tokio runtime
/// waits for blocking-pool tasks but not for async ones, so the backstop can
/// never itself become a reason the process fails to exit.
pub fn arm() {
    tokio::spawn(async {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                // No handler could be installed, so SIGINT still has its
                // default disposition and the kernel will terminate us without
                // any help. Nothing left to back up.
                return;
            }
            match decide_with(foreground_child_active).await {
                Decision::StandDown => continue,
                Decision::ForceExit => force_exit(),
            }
        }
    });
}

/// Tear down the terminal and leave. The graceful path never ran, so nothing
/// else is going to do this.
fn force_exit() -> ! {
    // Put stdout back on the terminal and stop the pager. SIGTERM rather than
    // SIGKILL: `less` restores the tty modes it changed only if it is allowed
    // to shut down normally.
    crate::output::pager::abandon();

    let mut err = std::io::stderr();
    if err.is_terminal() {
        // A `dialoguer` prompt may have hidden the cursor, and we are cutting
        // in mid-line either way. Undo both so the shell prompt lands somewhere
        // sane.
        let _ = err.write_all(b"\x1b[?25h\r\n");
    }
    let _ = writeln!(
        err,
        "{} interrupted",
        owo_colors::OwoColorize::red(&"error:")
    );
    let _ = err.flush();
    std::process::exit(EXIT_INTERRUPTED);
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::AtomicBool;

    /// Nesting matters: an inner child finishing must not hand the terminal
    /// back while an outer one still owns it. Guards are balanced, so this
    /// stays order-independent against the shared global under cargo's
    /// threaded harness.
    #[test]
    fn foreground_guard_nests_and_unwinds() {
        let base = FOREGROUND_CHILDREN.load(Ordering::SeqCst);
        {
            let _outer = ForegroundChild::enter();
            {
                let _inner = ForegroundChild::enter();
                assert_eq!(FOREGROUND_CHILDREN.load(Ordering::SeqCst), base + 2);
            }
            assert_eq!(FOREGROUND_CHILDREN.load(Ordering::SeqCst), base + 1);
            assert!(foreground_child_active());
        }
        assert_eq!(FOREGROUND_CHILDREN.load(Ordering::SeqCst), base);
    }

    #[tokio::test(start_paused = true)]
    async fn stands_down_immediately_while_a_child_owns_the_terminal() {
        assert_eq!(decide_with(|| true).await, Decision::StandDown);
    }

    #[tokio::test(start_paused = true)]
    async fn force_exits_when_nothing_else_claimed_the_signal() {
        assert_eq!(decide_with(|| false).await, Decision::ForceExit);
    }

    /// The window between "SIGINT arrived" and "grace expired" is exactly when
    /// a command is most likely to hand the terminal to a child, because the
    /// SIGINT it just took may be what ended the previous one. Entering the
    /// window with no child and leaving it with one must stand down.
    #[tokio::test(start_paused = true)]
    async fn stands_down_when_a_child_claims_the_terminal_during_the_grace() {
        static CLAIMED: AtomicBool = AtomicBool::new(false);
        let (decision, ()) = tokio::join!(decide_with(|| CLAIMED.load(Ordering::SeqCst)), async {
            tokio::time::sleep(GRACE / 2).await;
            CLAIMED.store(true, Ordering::SeqCst);
        });
        assert_eq!(decision, Decision::StandDown);
    }
}
