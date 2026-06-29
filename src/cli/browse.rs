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
    /// Print the resolved URL instead of opening a browser.
    #[arg(long, alias = "no-browser")]
    pub print_url: bool,
}

pub async fn run(args: BrowseArgs, host: Option<&str>) -> Result<()> {
    let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
    let repo = api::repo::get(&ctx.client, &ctx.owner, &ctx.name).await?;
    let url = browse_url(
        &repo.html_url,
        repo.default_branch.as_str(),
        args.path.as_deref(),
        args.branch.as_deref(),
    );
    handle_url(&url, args.print_url, web::open)
}

fn browse_url(
    repo_url: &str,
    default_branch: &str,
    path: Option<&str>,
    branch: Option<&str>,
) -> String {
    match path {
        None | Some("") => repo_url.to_string(),
        Some("issues") | Some("pulls") | Some("releases") | Some("wiki") | Some("settings") => {
            format!("{repo_url}/{}", path.unwrap())
        }
        Some(rest) => {
            let branch = branch.unwrap_or(default_branch);
            format!("{repo_url}/src/branch/{branch}/{rest}")
        }
    }
}

fn handle_url(url: &str, print_url: bool, opener: impl FnOnce(&str) -> Result<()>) -> Result<()> {
    if print_url {
        println!("{url}");
        return Ok(());
    }
    opener(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn browse_print_url_mode_does_not_invoke_opener() {
        let called = Cell::new(false);
        handle_url("https://forge.example/o/r", true, |_| {
            called.set(true);
            Ok(())
        })
        .unwrap();
        assert!(!called.get());
    }

    #[test]
    fn browse_no_browser_alias_sets_print_url() {
        use clap::Parser;
        let cli = crate::cli::Cli::try_parse_from(["fj", "browse", "--no-browser"])
            .expect("--no-browser should parse");
        let crate::cli::Command::Browse(args) = cli.command else {
            panic!("expected browse command");
        };
        assert!(args.print_url);
    }

    #[test]
    fn browse_url_uses_default_branch_for_paths() {
        assert_eq!(
            browse_url(
                "https://forge.example/o/r",
                "develop",
                Some("src/lib.rs"),
                None
            ),
            "https://forge.example/o/r/src/branch/develop/src/lib.rs"
        );
    }
}
