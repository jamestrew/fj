//! Social subcommands for `fj repo`: watch/unwatch, star/unstar, and listing
//! your starred repos. Thin command wrappers over `api::repo_social`.

use anyhow::Result;
use clap::Args;

use crate::api;
use crate::cli::context::resolve_repo;
use crate::cli::repo::pick_repo;
use crate::client::Client;
use crate::output;

#[derive(Debug, Args)]
pub struct SimpleRepoArgs {
    pub repo: Option<String>,
    #[arg(short = 'R', long = "repo")]
    pub repo_flag: Option<String>,
}

#[derive(Debug, Args)]
pub struct StarredArgs {
    #[arg(short = 'L', long, default_value_t = 30)]
    pub limit: u32,
    #[arg(long)]
    pub json: bool,
}

pub async fn watch(args: SimpleRepoArgs, host: Option<&str>, on: bool) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    if on {
        api::repo::watch(&ctx.client, &ctx.owner, &ctx.name).await?;
        println!("✓ Watching {}/{}", ctx.owner, ctx.name);
    } else {
        api::repo::unwatch(&ctx.client, &ctx.owner, &ctx.name).await?;
        println!("✓ Unwatched {}/{}", ctx.owner, ctx.name);
    }
    Ok(())
}

pub async fn star(args: SimpleRepoArgs, host: Option<&str>, on: bool) -> Result<()> {
    let ctx = resolve_repo(pick_repo(args.repo.as_ref(), args.repo_flag.as_ref()), host)?;
    if on {
        api::repo::star(&ctx.client, &ctx.owner, &ctx.name).await?;
        println!("★ Starred {}/{}", ctx.owner, ctx.name);
    } else {
        api::repo::unstar(&ctx.client, &ctx.owner, &ctx.name).await?;
        println!("☆ Unstarred {}/{}", ctx.owner, ctx.name);
    }
    Ok(())
}

pub async fn starred(args: StarredArgs, host: Option<&str>) -> Result<()> {
    let client = Client::connect(host)?;
    let repos = api::repo::list_starred(&client, args.limit).await?;
    if args.json {
        return output::print_json(&serde_json::to_value(&repos)?);
    }
    if repos.is_empty() {
        println!("(no starred repos)");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = repos
        .iter()
        .map(|r| {
            let vis = if r.private { "private" } else { "public" };
            vec![
                r.full_name.clone(),
                output::truncate(&r.description, 60),
                vis.into(),
            ]
        })
        .collect();
    print!(
        "{}",
        output::render_table(&["NAME", "DESCRIPTION", "VIS"], &rows)
    );
    Ok(())
}
