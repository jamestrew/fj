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

/// Resolve a repo from `-R` (`owner/name`) or auto-detect from the git remote.
/// Honors the global `--host` flag for host selection.
pub fn resolve_repo(
    explicit_repo: Option<&str>,
    explicit_host: Option<&str>,
) -> Result<RepoContext> {
    // `-R` wins; otherwise honor a per-repo default pinned by
    // `fj repo set-default` (stored in local git config), then fall back to
    // git-remote detection. The stored default behaves exactly like `-R`.
    let explicit_repo: Option<String> = explicit_repo
        .map(str::to_string)
        .or_else(|| git::config_get_local("fj.resolved"));
    let (host_for_client, owner, name) = match explicit_repo.as_deref() {
        Some(slug) => {
            let (o, n) = api::split_repo(slug)?;
            (
                explicit_host.map(str::to_string),
                o.to_string(),
                n.to_string(),
            )
        }
        None => {
            let detected = git::discover(explicit_host)
                .context("repository not specified and could not be inferred from git remote")?;
            // If the user passed --host, keep that as the API host even though
            // the remote URL also happens to name a host. They should match in
            // practice, but the explicit flag wins.
            (
                Some(explicit_host.unwrap_or(&detected.host).to_string()),
                detected.owner,
                detected.name,
            )
        }
    };

    // If the user didn't pass --host AND we didn't autodetect, fall back to
    // the configured default host.
    let client_host = host_for_client.or_else(|| {
        Hosts::load()
            .ok()
            .and_then(|h| h.resolve_host(None).ok().map(|s| s.to_string()))
    });

    let client = Client::connect(client_host.as_deref())?;
    let host = client_host.unwrap_or_else(|| client.host().to_string());

    Ok(RepoContext {
        client,
        host,
        owner,
        name,
    })
}
