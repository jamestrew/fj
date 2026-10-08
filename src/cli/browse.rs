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
    #[arg(long, conflicts_with = "tag")]
    pub branch: Option<String>,
    /// Tag name: open the tree (or PATH) as it was at that tag. A tag is a
    /// fixed snapshot, so the page it opens is read-only.
    #[arg(long)]
    pub tag: Option<String>,
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
        tree_ref(args.branch.as_deref(), args.tag.as_deref()),
    );
    handle_url(&url, args.print_url, web::open)
}

/// Which kind of ref a source-tree URL points at. Forgejo routes the two
/// under different prefixes (`src/branch/..` and `src/tag/..`), and only a
/// branch page offers edits, so they are never folded together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TreeRef<'a> {
    Default,
    Branch(&'a str),
    Tag(&'a str),
}

fn tree_ref<'a>(branch: Option<&'a str>, tag: Option<&'a str>) -> TreeRef<'a> {
    match (tag, branch) {
        (Some(t), _) => TreeRef::Tag(t),
        (None, Some(b)) => TreeRef::Branch(b),
        (None, None) => TreeRef::Default,
    }
}

fn browse_url(repo_url: &str, default_branch: &str, path: Option<&str>, at: TreeRef<'_>) -> String {
    let tree = match at {
        TreeRef::Tag(t) => format!("{repo_url}/src/tag/{t}"),
        TreeRef::Branch(b) => format!("{repo_url}/src/branch/{b}"),
        TreeRef::Default => format!("{repo_url}/src/branch/{default_branch}"),
    };
    match path {
        // A bare `--tag` or `--branch` opens that ref's root tree; with no
        // ref at all the repo home page is the better landing spot.
        None | Some("") => match at {
            TreeRef::Default => repo_url.to_string(),
            _ => tree,
        },
        Some("issues") | Some("pulls") | Some("releases") | Some("wiki") | Some("settings") => {
            format!("{repo_url}/{}", path.unwrap())
        }
        Some(rest) => format!("{tree}/{rest}"),
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
                TreeRef::Default
            ),
            "https://forge.example/o/r/src/branch/develop/src/lib.rs"
        );
    }

    #[test]
    fn browse_url_tag_uses_the_tag_route_not_the_branch_route() {
        let at = tree_ref(None, Some("v1.2.0"));
        assert_eq!(at, TreeRef::Tag("v1.2.0"));
        assert_eq!(
            browse_url("https://forge.example/o/r", "main", Some("src/lib.rs"), at),
            "https://forge.example/o/r/src/tag/v1.2.0/src/lib.rs"
        );
        // No path: the tag's root tree, not the repo home (which shows the
        // default branch).
        assert_eq!(
            browse_url("https://forge.example/o/r", "main", None, at),
            "https://forge.example/o/r/src/tag/v1.2.0"
        );
    }

    #[test]
    fn browse_url_branch_keeps_the_branch_route() {
        let at = tree_ref(Some("release"), None);
        assert_eq!(
            browse_url("https://forge.example/o/r", "main", Some("a.txt"), at),
            "https://forge.example/o/r/src/branch/release/a.txt"
        );
        assert_eq!(
            browse_url("https://forge.example/o/r", "main", None, at),
            "https://forge.example/o/r/src/branch/release"
        );
    }

    #[test]
    fn browse_url_without_a_ref_or_path_is_the_repo_home() {
        assert_eq!(
            browse_url("https://forge.example/o/r", "main", None, TreeRef::Default),
            "https://forge.example/o/r"
        );
        assert_eq!(
            browse_url(
                "https://forge.example/o/r",
                "main",
                Some("issues"),
                TreeRef::Tag("v1")
            ),
            "https://forge.example/o/r/issues"
        );
    }

    #[test]
    fn browse_tag_and_branch_are_mutually_exclusive() {
        use clap::Parser;
        assert!(crate::cli::Cli::try_parse_from([
            "fj", "browse", "--tag", "v1", "--branch", "main"
        ])
        .is_err());
        let cli = crate::cli::Cli::try_parse_from(["fj", "browse", "--tag", "v1"]).unwrap();
        let crate::cli::Command::Browse(args) = cli.command else {
            panic!("expected browse command");
        };
        assert_eq!(args.tag.as_deref(), Some("v1"));
    }
}
