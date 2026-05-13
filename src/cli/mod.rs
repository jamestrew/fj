pub mod alias;
pub mod api;
pub mod auth;
pub mod browse;
pub mod config;
pub mod context;
pub mod editor;
pub mod extension;
pub mod gist;
pub mod issue;
pub mod key;
pub mod label;
pub mod org;
pub mod pr;
pub mod release;
pub mod repo;
pub mod search;
pub mod status;
pub mod web;
pub mod workflow;

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
    /// Work with releases.
    Release(release::ReleaseCmd),
    /// Work with labels.
    Label(label::LabelCmd),
    /// Manage workflow runs (Forgejo Actions).
    Run(workflow::RunCmd),
    /// Manage Actions secrets.
    Secret(workflow::SecretCmd),
    /// Manage Actions variables.
    Variable(workflow::VariableCmd),
    /// Cross-cutting search (repos, issues, prs, users).
    Search(search::SearchCmd),
    /// Open the current repo (or a path within it) in your browser.
    Browse(browse::BrowseArgs),
    /// Notifications inbox.
    Status(status::StatusArgs),
    /// Organization helpers.
    Org(org::OrgCmd),
    /// Manage SSH keys on your account.
    SshKey(key::SshKeyCmd),
    /// Manage GPG keys on your account.
    GpgKey(key::GpgKeyCmd),
    /// User-defined command aliases.
    Alias(alias::AliasCmd),
    /// Manage local fj configuration (editor, pager, browser, etc.).
    Config(config::ConfigCmd),
    /// Run a discovered `fj-<name>` plugin from PATH.
    Extension(extension::ExtensionCmd),
    /// Forgejo gists.
    Gist(gist::GistCmd),
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
    // Alias resolution: if the first positional looks like a user-defined
    // alias and the user typed `fj <alias>`, expand it before dispatching.
    // clap already parsed; aliases are handled inside the dedicated `alias`
    // subcommand or by re-execing ourselves. See cli::alias::dispatch.
    match cli.command {
        Command::Auth(cmd) => auth::run(cmd).await,
        Command::Repo(cmd) => repo::run(cmd, cli.host.as_deref()).await,
        Command::Issue(cmd) => issue::run(cmd, cli.host.as_deref()).await,
        Command::Pr(cmd) => pr::run(cmd, cli.host.as_deref()).await,
        Command::Release(cmd) => release::run(cmd, cli.host.as_deref()).await,
        Command::Label(cmd) => label::run(cmd, cli.host.as_deref()).await,
        Command::Run(cmd) => workflow::run(cmd, cli.host.as_deref()).await,
        Command::Secret(cmd) => workflow::run_secret(cmd, cli.host.as_deref()).await,
        Command::Variable(cmd) => workflow::run_variable(cmd, cli.host.as_deref()).await,
        Command::Search(cmd) => search::run(cmd, cli.host.as_deref()).await,
        Command::Browse(args) => browse::run(args, cli.host.as_deref()).await,
        Command::Status(args) => status::run(args, cli.host.as_deref()).await,
        Command::Org(cmd) => org::run(cmd, cli.host.as_deref()).await,
        Command::SshKey(cmd) => key::run_ssh(cmd, cli.host.as_deref()).await,
        Command::GpgKey(cmd) => key::run_gpg(cmd, cli.host.as_deref()).await,
        Command::Alias(cmd) => alias::run(cmd).await,
        Command::Config(cmd) => config::run(cmd).await,
        Command::Extension(cmd) => extension::run(cmd).await,
        Command::Gist(cmd) => gist::run(cmd, cli.host.as_deref()).await,
        Command::Api(args) => api::run(args, cli.host.as_deref()).await,
        Command::Completion(args) => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(args.shell, &mut cmd, "fj", &mut std::io::stdout());
            Ok(())
        }
    }
}
