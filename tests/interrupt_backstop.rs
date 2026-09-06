//! End-to-end cover for the SIGINT backstop, against the real binary.
//!
//! `arm()` and `force_exit()` cannot be exercised in-process: the interesting
//! behaviour of one is a signal handler and of the other is a process that
//! dies. Both are reachable from outside, the way `tests/version.rs` already
//! reaches the built binary, and that is the level where the backstop's whole
//! claim lives.

use std::io::Write;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const FJ: &str = env!("CARGO_BIN_EXE_fj");

/// A child with its own config dir and no inherited fj credentials.
///
/// The environment is trimmed rather than cleared: `cargo llvm-cov` passes
/// `LLVM_PROFILE_FILE` down to spawned binaries, and clearing it would drop
/// this test's coverage of the very functions it exists to reach.
fn spawn(cfg: &Path, args: &[&str], stdin: Stdio) -> Child {
    Command::new(FJ)
        .args(args)
        .env("XDG_CONFIG_HOME", cfg)
        .env_remove("FJ_TOKEN")
        .env_remove("FJ_SESSION")
        .env_remove("BW_SESSION")
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn fj")
}

fn sigint(child: &Child) {
    // Safety: the pid is this process's own child and has not been reaped.
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
}

fn wait_for(child: &mut Child, limit: Duration) -> Option<std::process::ExitStatus> {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("try_wait") {
            return Some(status);
        }
        if start.elapsed() > limit {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn write_store(dir: &Path) {
    std::fs::create_dir_all(dir).expect("config dir");
    std::fs::write(
        dir.join("tokens.toml"),
        "[tokens]\n\"a.example\" = \"aaaaaaaaaaaaaaaa\"\n\"b.example\" = \"bbbbbbbbbbbbbbbb\"\n",
    )
    .expect("tokens.toml");
    std::fs::write(
        dir.join("hosts.toml"),
        "[hosts.\"a.example\"]\ngit_protocol = \"https\"\n\n\
         [hosts.\"b.example\"]\ngit_protocol = \"https\"\n",
    )
    .expect("hosts.toml");
}

/// The bug this PR fixes: a command parked in a synchronous read cannot be
/// cancelled by the graceful select, so without the backstop Ctrl+C leaves the
/// process running. `--with-token` reads stdin, and a pipe nobody writes to
/// parks it exactly there.
///
/// Asserting on the exit is what catches a missing backstop; awaiting the
/// process looks the same either way until it does not come back.
#[test]
fn sigint_force_exits_a_command_parked_in_a_synchronous_read() {
    let cfg = tempfile::tempdir().expect("tempdir");
    write_store(&cfg.path().join("fj"));
    let mut child = spawn(
        cfg.path(),
        &["auth", "login", "--with-token", "--host", "a.example"],
        Stdio::piped(),
    );
    // Let it reach the read and arm the handler.
    std::thread::sleep(Duration::from_millis(300));
    sigint(&child);

    let status = wait_for(&mut child, Duration::from_secs(10));
    let status = match status {
        Some(status) => status,
        None => {
            let _ = child.kill();
            panic!(
                "fj was still running 10s after SIGINT while parked in a synchronous \
                 read; the interrupt backstop did not fire"
            );
        }
    };
    assert!(
        !status.success(),
        "an interrupted sign-in must not report success: {status:?}"
    );
}

/// The data-safety half. Whatever the backstop does to the process, it must not
/// be able to leave the token store empty: `fj auth logout` rewrites it, and an
/// exit landing inside a truncate-then-write would take every host with it.
///
/// This covers the invariant end to end. It is not the deterministic regression
/// pin for the atomic write, because the real window is microseconds wide and
/// this cannot reliably land in it; that pin is the in-process test in
/// `src/auth/mod.rs`, which observes the replacement directly.
#[test]
fn sigint_during_a_logout_never_leaves_the_token_store_empty() {
    for delay_ms in [0u64, 1, 2, 3, 5, 8, 12, 20, 30, 50] {
        let cfg = tempfile::tempdir().expect("tempdir");
        let dir = cfg.path().join("fj");
        write_store(&dir);
        let tokens = dir.join("tokens.toml");

        let mut child = spawn(
            cfg.path(),
            &["auth", "logout", "--host", "a.example"],
            Stdio::null(),
        );
        std::thread::sleep(Duration::from_millis(delay_ms));
        sigint(&child);
        if wait_for(&mut child, Duration::from_secs(10)).is_none() {
            let _ = child.kill();
            panic!("fj still running 10s after SIGINT during logout (delay {delay_ms}ms)");
        }

        let after = std::fs::read_to_string(&tokens).expect("token store must still exist");
        assert!(
            !after.trim().is_empty(),
            "token store was left empty by a SIGINT {delay_ms}ms into logout"
        );
        assert!(
            after.contains("b.example"),
            "a SIGINT {delay_ms}ms into logging out of a.example destroyed the \
             unrelated b.example token: {after:?}"
        );
    }
}

/// The backstop must not slow down commands that are not interrupted.
#[test]
fn arming_the_backstop_does_not_delay_an_ordinary_command() {
    let cfg = tempfile::tempdir().expect("tempdir");
    write_store(&cfg.path().join("fj"));
    let start = Instant::now();
    let mut child = spawn(cfg.path(), &["auth", "status"], Stdio::null());
    let status = wait_for(&mut child, Duration::from_secs(20));
    assert!(status.is_some(), "fj auth status did not finish");
    assert!(
        start.elapsed() < Duration::from_secs(15),
        "fj auth status took {:?} with the backstop armed",
        start.elapsed()
    );
    let _ = std::io::stderr().flush();
}

/// The other half of the backstop: while a child owns the terminal, the
/// backstop must stand down and let that child decide what SIGINT means.
///
/// `fj auth setup-git` spawns `git config --global` inside a
/// `ForegroundChild::enter()` guard, which is the same one-line wrapper used at
/// the `$EDITOR`, browser and `git` call sites. Driving it here covers the
/// guard's `enter`/`drop` pair and `foreground_child_active` in the real
/// process rather than through an injected closure, and it does so offline:
/// `--global` writes to the scratch `HOME` this test hands it.
#[test]
fn a_guarded_child_spawn_completes_and_writes_only_inside_the_scratch_home() {
    let home = tempfile::tempdir().expect("tempdir");
    let cfg = tempfile::tempdir().expect("tempdir");
    write_store(&cfg.path().join("fj"));

    let out = Command::new(FJ)
        .args(["auth", "setup-git", "--host", "a.example"])
        .env("XDG_CONFIG_HOME", cfg.path())
        .env("HOME", home.path())
        .env_remove("FJ_TOKEN")
        .env_remove("FJ_SESSION")
        .env_remove("BW_SESSION")
        .stdin(Stdio::null())
        .output()
        .expect("run fj auth setup-git");

    assert!(
        out.status.success(),
        "setup-git failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gitconfig = std::fs::read_to_string(home.path().join(".gitconfig"))
        .expect("git config --global must have written into the scratch HOME");
    assert!(
        gitconfig.contains("a.example"),
        "the credential helper must be installed for the host asked for: {gitconfig:?}"
    );
}

/// `--dry-run` must not spawn the child or touch git config at all. Pairs with
/// the test above: together they show the guarded spawn is reached in one case
/// and genuinely skipped in the other, rather than the guard being decorative.
#[test]
fn setup_git_dry_run_spawns_nothing_and_writes_nothing() {
    let home = tempfile::tempdir().expect("tempdir");
    let cfg = tempfile::tempdir().expect("tempdir");
    write_store(&cfg.path().join("fj"));

    let out = Command::new(FJ)
        .args(["auth", "setup-git", "--host", "a.example", "--dry-run"])
        .env("XDG_CONFIG_HOME", cfg.path())
        .env("HOME", home.path())
        .env_remove("FJ_TOKEN")
        .env_remove("FJ_SESSION")
        .env_remove("BW_SESSION")
        .stdin(Stdio::null())
        .output()
        .expect("run fj auth setup-git --dry-run");

    assert!(
        out.status.success(),
        "setup-git --dry-run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !home.path().join(".gitconfig").exists(),
        "--dry-run must not write git config"
    );
}
