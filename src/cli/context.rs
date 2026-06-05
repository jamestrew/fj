//! Shared helpers for resolving the target repo + host across subcommands.
//!
//! The contract: each subcommand accepts `-R <owner>/<name>` plus the global
//! `--host`. When `-R` is omitted, we infer the repo from the current
//! directory's git remotes, preferring `upstream` then `origin`.

use anyhow::{Context, Result};
use clap::Args;

use crate::api;
use crate::client::Client;
use crate::config::hosts::Hosts;
use crate::git;

/// Flattened `-R/--repo` flag used by every repo-scoped subcommand. When
/// omitted, the repo is inferred from the current directory's git remotes.
#[derive(Debug, Args, Clone, Default)]
pub struct RepoFlag {
    /// Target repository as `<owner>/<name>`. Inferred from the git remote
    /// when omitted.
    #[arg(short = 'R', long = "repo", global = true)]
    pub repo: Option<String>,
}

/// A repo target resolved from either `-R` or git remotes, plus a connected
/// API client for the correct host.
pub struct RepoContext {
    pub client: Client,
    pub host: String,
    pub owner: String,
    pub name: String,
}

impl RepoContext {
    #[allow(dead_code)]
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

/// The repo slug to target, by precedence: an explicit `-R` flag wins, then a
/// per-repo default pinned by `fj repo set-default` (local git config), else
/// `None` so the caller falls back to git-remote detection. Pure so the
/// precedence is unit-testable without touching git or the network.
fn resolved_repo_slug(explicit: Option<&str>, pinned: Option<String>) -> Option<String> {
    explicit.map(str::to_string).or(pinned)
}

/// The API host to connect to, by precedence: an explicit `--host` always
/// wins; otherwise the host named by the autodetected remote (`detected_host`,
/// `None` when the repo came from a slug rather than detection); otherwise the
/// configured default. Pure for the same reason as [`resolved_repo_slug`].
fn resolve_client_host(
    explicit_host: Option<&str>,
    detected_host: Option<&str>,
    default_host: Option<String>,
) -> Option<String> {
    explicit_host
        .map(str::to_string)
        .or_else(|| detected_host.map(str::to_string))
        .or(default_host)
}

/// Resolve a repo from `-R` (`owner/name`) or auto-detect from the git remote.
/// Honors the global `--host` flag for host selection.
///
/// Coverage is off here (like the async command handlers): it's I/O-coupled
/// (git remote discovery, config load, keychain/network via `Client::connect`)
/// and exercised by the live `FJ_E2E` smoke, not unit tests. The pure
/// precedence it delegates to (`resolved_repo_slug`, `resolve_client_host`) is
/// unit-tested below.
#[cfg_attr(coverage_nightly, coverage(off))]
pub fn resolve_repo(
    explicit_repo: Option<&str>,
    explicit_host: Option<&str>,
) -> Result<RepoContext> {
    // `-R` wins; otherwise honor a per-repo default pinned by
    // `fj repo set-default` (stored in local git config), then fall back to
    // git-remote detection. The stored default behaves exactly like `-R`.
    let explicit_repo = resolved_repo_slug(explicit_repo, git::config_get_local("fj.resolved"));
    let (detected_host, owner, name) = match explicit_repo.as_deref() {
        Some(slug) => {
            let (o, n) = api::split_repo(slug)?;
            (None, o.to_string(), n.to_string())
        }
        None => {
            let detected = git::discover(explicit_host)
                .context("repository not specified and could not be inferred from git remote")?;
            // If the user passed --host, keep that as the API host even though
            // the remote URL also happens to name a host. They should match in
            // practice, but the explicit flag wins (handled in
            // `resolve_client_host`).
            (Some(detected.host), detected.owner, detected.name)
        }
    };

    // If the user didn't pass --host AND we didn't autodetect, fall back to
    // the configured default host.
    let default_host = || {
        Hosts::load()
            .ok()
            .and_then(|h| h.resolve_host(None).ok().map(|s| s.to_string()))
    };
    let client_host = resolve_client_host(explicit_host, detected_host.as_deref(), default_host());

    let client = Client::connect(client_host.as_deref())?;
    let host = client_host.unwrap_or_else(|| client.host().to_string());

    Ok(RepoContext {
        client,
        host,
        owner,
        name,
    })
}

#[cfg(test)]
mod tests {
    use super::{resolve_client_host, resolved_repo_slug};

    #[test]
    fn slug_precedence_flag_beats_pinned_beats_none() {
        // explicit `-R` wins over a pinned default.
        assert_eq!(
            resolved_repo_slug(Some("o/n"), Some("p/d".into())).as_deref(),
            Some("o/n")
        );
        // no flag → pinned default is used.
        assert_eq!(
            resolved_repo_slug(None, Some("p/d".into())).as_deref(),
            Some("p/d")
        );
        // neither → None, so the caller falls back to git detection.
        assert_eq!(resolved_repo_slug(None, None), None);
    }

    #[test]
    fn host_precedence_explicit_beats_detected_beats_default() {
        // --host always wins, even over a detected remote host.
        assert_eq!(
            resolve_client_host(Some("h.example"), Some("d.example"), Some("def".into()))
                .as_deref(),
            Some("h.example")
        );
        // no --host: the detected remote host wins over the configured default.
        assert_eq!(
            resolve_client_host(None, Some("d.example"), Some("def".into())).as_deref(),
            Some("d.example")
        );
        // slug path (no detection) and no --host: fall back to the default.
        assert_eq!(
            resolve_client_host(None, None, Some("def".into())).as_deref(),
            Some("def")
        );
        // nothing configured anywhere → None (Client::connect picks current host).
        assert_eq!(resolve_client_host(None, None, None), None);
    }
}
