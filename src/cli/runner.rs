//! `fj runner`: Forgejo Actions runners.

use anyhow::Result;
use clap::{Args, Subcommand};

use crate::api::runner::{registration_token, Scope};
use crate::cli::context::{resolve_repo, RepoFlag};
use crate::client::Client;

#[derive(Debug, Args)]
pub struct RunnerCmd {
    #[command(subcommand)]
    pub command: RunnerSub,
}

#[derive(Debug, Subcommand)]
pub enum RunnerSub {
    /// Print a runner registration token, and nothing else, for
    /// `forgejo-runner register --token "$(fj runner token --org ORG)"`.
    /// Scope is the repository (`-R`, or the current clone) unless `--org`,
    /// `--user` or `--instance` says otherwise. The token lets a machine join
    /// as a runner, so treat it as a credential.
    Token(TokenArgs),
}

#[derive(Debug, Args)]
pub struct TokenArgs {
    #[command(flatten)]
    pub r: RepoFlag,
    /// Organization scope: a runner shared by the organization's repos.
    #[arg(long, value_name = "ORG", conflicts_with_all = ["repo", "user", "instance"])]
    pub org: Option<String>,
    /// Your own account's scope: a runner for your personal repos.
    #[arg(long, conflicts_with_all = ["repo", "instance"])]
    pub user: bool,
    /// Instance scope: a runner for every repo. Needs an instance administrator.
    #[arg(long, conflicts_with = "repo")]
    pub instance: bool,
}

#[cfg_attr(coverage_nightly, coverage(off))]
pub async fn run(cmd: RunnerCmd, host: Option<&str>) -> Result<()> {
    match cmd.command {
        RunnerSub::Token(args) => token(args, host).await,
    }
}

#[cfg_attr(coverage_nightly, coverage(off))]
async fn token(args: TokenArgs, host: Option<&str>) -> Result<()> {
    let (client, scope) = if let Some(org) = args.org {
        (Client::connect(host)?, Scope::Org(org))
    } else if args.user {
        (Client::connect(host)?, Scope::User)
    } else if args.instance {
        (Client::connect(host)?, Scope::Instance)
    } else {
        let ctx = resolve_repo(args.r.repo.as_deref(), host)?;
        let scope = Scope::Repo {
            owner: ctx.owner,
            name: ctx.name,
        };
        (ctx.client, scope)
    };
    println!("{}", registration_token(&client, &scope).await?);
    Ok(())
}
