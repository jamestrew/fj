//! Cross-process single-flight for the Fjord Account OIDC refresh.
//!
//! Paragon rotates the refresh token on every exchange and detects replay, so
//! two `fj` processes that refresh concurrently do not merely race: the loser's
//! exchange reads as a replay and the OP revokes the whole token family,
//! killing the winner's freshly minted token along with it. Both sessions die
//! while `hosts.toml` and the keychain slots still look healthy (fj#272).
//!
//! An advisory file lock, held across the entire exchange-and-persist, makes
//! the refresh single-flight across processes. The lock is per platform URL:
//! two platforms own disjoint token families and never need to wait on each
//! other.

use std::fs::{File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};

use crate::config;

/// How long to wait for a peer's refresh before giving up. A refresh is one
/// HTTP round trip, so a wait this long means the holder is wedged rather than
/// busy.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(30);

/// Gap between `try_lock` attempts while waiting for a peer.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// A held refresh lock. Releasing happens on drop.
#[derive(Debug)]
pub struct RefreshLock {
    file: File,
}

/// File name holding the refresh lock for `platform_url`, sanitised so a URL
/// can never escape the config directory or collide with another platform's
/// lock.
pub fn lock_file_name(platform_url: &str) -> String {
    // Every byte outside `[A-Za-z0-9]` collapses to `-`, so a platform URL can
    // contribute neither a path separator nor a `..` hop. Two URLs that differ
    // only in punctuation therefore share a lock file; that costs them a brief
    // wait on each other and never costs correctness.
    let slug: String = platform_url
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("fjord-refresh-{slug}.lock")
}

/// Acquire the refresh lock for `platform_url` in fj's config directory,
/// waiting up to [`ACQUIRE_TIMEOUT`] for a peer to finish.
pub fn acquire(platform_url: &str) -> Result<RefreshLock> {
    acquire_in(&config::config_dir()?, platform_url, ACQUIRE_TIMEOUT)
}

/// [`acquire`] against an explicit directory and timeout, so tests drive it
/// without touching the real config directory or waiting 30 seconds.
pub(crate) fn acquire_in(dir: &Path, platform_url: &str, timeout: Duration) -> Result<RefreshLock> {
    let path = lock_path(dir, platform_url)?;
    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("opening the refresh lock at {}", path.display()))?;

    let deadline = Instant::now() + timeout;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(RefreshLock { file }),
            Err(std::fs::TryLockError::WouldBlock) => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "timed out after {timeout:?} waiting for another fj process to finish \
                         refreshing the Fjord Account session for {platform_url}. If no other fj \
                         is running, remove {} and retry.",
                        path.display()
                    ));
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(std::fs::TryLockError::Error(err)) => {
                return Err(err)
                    .with_context(|| format!("locking the refresh lock at {}", path.display()));
            }
        }
    }
}

/// Resolve (and create the parent of) the lock file for `platform_url`.
fn lock_path(dir: &Path, platform_url: &str) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating the config directory at {}", dir.display()))?;
    Ok(dir.join(lock_file_name(platform_url)))
}

impl Drop for RefreshLock {
    fn drop(&mut self) {
        // Closing the descriptor would release the lock anyway; unlocking
        // explicitly keeps the release visible at the point it happens.
        let _ = self.file.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_holder_of_the_same_platform_lock_is_refused_while_the_first_holds_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _held = acquire_in(dir.path(), "https://fjord.sh", Duration::from_millis(50))
            .expect("first acquire");

        let contended = acquire_in(dir.path(), "https://fjord.sh", Duration::from_millis(50));

        assert!(
            contended.is_err(),
            "a concurrent refresh must not proceed while a peer holds the lock"
        );
    }

    #[test]
    fn releasing_the_lock_lets_the_next_holder_in() {
        let dir = tempfile::tempdir().expect("tempdir");
        let held = acquire_in(dir.path(), "https://fjord.sh", Duration::from_millis(50))
            .expect("first acquire");
        drop(held);

        acquire_in(dir.path(), "https://fjord.sh", Duration::from_millis(50))
            .expect("the lock must be free once the first holder drops it");
    }

    #[test]
    fn distinct_platform_urls_hold_disjoint_locks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _one = acquire_in(dir.path(), "https://fjord.sh", Duration::from_millis(50))
            .expect("first platform");

        acquire_in(
            dir.path(),
            "https://staging.fjord.sh",
            Duration::from_millis(50),
        )
        .expect("a different platform owns a different token family and must not contend");
    }

    #[test]
    fn lock_file_name_cannot_escape_the_config_directory() {
        let name = lock_file_name("https://fjord.sh/../../etc/passwd");

        assert!(
            !name.contains('/'),
            "{name} must not contain a path separator"
        );
        assert!(
            !name.contains(".."),
            "{name} must not contain a parent-directory hop"
        );
    }

    #[test]
    fn lock_file_names_stay_distinct_per_platform() {
        assert_ne!(
            lock_file_name("https://fjord.sh"),
            lock_file_name("https://staging.fjord.sh")
        );
    }
}
