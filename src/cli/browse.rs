//! `fj browse [path...]` — open the current repo (or a path within it) on the web.

use anyhow::Result;
use clap::Args;

use crate::api;
use crate::cli::context::{resolve_repo, RepoFlag};

use super::web;

#[derive(Debug, Args)]
pub struct BrowseArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Optional sub-path (e.g. `src/main.rs`, `issues`, `pulls/3`).
    pub path: Option<String>,
    /// Branch name for source-tree paths.
    #[arg(long)]
    pub branch: Option<String>,
}

pub async fn run(args: BrowseArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let repo = api::repo::get(&ctx.client, &ctx.owner, &ctx.name).await?;
    let url = match args.path.as_deref() {
        None | Some("") => repo.html_url.clone(),
        Some("issues") | Some("pulls") | Some("releases") | Some("wiki") | Some("settings") => {
            format!("{}/{}", repo.html_url, args.path.unwrap())
        }
        Some(rest) => {
            let branch = args.branch.as_deref().unwrap_or(&repo.default_branch);
            format!("{}/src/branch/{}/{}", repo.html_url, branch, rest)
        }
    };
    web::open(&url)
}
