pub mod api;
pub mod auth;
pub mod context;
pub mod editor;
pub mod issue;
pub mod pr;
pub mod repo;
pub mod web;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// `fj` — command-line tool for Forgejo.
#[derive(Debug, Parser)]
#[command(
    name = "fj",
    version,
    about = "Command-line tool for Forgejo",
    long_about = None,
    propagate_version = true,
    arg_required_else_help = true,
)]
pub struct Cli {
    /// Override the host for this command (otherwise the current host from
    /// `fj auth login` is used).
    #[arg(long, global = true, env = "FJ_HOST")]
    pub host: Option<String>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Manage authentication against Forgejo hosts.
    Auth(auth::AuthCmd),
    /// Work with repositories.
    Repo(repo::RepoCmd),
    /// Work with issues.
    Issue(issue::IssueCmd),
    /// Work with pull requests.
    Pr(pr::PrCmd),
    /// Make a raw API request (like `gh api`).
    Api(api::ApiArgs),
    /// Print shell completions to stdout.
    Completion(CompletionArgs),
}

#[derive(Debug, clap::Args)]
pub struct CompletionArgs {
    /// Shell to generate completions for.
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

pub async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Auth(cmd) => auth::run(cmd).await,
        Command::Repo(cmd) => repo::run(cmd, cli.host.as_deref()).await,
        Command::Issue(cmd) => issue::run(cmd, cli.host.as_deref()).await,
        Command::Pr(cmd) => pr::run(cmd, cli.host.as_deref()).await,
        Command::Api(args) => api::run(args, cli.host.as_deref()).await,
        Command::Completion(args) => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(args.shell, &mut cmd, "fj", &mut std::io::stdout());
            Ok(())
        }
    }
}
