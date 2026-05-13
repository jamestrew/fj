pub mod alias;
pub mod api;
pub mod auth;
pub mod browse;
pub mod config;
pub mod context;
pub mod editor;
pub mod extension;
pub mod gist;
pub mod hook;
pub mod issue;
pub mod key;
pub mod label;
pub mod milestone;
pub mod org;
pub mod pr;
pub mod protect;
pub mod release;
pub mod repo;
pub mod search;
pub mod status;
pub mod web;
pub mod workflow;

use anyhow::{Context, Result};
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

    /// Log every HTTP request fj makes to stderr.
    #[arg(long, global = true, env = "FJ_DEBUG")]
    pub debug: bool,

    /// Skip piping long output through `$FJ_PAGER` / `$PAGER` (default: less).
    #[arg(long, global = true)]
    pub no_pager: bool,

    /// Comma-separated list of fields to keep in `--json` output (gh-style
    /// projection). Supports dotted paths like `owner.login`.
    #[arg(long, global = true, value_name = "FIELDS")]
    pub json_fields: Option<String>,

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
    /// Work with milestones.
    Milestone(milestone::MilestoneCmd),
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
    /// Manage branch protection rules.
    Protect(protect::ProtectCmd),
    /// Manage repository webhooks.
    Hook(hook::HookCmd),
    /// Run a discovered `fj-<name>` plugin from PATH.
    Extension(extension::ExtensionCmd),
    /// Forgejo gists.
    Gist(gist::GistCmd),
    /// Make a raw API request (like `gh api`).
    Api(api::ApiArgs),
    /// Print shell completions to stdout.
    Completion(CompletionArgs),
    /// Generate man pages for `fj` and all subcommands into a directory.
    Man(ManArgs),
}

#[derive(Debug, clap::Args)]
pub struct ManArgs {
    /// Output directory. Defaults to `./man`.
    #[arg(short = 'o', long, default_value = "./man")]
    pub output_dir: std::path::PathBuf,
}

#[derive(Debug, clap::Args)]
pub struct CompletionArgs {
    /// Shell to generate completions for.
    #[arg(value_enum)]
    pub shell: clap_complete::Shell,
}

pub async fn run(cli: Cli) -> Result<()> {
    crate::client::set_debug(cli.debug);
    crate::output::set_json_fields(cli.json_fields.as_deref());
    // Page commands whose output can run long. Pass `cli.no_pager` through
    // explicitly so we don't have to mutate the process environment to opt
    // out, which would be a process-wide side effect and an unsafe set_var.
    let _pager = match &cli.command {
        Command::Repo(_)
        | Command::Issue(_)
        | Command::Pr(_)
        | Command::Release(_)
        | Command::Search(_)
        | Command::Status(_)
        | Command::Label(_)
        | Command::Milestone(_)
        | Command::Run(_)
        | Command::Api(_) => crate::output::pager::maybe_start(cli.no_pager),
        _ => None,
    };

    // Run the command, racing it against Ctrl+C. On SIGINT we return an error
    // and let `_pager` drop naturally — its Drop impl restores stdout and
    // waits on the pager child, leaving the terminal in a clean state.
    let cli_host = cli.host.clone();
    let cmd_future = dispatch(cli.command, cli_host.as_deref());
    tokio::select! {
        biased;
        sig = tokio::signal::ctrl_c() => {
            sig.context("installing SIGINT handler")?;
            Err(anyhow::anyhow!("interrupted"))
        }
        res = cmd_future => res,
    }
}

async fn dispatch(command: Command, host: Option<&str>) -> Result<()> {
    match command {
        Command::Auth(cmd) => auth::run(cmd).await,
        Command::Repo(cmd) => repo::run(cmd, host).await,
        Command::Issue(cmd) => issue::run(cmd, host).await,
        Command::Pr(cmd) => pr::run(cmd, host).await,
        Command::Release(cmd) => release::run(cmd, host).await,
        Command::Label(cmd) => label::run(cmd, host).await,
        Command::Milestone(cmd) => milestone::run(cmd, host).await,
        Command::Run(cmd) => workflow::run(cmd, host).await,
        Command::Secret(cmd) => workflow::run_secret(cmd, host).await,
        Command::Variable(cmd) => workflow::run_variable(cmd, host).await,
        Command::Search(cmd) => search::run(cmd, host).await,
        Command::Browse(args) => browse::run(args, host).await,
        Command::Status(args) => status::run(args, host).await,
        Command::Org(cmd) => org::run(cmd, host).await,
        Command::SshKey(cmd) => key::run_ssh(cmd, host).await,
        Command::GpgKey(cmd) => key::run_gpg(cmd, host).await,
        Command::Alias(cmd) => alias::run(cmd).await,
        Command::Config(cmd) => config::run(cmd).await,
        Command::Protect(cmd) => protect::run(cmd, host).await,
        Command::Hook(cmd) => hook::run(cmd, host).await,
        Command::Extension(cmd) => extension::run(cmd).await,
        Command::Gist(cmd) => gist::run(cmd, host).await,
        Command::Api(args) => api::run(args, host).await,
        Command::Completion(args) => {
            use clap::CommandFactory;
            let mut cmd = Cli::command();
            clap_complete::generate(args.shell, &mut cmd, "fj", &mut std::io::stdout());
            Ok(())
        }
        Command::Man(args) => generate_man_pages(args).await,
    }
}

async fn generate_man_pages(args: ManArgs) -> Result<()> {
    use clap::CommandFactory;
    let dir = &args.output_dir;
    std::fs::create_dir_all(dir)?;
    let cmd = Cli::command();
    write_man_for(&cmd, "fj", dir)?;
    for sub in cmd.get_subcommands() {
        let name = format!("fj-{}", sub.get_name());
        write_man_for(sub, &name, dir)?;
    }
    println!("✓ Wrote man pages to {}", dir.display());
    Ok(())
}

fn write_man_for(cmd: &clap::Command, file_name: &str, dir: &std::path::Path) -> Result<()> {
    let path = dir.join(format!("{file_name}.1"));
    let mut out = std::fs::File::create(&path)?;
    let man = clap_mangen::Man::new(cmd.clone());
    man.render(&mut out)?;
    Ok(())
}
